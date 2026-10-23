//! The OpenAI-compatible client and `ChatBackend` implementation (spec s7.5).
//!
//! Unlike the other three, everything about this backend is user-supplied: a
//! display name, a base URL, an optional key and optional headers. Several can
//! be configured at once, each with its own `BackendId`, which is also the
//! account name its key is stored under.

use futures_util::stream::BoxStream;
use std::collections::BTreeMap;
use std::sync::Arc;
use tokio_util::sync::CancellationToken;
use ws_context::{
    BackendError, BackendEvent, BackendId, BackendRequest, ChatBackend, ModelCapabilities,
    ModelInfo, ModelProfiles, ModelRef, ReasoningSupport,
};
use ws_secrets::SecretStore;

use super::decode::CompatDecoder;
use super::encode::build_body;
use crate::http::{post_with_retry, summarise_error};
use crate::retry::RetryPolicy;
use crate::stream::event_stream;

/// A user-configured OpenAI-compatible endpoint.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompatConfig {
    pub id: BackendId,
    pub display_name: String,
    pub base_url: String,
    /// Extra headers some gateways require (a routing header, say).
    pub extra_headers: BTreeMap<String, String>,
    /// False for a local server that needs no key, so a missing key is not
    /// reported as an error.
    pub requires_key: bool,
}

#[derive(Debug, Clone)]
pub struct CompatBackend {
    config: CompatConfig,
    http: reqwest::Client,
    secrets: SecretStore,
    profiles: Arc<dyn ModelProfiles>,
    retry: RetryPolicy,
}

impl CompatBackend {
    #[must_use]
    pub fn new(config: CompatConfig, profiles: Arc<dyn ModelProfiles>) -> Self {
        Self {
            config,
            http: reqwest::Client::new(),
            secrets: SecretStore::new(),
            profiles,
            retry: RetryPolicy::default(),
        }
    }

    #[must_use]
    pub fn with_retry(mut self, retry: RetryPolicy) -> Self {
        self.retry = retry;
        self
    }

    #[must_use]
    pub fn display_name(&self) -> &str {
        &self.config.display_name
    }

    fn base(&self) -> &str {
        self.config.base_url.trim_end_matches('/')
    }

    /// Headers for a request, including the bearer token when one is set.
    fn headers(&self) -> Result<Vec<(&str, String)>, BackendError> {
        let mut headers: Vec<(&str, String)> = Vec::new();

        match self.secrets.get_key(self.config.id.as_str()) {
            Ok(Some(key)) => {
                headers.push(("authorization", format!("Bearer {}", key.expose())));
            }
            Ok(None) if self.config.requires_key => {
                return Err(BackendError::MissingApiKey {
                    backend: self.config.id.clone(),
                });
            }
            // A local server that needs no key is the normal case here.
            Ok(None) => {}
            Err(e) => {
                return Err(BackendError::Transport {
                    backend: self.config.id.clone(),
                    message: e.to_string(),
                });
            }
        }

        for (name, value) in &self.config.extra_headers {
            headers.push((name.as_str(), value.clone()));
        }
        Ok(headers)
    }
}

#[async_trait::async_trait]
impl ChatBackend for CompatBackend {
    fn id(&self) -> BackendId {
        self.config.id.clone()
    }

    async fn list_models(&self) -> Result<Vec<ModelInfo>, BackendError> {
        let headers = self.headers()?;
        let mut builder = self.http.get(format!("{}/models", self.base()));
        for (name, value) in &headers {
            builder = builder.header(*name, value);
        }

        let resp = builder.send().await.map_err(|e| BackendError::Transport {
            backend: self.config.id.clone(),
            message: e.to_string(),
        })?;

        // `/models` is optional in this protocol. A server without it is
        // usable; the user adds model ids by hand (master spec s7.5).
        if !resp.status().is_success() {
            tracing::debug!(
                backend = %self.config.id,
                status = resp.status().as_u16(),
                "endpoint does not serve a model list"
            );
            return Ok(Vec::new());
        }

        let body: serde_json::Value = resp.json().await.map_err(|e| BackendError::Protocol {
            backend: self.config.id.clone(),
            message: e.to_string(),
        })?;
        Ok(parse_model_list(&self.config.id, &body))
    }

    fn capabilities(&self, model: &ModelRef) -> ModelCapabilities {
        // Whatever the profile says, this protocol cannot replay reasoning:
        // there is nothing signed to replay (master spec s7.5). Declaring it
        // here keeps the context builder from ever trying.
        ModelCapabilities {
            reasoning: ReasoningSupport::None,
            ..self.profiles.capabilities(model)
        }
    }

