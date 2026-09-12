//! The Anthropic HTTP client and `ChatBackend` implementation (master spec s7.2).

use bytes::Bytes;
use futures_util::stream::{BoxStream, StreamExt};
use std::collections::VecDeque;
use std::sync::Arc;
use std::time::Duration;
use tokio_util::sync::CancellationToken;
use ws_context::{
    BackendError, BackendEvent, BackendId, BackendRequest, ChatBackend, ModelCapabilities,
    ModelInfo, ModelProfiles, ModelRef, StopReason,
};
use ws_secrets::SecretStore;

use super::decode::AnthropicDecoder;
use super::encode::{API_VERSION, build_body};
use crate::retry::{RetryPolicy, retry_after_from_headers};
use crate::sse::SseParser;

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

    fn transport_err(&self, e: &reqwest::Error) -> BackendError {
        BackendError::Transport {
            backend: self.id.clone(),
            message: e.to_string(),
        }
    }

    /// Send the request, retrying only while nothing has been streamed.
    ///
    /// Master spec s7.1: retry on 429, 5xx and connect errors, at most
    /// `max_attempts` times, honouring `retry-after`. Because this runs before
    /// the response body is read, no partial reply can be duplicated.
    async fn send_with_retry(
        &self,
        body: &serde_json::Value,
    ) -> Result<reqwest::Response, BackendError> {
        let key = self.api_key()?;
        let url = format!("{}/v1/messages", self.base_url.trim_end_matches('/'));
        let mut last: Option<BackendError> = None;
        let mut retry_after: Option<Duration> = None;

        for attempt in 0..self.retry.max_attempts {
            if attempt > 0 {
                let wait = self.retry.wait_for(attempt, retry_after);
                tracing::debug!(attempt, ?wait, "retrying Anthropic request");
                tokio::time::sleep(wait).await;
            }

            // Only the shape of the request is logged, never its body or
            // headers: both carry user content and the API key (s7.1).
            tracing::debug!(attempt, "posting to Anthropic");

            let result = self
                .http
                .post(&url)
                .header("x-api-key", key.expose())
                .header("anthropic-version", API_VERSION)
                .header("content-type", "application/json")
                .json(body)
                .send()
                .await;

            match result {
                Ok(resp) if resp.status().is_success() => return Ok(resp),
                Ok(resp) => {
                    let status = resp.status().as_u16();
                    let suggested = retry_after_from_headers(resp.headers());
                    let message = resp.text().await.unwrap_or_default();
                    let err = BackendError::Http {
                        backend: self.id.clone(),
                        status,
                        message: summarise_api_error(&message),
                    };
                    if !err.is_retryable() {
                        return Err(err);
                    }
                    last = Some(err);
                    retry_after = suggested;
                }
                Err(e) => {
                    let err = self.transport_err(&e);
                    if !err.is_retryable() {
                        return Err(err);
                    }
                    last = Some(err);
                    retry_after = None;
                }
            }
        }

        Err(last.unwrap_or_else(|| BackendError::Transport {
            backend: self.id.clone(),
            message: "request failed after retries".to_owned(),
        }))
    }
}

/// Pull a human-usable message out of an API error body, falling back to the
/// raw text. Never includes headers.
fn summarise_api_error(body: &str) -> String {
    serde_json::from_str::<serde_json::Value>(body)
        .ok()
        .and_then(|v| {
            v.pointer("/error/message")
                .and_then(|m| m.as_str())
                .map(str::to_owned)
        })
        .unwrap_or_else(|| body.chars().take(500).collect())
}

/// Everything the event stream needs to carry between polls.
struct StreamState {
    bytes: BoxStream<'static, reqwest::Result<Bytes>>,
    parser: SseParser,
    decoder: AnthropicDecoder,
    queue: VecDeque<BackendEvent>,
    cancel: CancellationToken,
    backend: BackendId,
    finished: bool,
    /// Bytes of an incomplete UTF-8 sequence left by the previous chunk.
    pending_tail: Vec<u8>,
}

