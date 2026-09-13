//! An in-memory backend for driving turns in tests.
//!
//! Scripted rather than random: each call to `stream` pops the next scripted
//! reply, so a test can assert on exactly what a multi-turn sequence produced.

use futures_util::stream::{self, BoxStream, StreamExt};
use std::sync::{Arc, Mutex};
use tokio_util::sync::CancellationToken;
use ws_context::{
    BackendError, BackendEvent, BackendId, BackendRequest, ChatBackend, ModelCapabilities,
    ModelInfo, ModelProfiles, ModelRef, ProviderKind, ProviderOpaque, StopReason,
};

/// What a scripted turn should produce.
#[derive(Debug, Clone)]
pub enum Scripted {
    /// Plain text, delivered as one delta per element.
    Text(Vec<String>),
    /// Text preceded by a provider-native block, as a thinking model would.
    TextWithOpaque {
        deltas: Vec<String>,
        opaque_payload: serde_json::Value,
    },
    /// Text, then a failure partway through.
    FailsAfter { deltas: Vec<String>, error: String },
    /// Nothing streams; the request itself is refused.
    RefusesToStart { error: BackendError },
    /// Text with a pause before each delta, so a draft checkpoint can land
    /// while the reply is still arriving.
    SlowText {
        deltas: Vec<String>,
        delay: std::time::Duration,
    },
}

/// Records what each call received, so tests can assert on the request the
/// orchestrator actually built.
#[derive(Debug, Default)]
pub struct Recorder {
    pub requests: Vec<BackendRequest>,
}

#[derive(Clone)]
pub struct FakeBackend {
    id: BackendId,
    script: Arc<Mutex<Vec<Scripted>>>,
    pub recorder: Arc<Mutex<Recorder>>,
    profiles: Arc<dyn ModelProfiles>,
}

impl std::fmt::Debug for FakeBackend {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FakeBackend").field("id", &self.id).finish()
    }
}

impl FakeBackend {
    #[must_use]
    pub fn new(id: impl Into<BackendId>, profiles: Arc<dyn ModelProfiles>) -> Self {
        Self {
            id: id.into(),
            script: Arc::new(Mutex::new(Vec::new())),
            recorder: Arc::new(Mutex::new(Recorder::default())),
            profiles,
        }
    }

    /// Queue a reply. Replies are consumed in the order they are pushed.
    #[must_use]
    pub fn scripted(self, reply: Scripted) -> Self {
        if let Ok(mut s) = self.script.lock() {
            s.push(reply);
        }
        self
    }

    /// Convenience: a one-delta text reply.
    #[must_use]
    pub fn says(self, text: &str) -> Self {
        self.scripted(Scripted::Text(vec![text.to_owned()]))
    }

    /// The requests this backend has been given, in order.
    #[must_use]
    pub fn requests(&self) -> Vec<BackendRequest> {
        self.recorder
            .lock()
            .map(|r| r.requests.clone())
            .unwrap_or_default()
    }

    fn next_reply(&self) -> Scripted {
        self.script
            .lock()
            .ok()
            .and_then(|mut s| {
                if s.is_empty() {
                    None
                } else {
                    Some(s.remove(0))
                }
            })
            // An unscripted turn still has to produce something well-formed,
            // so a test that forgets a reply fails on its own assertion rather
            // than on a hang.
            .unwrap_or_else(|| Scripted::Text(vec!["(unscripted reply)".to_owned()]))
    }
}

fn raw_payload_for(text: &str, extra: Option<&serde_json::Value>) -> serde_json::Value {
    let mut content = Vec::new();
    if let Some(block) = extra {
        content.push(block.clone());
    }
    content.push(serde_json::json!({"type": "text", "text": text}));
    serde_json::json!({ "content": content })
}

#[async_trait::async_trait]
impl ChatBackend for FakeBackend {
    fn id(&self) -> BackendId {
        self.id.clone()
    }