    /// Chat Completions takes `tools`; a server or model that cannot use
    /// them answers with an error (OpenRouter's tool-less models are left out
    /// of the list for that reason).
    fn supports_tools(&self) -> bool {
        true
    }

    async fn stream(
        &self,
        request: BackendRequest,
        cancel: CancellationToken,
    ) -> Result<BoxStream<'static, BackendEvent>, BackendError> {
        let headers = self.headers()?;
        let body = build_body(&request, true);

        let resp = post_with_retry(
            &self.http,
            &self.config.id,
            &format!("{}/chat/completions", self.base()),
            headers,
            &body,
            self.retry,
            summarise_error,
        )
        .await?;

        let decoder = CompatDecoder::new(self.config.id.clone(), request.model.model_id.clone());
        Ok(event_stream(resp, decoder, self.config.id.clone(), cancel))
    }
}

/// The largest reply allowance taken from a gateway's list.
const MAX_REPLY: u32 = 32_768;

/// The limits a gateway reports for a model, when it reports any: OpenRouter
/// says `context_length` and `top_provider.max_completion_tokens`, Groq
/// `context_window` and `max_completion_tokens`.
fn reported_limits(item: &serde_json::Value, tools: bool) -> Option<ModelCapabilities> {
    let number = |v: Option<&serde_json::Value>| {
        v.and_then(serde_json::Value::as_u64)
            .and_then(|n| u32::try_from(n).ok())
    };
    let context_window = number(item.get("context_length"))
        .or_else(|| number(item.get("context_window")))
        .filter(|&n| n > 0)?;
    let reported = number(item.pointer("/top_provider/max_completion_tokens"))
        .or_else(|| number(item.get("max_completion_tokens")));
    let vision = item
        .pointer("/architecture/input_modalities")
        .and_then(|v| v.as_array())
        .is_some_and(|m| m.iter().any(|v| v.as_str() == Some("image")));
    Some(ModelCapabilities {
        context_window,
        // At most a quarter of the window, since the reply's room comes out of
        // it before the chat gets any; and at most 32k, since gateways charge
        // credit up front for the largest reply allowed (OpenRouter refused a
        // 262k allowance on a 1M-window model), and few replies need more.
        max_output_tokens: reported
            .unwrap_or(u32::MAX)
            .min(context_window / 4)
            .clamp(256, MAX_REPLY),
        vision,
        tools,
        ..ModelCapabilities::default()
    })
}

