//! The OpenAI Responses client and `ChatBackend` implementation (spec s7.3).

use futures_util::stream::BoxStream;
use std::sync::Arc;
use tokio_util::sync::CancellationToken;
use ws_context::{
    BackendError, BackendEvent, BackendId, BackendRequest, ChatBackend, ModelCapabilities,
    ModelInfo, ModelProfiles, ModelRef,
};
use ws_secrets::SecretStore;

use super::decode::OpenAiDecoder;
use super::encode::build_body;
use crate::http::{post_with_retry, summarise_error};
use crate::retry::RetryPolicy;
use crate::stream::event_stream;

pub const DEFAULT_BASE_URL: &str = "https://api.openai.com";

#[derive(Debug, Clone)]
pub struct OpenAiBackend {
    id: BackendId,
    http: reqwest::Client,
    base_url: String,
    secrets: SecretStore,
    profiles: Arc<dyn ModelProfiles>,
    retry: RetryPolicy,
}

impl OpenAiBackend {
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
impl ChatBackend for OpenAiBackend {
    fn id(&self) -> BackendId {
        self.id.clone()
    }

    async fn list_models(&self) -> Result<Vec<ModelInfo>, BackendError> {
        let key = self.api_key()?;
        let resp = self
            .http
            .get(self.url("/v1/models"))
            .header("authorization", format!("Bearer {}", key.expose()))
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
        let body = build_body(&request, true);

        let resp = post_with_retry(
            &self.http,
            &self.id,
            &self.url("/v1/responses"),
            vec![("authorization", format!("Bearer {}", key.expose()))],
            &body,
            self.retry,
            summarise_error,
        )
        .await?;

        let decoder = OpenAiDecoder::new(self.id.clone(), request.model.model_id.clone());
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
                    Some(ModelInfo {
                        model: ModelRef::new(backend.clone(), id),
                        display_name: id.to_owned(),
                        // The list endpoint reports no context window, so the
                        // profile supplies it.
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
        let backend = BackendId::new("openai");
        let body = serde_json::json!({
            "object": "list",
            "data": [{"id": "gpt-a", "object": "model"}, {"id": "gpt-b", "object": "model"}]
        });
        let models = parse_model_list(&backend, &body);
        assert_eq!(models.len(), 2);
        assert_eq!(models[0].model.model_id, "gpt-a");
    }

    #[test]
    fn an_unexpected_model_list_yields_nothing_rather_than_panicking() {
        let backend = BackendId::new("openai");
        assert!(parse_model_list(&backend, &serde_json::json!({})).is_empty());
        assert!(parse_model_list(&backend, &serde_json::json!({"data": 5})).is_empty());
    }
}
