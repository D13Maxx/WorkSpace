//! The Gemini client and `ChatBackend` implementation (master spec s7.4).

use futures_util::stream::BoxStream;
use std::sync::Arc;
use tokio_util::sync::CancellationToken;
use ws_context::{
    BackendError, BackendEvent, BackendId, BackendRequest, ChatBackend, ModelCapabilities,
    ModelInfo, ModelProfiles, ModelRef,
};
use ws_secrets::SecretStore;

use super::decode::GeminiDecoder;
use super::encode::build_body;
use crate::http::{post_with_retry, summarise_error};
use crate::retry::RetryPolicy;
use crate::stream::event_stream;

pub const DEFAULT_BASE_URL: &str = "https://generativelanguage.googleapis.com";
const API_VERSION: &str = "v1beta";

#[derive(Debug, Clone)]
pub struct GeminiBackend {
    id: BackendId,
    http: reqwest::Client,
    base_url: String,
    secrets: SecretStore,
    profiles: Arc<dyn ModelProfiles>,
    retry: RetryPolicy,
}

impl GeminiBackend {
    #[must_use]
    pub fn new(id: BackendId, profiles: Arc<dyn ModelProfiles>) -> Self {
        Self {
            id,
            http: reqwest::Client::new(),
            base_url: DEFAULT_BASE_URL.to_owned(),
            secrets: SecretStore::new(),
            profiles,
            retry: RetryPolicy::default(),
        }
    }

    #[must_use]
    pub fn with_base_url(mut self, base_url: impl Into<String>) -> Self {
        self.base_url = base_url.into();
        self
    }

    #[must_use]
    pub fn with_retry(mut self, retry: RetryPolicy) -> Self {
        self.retry = retry;
        self
    }

    fn api_key(&self) -> Result<ws_secrets::Secret, BackendError> {
        self.secrets
            .get_key(self.id.as_str())
            .map_err(|e| BackendError::Transport {
                backend: self.id.clone(),
                message: e.to_string(),
            })?
            .ok_or_else(|| BackendError::MissingApiKey {
                backend: self.id.clone(),
            })
    }

    fn base(&self) -> &str {
        self.base_url.trim_end_matches('/')
    }
}

/// Model ids arrive as `models/gemini-x`; the bare id is what the rest of the
/// app stores, so strip the prefix on the way in and add it on the way out.
fn qualify(model_id: &str) -> String {
    if model_id.starts_with("models/") {
        model_id.to_owned()
    } else {
        format!("models/{model_id}")
    }
}

fn strip_prefix(model_id: &str) -> &str {
    model_id.strip_prefix("models/").unwrap_or(model_id)
}

#[async_trait::async_trait]
impl ChatBackend for GeminiBackend {
    fn id(&self) -> BackendId {
        self.id.clone()
    }

    async fn list_models(&self) -> Result<Vec<ModelInfo>, BackendError> {
        let key = self.api_key()?;
        let url = format!("{}/{API_VERSION}/models", self.base());
        let resp = self
            .http
            .get(url)
            .header("x-goog-api-key", key.expose())
            .send()
            .await
            .map_err(|e| BackendError::Transport {
                backend: self.id.clone(),
                message: e.to_string(),
            })?;

        if !resp.status().is_success() {
            let status = resp.status().as_u16();
            let body = resp.text().await.unwrap_or_default();
            return Err(BackendError::Http {
                backend: self.id.clone(),
                status,
                message: summarise_error(&body),
            });
        }

        let body: serde_json::Value = resp.json().await.map_err(|e| BackendError::Protocol {
            backend: self.id.clone(),
            message: e.to_string(),
        })?;
        Ok(parse_model_list(&self.id, &body))
    }

    fn capabilities(&self, model: &ModelRef) -> ModelCapabilities {
        self.profiles.capabilities(model)
    }

    async fn stream(
        &self,
        request: BackendRequest,
        cancel: CancellationToken,
    ) -> Result<BoxStream<'static, BackendEvent>, BackendError> {
        let key = self.api_key()?;
        let body = build_body(&request);

