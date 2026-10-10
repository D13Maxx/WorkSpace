//! The Anthropic HTTP client and `ChatBackend` implementation (spec s7.2).

use futures_util::stream::BoxStream;
use std::sync::Arc;
use tokio_util::sync::CancellationToken;
use ws_context::{
    BackendError, BackendEvent, BackendId, BackendRequest, ChatBackend, ModelCapabilities,
    ModelInfo, ModelProfiles, ModelRef,
};
use ws_secrets::SecretStore;

use super::decode::AnthropicDecoder;
use super::encode::{API_VERSION, build_body};
use crate::http::{post_with_retry, summarise_error};
use crate::retry::RetryPolicy;
use crate::stream::event_stream;

pub const DEFAULT_BASE_URL: &str = "https://api.anthropic.com";

#[derive(Debug, Clone)]
pub struct AnthropicBackend {
    id: BackendId,
    http: reqwest::Client,
    base_url: String,
    secrets: SecretStore,
    profiles: Arc<dyn ModelProfiles>,
    retry: RetryPolicy,
}

impl AnthropicBackend {
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

    fn url(&self, path: &str) -> String {
        format!("{}{path}", self.base_url.trim_end_matches('/'))
    }
}

#[async_trait::async_trait]
impl ChatBackend for AnthropicBackend {
    fn id(&self) -> BackendId {
        self.id.clone()
    }

    async fn list_models(&self) -> Result<Vec<ModelInfo>, BackendError> {
        let key = self.api_key()?;
        let resp = self
            .http
            .get(self.url("/v1/models"))
            .header("x-api-key", key.expose())
            .header("anthropic-version", API_VERSION)
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

    fn supports_tools(&self) -> bool {
        true
    }

    async fn stream(
        &self,
        request: BackendRequest,
        cancel: CancellationToken,
    ) -> Result<BoxStream<'static, BackendEvent>, BackendError> {
        let key = self.api_key()?;
        let body = build_body(&request, true);

        let resp = post_with_retry(
            &self.http,
            &self.id,
            &self.url("/v1/messages"),
            vec![
                ("x-api-key", key.expose().to_owned()),
                ("anthropic-version", API_VERSION.to_owned()),
            ],
            &body,
            self.retry,
            summarise_error,
        )
        .await?;

        let decoder = AnthropicDecoder::new(self.id.clone(), request.model.model_id.clone());
        Ok(event_stream(resp, decoder, self.id.clone(), cancel))
    }
}

/// Model ids come from the provider, never a hardcoded list (spec s7.1).
fn parse_model_list(backend: &BackendId, body: &serde_json::Value) -> Vec<ModelInfo> {
    body.get("data")
        .and_then(|d| d.as_array())
        .map(|items| {
            items
                .iter()
                .filter_map(|item| {
                    let id = item.get("id").and_then(|v| v.as_str())?;
                    let display = item
                        .get("display_name")
                        .and_then(|v| v.as_str())
                        .unwrap_or(id);
                    Some(ModelInfo {
                        model: ModelRef::new(backend.clone(), id),
                        display_name: display.to_owned(),
                        // Anthropic's list endpoint does not report context
                        // windows, so the profile supplies them.
                        known_capabilities: None,
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
    fn the_model_list_comes_from_the_provider() {
        let backend = BackendId::new("anthropic");
        let body = serde_json::json!({
            "data": [
                {"id": "claude-a", "display_name": "Claude A"},
                {"id": "claude-b"}
            ]
        });
        let models = parse_model_list(&backend, &body);
        assert_eq!(models.len(), 2);
        assert_eq!(models[0].model.model_id, "claude-a");
        assert_eq!(models[0].display_name, "Claude A");
        // Falls back to the id when no display name is given.
        assert_eq!(models[1].display_name, "claude-b");
    }

    #[test]
    fn an_empty_or_unexpected_model_list_yields_nothing_rather_than_panicking() {
        let backend = BackendId::new("anthropic");
        assert!(parse_model_list(&backend, &serde_json::json!({})).is_empty());
        assert!(parse_model_list(&backend, &serde_json::json!({"data": "nope"})).is_empty());
    }
}