impl StreamState {
    /// Decode one chunk of body bytes into queued events.
    fn absorb(&mut self, chunk: &[u8]) {
        // A multi-byte UTF-8 sequence can straddle a chunk boundary, and
        // from_utf8_lossy would turn the split halves into replacement
        // characters -- silently corrupting any non-ASCII reply. Decode the
        // valid prefix and carry the remainder into the next chunk.
        let mut buf = std::mem::take(&mut self.pending_tail);
        buf.extend_from_slice(chunk);

        let (text, tail) = match std::str::from_utf8(&buf) {
            Ok(text) => (text.to_owned(), Vec::new()),
            Err(e) => {
                let valid = e.valid_up_to();
                let head = String::from_utf8_lossy(&buf[..valid]).into_owned();
                match e.error_len() {
                    // Truly invalid bytes: skip them rather than waiting
                    // forever for a continuation that will never come.
                    Some(bad) => (head, buf[valid + bad..].to_vec()),
                    // Incomplete: keep it for the next chunk.
                    None => (head, buf[valid..].to_vec()),
                }
            }
        };
        self.pending_tail = tail;

        for ev in self.parser.push(&text) {
            let decoded = self.decoder.on_event(&ev);
            self.queue.extend(decoded);
        }
    }
}

fn build_event_stream(state: StreamState) -> BoxStream<'static, BackendEvent> {
    futures_util::stream::unfold(state, |mut st| async move {
        loop {
            if let Some(ev) = st.queue.pop_front() {
                return Some((ev, st));
            }
            if st.finished {
                return None;
            }

            if st.cancel.is_cancelled() {
                st.finished = true;
                st.queue.push_back(BackendEvent::Done {
                    stop_reason: StopReason::Cancelled,
                });
                continue;
            }

            // Race the next chunk against cancellation so stop is responsive
            // even when the provider has gone quiet.
            let next = tokio::select! {
                biased;
                () = st.cancel.cancelled() => {
                    st.finished = true;
                    st.queue.push_back(BackendEvent::Done { stop_reason: StopReason::Cancelled });
                    continue;
                }
                chunk = st.bytes.next() => chunk,
            };

            match next {
                Some(Ok(chunk)) => st.absorb(&chunk),
                Some(Err(e)) => {
                    st.finished = true;
                    st.queue
                        .push_back(BackendEvent::Error(BackendError::Transport {
                            backend: st.backend.clone(),
                            message: e.to_string(),
                        }));
                }
                None => {
                    st.finished = true;
                    for ev in st.parser.finish() {
                        let decoded = st.decoder.on_event(&ev);
                        st.queue.extend(decoded);
                    }
                    // Reports a protocol error unless message_stop already
                    // closed the stream cleanly.
                    let tail = st.decoder.finish();
                    st.queue.extend(tail);
                }
            }
        }
    })
    .boxed()
}

#[async_trait::async_trait]
impl ChatBackend for AnthropicBackend {
    fn id(&self) -> BackendId {
        self.id.clone()
    }

    async fn list_models(&self) -> Result<Vec<ModelInfo>, BackendError> {
        let key = self.api_key()?;
        let url = format!("{}/v1/models", self.base_url.trim_end_matches('/'));
        let resp = self
            .http
            .get(&url)
            .header("x-api-key", key.expose())
            .header("anthropic-version", API_VERSION)
            .send()
            .await
            .map_err(|e| self.transport_err(&e))?;

        if !resp.status().is_success() {
            let status = resp.status().as_u16();
            let body = resp.text().await.unwrap_or_default();
            return Err(BackendError::Http {
                backend: self.id.clone(),
                status,
                message: summarise_api_error(&body),
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
        let body = build_body(&request, true);
        let resp = self.send_with_retry(&body).await?;

        let decoder = AnthropicDecoder::new(self.id.clone(), request.model.model_id.clone());
        Ok(build_event_stream(StreamState {
            bytes: resp.bytes_stream().boxed(),
            parser: SseParser::new(),
            decoder,
            queue: VecDeque::new(),
            cancel,
            backend: self.id.clone(),
            finished: false,
            pending_tail: Vec::new(),
        }))
    }
}

/// Model ids come from the provider, never from a hardcoded list (s7.1).
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
    fn an_api_error_body_is_reduced_to_its_message() {
        let body = r#"{"type":"error","error":{"type":"invalid_request_error","message":"max_tokens is too large"}}"#;
        assert_eq!(summarise_api_error(body), "max_tokens is too large");
    }

    #[test]
    fn an_unparseable_error_body_is_truncated_not_dropped() {
        let body = "x".repeat(5000);
        let out = summarise_api_error(&body);
        assert_eq!(out.len(), 500);
    }

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