        // `alt=sse` is what makes this a Server-Sent Events stream rather than
        // a JSON array delivered in chunks.
        let url = format!(
            "{}/{API_VERSION}/{}:streamGenerateContent?alt=sse",
            self.base(),
            qualify(&request.model.model_id)
        );

        let resp = post_with_retry(
            &self.http,
            &self.id,
            &url,
            vec![("x-goog-api-key", key.expose().to_owned())],
            &body,
            self.retry,
            summarise_error,
        )
        .await?;

        let decoder = GeminiDecoder::new(self.id.clone(), request.model.model_id.clone());
        Ok(event_stream(resp, decoder, self.id.clone(), cancel))
    }
}

/// Model ids come from the provider, never a hardcoded list (spec s7.1).
///
/// Gemini's list endpoint *does* report token limits, so unlike the other
/// providers the capabilities can be filled in without the user profiling
/// every model by hand.
fn parse_model_list(backend: &BackendId, body: &serde_json::Value) -> Vec<ModelInfo> {
    body.get("models")
        .and_then(|d| d.as_array())
        .map(|items| {
            items
                .iter()
                .filter_map(|item| {
                    let name = item.get("name").and_then(|v| v.as_str())?;
                    let id = strip_prefix(name);
                    let display = item
                        .get("displayName")
                        .and_then(|v| v.as_str())
                        .unwrap_or(id);

                    let input = item
                        .get("inputTokenLimit")
                        .and_then(serde_json::Value::as_u64)
                        .and_then(|v| u32::try_from(v).ok());
                    let output = item
                        .get("outputTokenLimit")
                        .and_then(serde_json::Value::as_u64)
                        .and_then(|v| u32::try_from(v).ok());

                    let known_capabilities = input.map(|context_window| ModelCapabilities {
                        context_window,
                        max_output_tokens: output.unwrap_or(ws_context::DEFAULT_MAX_OUTPUT_TOKENS),
                        ..ModelCapabilities::default()
                    });

                    Some(ModelInfo {
                        model: ModelRef::new(backend.clone(), id),
                        display_name: display.to_owned(),
                        known_capabilities,
                    })
                })
                .collect()
        })
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    // Spec s14 permits unwrap/expect in tests.
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;

    #[test]
    fn a_model_id_is_qualified_for_the_url_and_stripped_for_storage() {
        assert_eq!(qualify("gemini-x"), "models/gemini-x");
        assert_eq!(qualify("models/gemini-x"), "models/gemini-x");
        assert_eq!(strip_prefix("models/gemini-x"), "gemini-x");
        assert_eq!(strip_prefix("gemini-x"), "gemini-x");
    }

    #[test]
    fn the_model_list_carries_the_token_limits_the_provider_reports() {
        let backend = BackendId::new("gemini");
        let body = serde_json::json!({
            "models": [{
                "name": "models/gemini-test",
                "displayName": "Gemini Test",
                "inputTokenLimit": 1_048_576,
                "outputTokenLimit": 65_536
            }]
        });
        let models = parse_model_list(&backend, &body);
        assert_eq!(models.len(), 1);
        assert_eq!(models[0].model.model_id, "gemini-test");
        let caps = models[0].known_capabilities.as_ref().unwrap();
        assert_eq!(caps.context_window, 1_048_576);
        assert_eq!(caps.max_output_tokens, 65_536);
    }

    #[test]
    fn a_model_without_limits_is_left_for_the_profile_to_fill_in() {
        let backend = BackendId::new("gemini");
        let body = serde_json::json!({"models": [{"name": "models/mystery"}]});
        let models = parse_model_list(&backend, &body);
        assert_eq!(models[0].display_name, "mystery");
        assert!(models[0].known_capabilities.is_none());
    }

    #[test]
    fn an_unexpected_model_list_yields_nothing_rather_than_panicking() {
        let backend = BackendId::new("gemini");
        assert!(parse_model_list(&backend, &serde_json::json!({})).is_empty());
        assert!(parse_model_list(&backend, &serde_json::json!({"models": 7})).is_empty());
    }
}