fn parse_model_list(backend: &BackendId, body: &serde_json::Value) -> Vec<ModelInfo> {
    body.get("data")
        .and_then(|d| d.as_array())
        .map(|items| {
            items
                .iter()
                .filter_map(|item| {
                    let id = item.get("id").and_then(|v| v.as_str())?;
                    // Gateways that list what each model takes (OpenRouter does):
                    // one that cannot call tools fails every request, since the
                    // agent's tools go with each one. Plain servers list nothing.
                    let params = item.get("supported_parameters").and_then(|v| v.as_array());
                    let tools = params.map(|p| p.iter().any(|v| v.as_str() == Some("tools")));
                    if tools == Some(false) {
                        return None;
                    }
                    Some(ModelInfo {
                        model: ModelRef::new(backend.clone(), id),
                        display_name: id.to_owned(),
                        known_capabilities: reported_limits(item, tools.unwrap_or(false)),
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
    use ws_context::DefaultProfiles;

    fn config() -> CompatConfig {
        CompatConfig {
            id: BackendId::new("local-llama"),
            display_name: "Local llama.cpp".to_owned(),
            base_url: "http://localhost:8080/v1/".to_owned(),
            extra_headers: BTreeMap::new(),
            requires_key: false,
        }
    }

    #[test]
    fn a_trailing_slash_on_the_base_url_does_not_double_up() {
        let b = CompatBackend::new(config(), Arc::new(DefaultProfiles));
        assert_eq!(b.base(), "http://localhost:8080/v1");
    }

    #[test]
    fn a_server_needing_no_key_produces_no_auth_header_and_no_error() {
        let b = CompatBackend::new(config(), Arc::new(DefaultProfiles));
        let headers = b.headers().unwrap();
        assert!(
            !headers.iter().any(|(n, _)| *n == "authorization"),
            "a local server should not be sent a bearer token it never set"
        );
    }

    #[test]
    fn extra_headers_are_passed_through() {
        let mut cfg = config();
        cfg.extra_headers
            .insert("x-route".to_owned(), "eu-west".to_owned());
        let b = CompatBackend::new(cfg, Arc::new(DefaultProfiles));
        let headers = b.headers().unwrap();
        assert!(headers.contains(&("x-route", "eu-west".to_owned())));
    }

    #[test]
    fn reasoning_replay_is_refused_whatever_the_profile_claims() {
        // Master spec s7.5: this protocol has nothing signed to replay, so the
        // capability is denied at the adapter rather than trusted from config.
        #[derive(Debug)]
        struct Optimistic;
        impl ModelProfiles for Optimistic {
            fn capabilities(&self, _m: &ModelRef) -> ModelCapabilities {
                ModelCapabilities {
                    reasoning: ReasoningSupport::Available,
                    ..ModelCapabilities::default()
                }
            }
            fn thinking(&self, _m: &ModelRef) -> ws_context::ThinkingMode {
                ws_context::ThinkingMode::Off
            }
        }

        let b = CompatBackend::new(config(), Arc::new(Optimistic));
        let caps = b.capabilities(&ModelRef::new("local-llama", "any"));
        assert_eq!(caps.reasoning, ReasoningSupport::None);
    }

    #[test]
    fn the_model_list_comes_from_the_endpoint() {
        let backend = BackendId::new("local-llama");
        let body = serde_json::json!({"data": [{"id": "llama-3-8b"}, {"id": "qwen-7b"}]});
        let models = parse_model_list(&backend, &body);
        assert_eq!(models.len(), 2);
        assert_eq!(models[0].model.model_id, "llama-3-8b");
    }

    #[test]
    fn a_gateway_list_drops_models_without_tools_and_reports_limits() {
        let backend = BackendId::new("openrouter");
        let body = serde_json::json!({"data": [
            {"id": "z-ai/glm-4.6", "context_length": 202_752, "supported_parameters": ["tools", "temperature"],
             "top_provider": {"max_completion_tokens": 131_072}, "architecture": {"input_modalities": ["text"]}},
            {"id": "translator", "context_length": 8192, "supported_parameters": ["temperature"]},
            {"id": "vision-small", "context_length": 32_000, "supported_parameters": ["tools"],
             "top_provider": {"max_completion_tokens": 2000}, "architecture": {"input_modalities": ["text", "image"]}},
            {"id": "plain"}
        ]});
        let models = parse_model_list(&backend, &body);
        let ids: Vec<_> = models.iter().map(|m| m.model.model_id.as_str()).collect();
        assert_eq!(ids, ["z-ai/glm-4.6", "vision-small", "plain"]);
        let glm = models[0].known_capabilities.as_ref().unwrap();
        assert_eq!(
            (
                glm.context_window,
                glm.max_output_tokens,
                glm.tools,
                glm.vision
            ),
            (202_752, 32_768, true, false)
        );
        let small = models[1].known_capabilities.as_ref().unwrap();
        assert_eq!((small.max_output_tokens, small.vision), (2000, true));
        // A plain server says nothing, so the model keeps its profile's numbers.
        assert!(models[2].known_capabilities.is_none());
    }

    #[test]
    fn groq_names_its_limits_differently() {
        let backend = BackendId::new("groq");
        let body = serde_json::json!({"data": [
            {"id": "llama-3.3-70b-versatile", "context_window": 131_072, "max_completion_tokens": 32_768}
        ]});
        let caps = parse_model_list(&backend, &body)[0]
            .known_capabilities
            .clone()
            .unwrap();
        // The reported 32k, a quarter of the window and the 32k cap all agree here.
        assert_eq!(
            (caps.context_window, caps.max_output_tokens),
            (131_072, 32_768)
        );
        let body = serde_json::json!({"data": [{"id": "small", "context_window": 8192, "max_completion_tokens": 1024}]});
        let caps = parse_model_list(&backend, &body)[0]
            .known_capabilities
            .clone()
            .unwrap();
        assert_eq!((caps.context_window, caps.max_output_tokens), (8192, 1024));
    }

    #[test]
    fn an_unexpected_model_list_yields_nothing_rather_than_panicking() {
        let backend = BackendId::new("local-llama");
        assert!(parse_model_list(&backend, &serde_json::json!({})).is_empty());
        assert!(parse_model_list(&backend, &serde_json::json!({"data": "no"})).is_empty());
    }
}