    async fn list_models(&self) -> Result<Vec<ModelInfo>, BackendError> {
        Ok(vec![ModelInfo {
            model: ModelRef::new(self.id.clone(), "fake-1"),
            display_name: "Fake 1".to_owned(),
            known_capabilities: None,
        }])
    }

    fn capabilities(&self, model: &ModelRef) -> ModelCapabilities {
        self.profiles.capabilities(model)
    }

    async fn stream(
        &self,
        request: BackendRequest,
        cancel: CancellationToken,
    ) -> Result<BoxStream<'static, BackendEvent>, BackendError> {
        let model_id = request.model.model_id.clone();
        if let Ok(mut r) = self.recorder.lock() {
            r.requests.push(request);
        }

        let mut events: Vec<BackendEvent> = Vec::new();
        match self.next_reply() {
            Scripted::Text(deltas) => {
                let whole = deltas.concat();
                events.extend(deltas.into_iter().map(BackendEvent::TextDelta));
                events.push(BackendEvent::Usage {
                    input_tokens: 10,
                    output_tokens: 20,
                });
                events.push(BackendEvent::RawPayload(raw_payload_for(&whole, None)));
                events.push(BackendEvent::Done {
                    stop_reason: StopReason::EndTurn,
                });
            }
            Scripted::TextWithOpaque {
                deltas,
                opaque_payload,
            } => {
                let whole = deltas.concat();
                events.push(BackendEvent::OpaqueBlock(ProviderOpaque {
                    provider: ProviderKind::Anthropic,
                    model_id: model_id.clone(),
                    kind: "anthropic.thinking".to_owned(),
                    position: 0,
                    payload: opaque_payload.clone(),
                }));
                events.extend(deltas.into_iter().map(BackendEvent::TextDelta));
                events.push(BackendEvent::RawPayload(raw_payload_for(
                    &whole,
                    Some(&opaque_payload),
                )));
                events.push(BackendEvent::Done {
                    stop_reason: StopReason::EndTurn,
                });
            }
            Scripted::FailsAfter { deltas, error } => {
                events.extend(deltas.into_iter().map(BackendEvent::TextDelta));
                events.push(BackendEvent::Error(BackendError::Transport {
                    backend: self.id.clone(),
                    message: error,
                }));
            }
            Scripted::RefusesToStart { error } => return Err(error),
            Scripted::SlowText { deltas, delay } => {
                let whole = deltas.concat();
                let cancel_for_slow = cancel.clone();
                let slow = stream::iter(deltas.into_iter().map(BackendEvent::TextDelta))
                    .then(move |event| {
                        let delay = delay;
                        async move {
                            tokio::time::sleep(delay).await;
                            event
                        }
                    })
                    .chain(stream::iter(vec![
                        BackendEvent::RawPayload(raw_payload_for(&whole, None)),
                        BackendEvent::Done {
                            stop_reason: StopReason::EndTurn,
                        },
                    ]))
                    .take_while(move |_| {
                        let cancelled = cancel_for_slow.is_cancelled();
                        async move { !cancelled }
                    })
                    .chain(stream::once(async move {
                        BackendEvent::Done {
                            stop_reason: if cancel.is_cancelled() {
                                StopReason::Cancelled
                            } else {
                                StopReason::EndTurn
                            },
                        }
                    }));
                return Ok(slow.boxed());
            }
        }

        // Cancellation is honoured between events, as a real adapter does.
        let cancel_for_stream = cancel.clone();
        Ok(stream::iter(events)
            .take_while(move |_| {
                let cancelled = cancel_for_stream.is_cancelled();
                async move { !cancelled }
            })
            .chain(stream::once(async move {
                BackendEvent::Done {
                    stop_reason: if cancel.is_cancelled() {
                        StopReason::Cancelled
                    } else {
                        StopReason::EndTurn
                    },
                }
            }))
            .boxed())
    }
}
