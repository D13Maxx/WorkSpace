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
    /// Stop to call tools: `(call_id, name, args_json)` each.
    CallsTools(Vec<(String, String, String)>),
    /// `then`, after a pause before anything streams: a model taking its time.
    Delayed {
        delay: std::time::Duration,
        then: Box<Scripted>,
    },
}

impl Scripted {
    /// Convenience: one tool call.
    #[must_use]
    pub fn calls(call_id: &str, name: &str, args: &str) -> Self {
        Self::CallsTools(vec![(call_id.to_owned(), name.to_owned(), args.to_owned())])
    }
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
    /// Which provider this fake impersonates, so the opaque blocks it emits
    /// look like that provider's. Cross-provider tests need this to be real.
    provider: ProviderKind,
    script: Arc<Mutex<Vec<Scripted>>>,
    /// Replies for one model id, taken before the shared script: a lead and
    /// its workers running at once each follow their own.
    by_model: Arc<Mutex<std::collections::HashMap<String, Vec<Scripted>>>>,
    pub recorder: Arc<Mutex<Recorder>>,
    profiles: Arc<dyn ModelProfiles>,
    tools: bool,
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
            provider: ProviderKind::Anthropic,
            script: Arc::new(Mutex::new(Vec::new())),
            by_model: Arc::default(),
            recorder: Arc::new(Mutex::new(Recorder::default())),
            profiles,
            tools: true,
        }
    }

    /// Behave like a provider whose adapter cannot carry tools.
    #[must_use]
    pub fn without_tools(mut self) -> Self {
        self.tools = false;
        self
    }

    /// Impersonate a particular provider's opaque blocks.
    #[must_use]
    pub fn as_provider(mut self, provider: ProviderKind) -> Self {
        self.provider = provider;
        self
    }

    /// Queue a reply. Replies are consumed in the order they are pushed.
    #[must_use]
    pub fn scripted(self, reply: Scripted) -> Self {
        if let Ok(mut s) = self.script.lock() {
            s.push(reply);
        }
        self
    }

    /// Queue a reply for requests to `model_id` only; works on a shared fake.
    pub fn script_for(&self, model_id: &str, reply: Scripted) {
        if let Ok(mut m) = self.by_model.lock() {
            m.entry(model_id.to_owned()).or_default().push(reply);
        }
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

    fn next_reply(&self, model_id: &str) -> Scripted {
        let own = self.by_model.lock().ok().and_then(|mut m| {
            let queue = m.get_mut(model_id)?;
            (!queue.is_empty()).then(|| queue.remove(0))
        });
        if let Some(reply) = own {
            return reply;
        }
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

    fn supports_tools(&self) -> bool {
        self.tools
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
        let mut reply = self.next_reply(&model_id);
        while let Scripted::Delayed { delay, then } = reply {
            tokio::time::sleep(delay).await;
            reply = *then;
        }
        match reply {
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
                    provider: self.provider,
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
            // Unwrapped above.
            Scripted::Delayed { .. } => {}
            Scripted::CallsTools(calls) => {
                let mut content = Vec::new();
                for (call_id, name, args) in calls {
                    content.push(serde_json::json!({
                        "type": "tool_use", "id": call_id, "name": name,
                        "input": serde_json::from_str::<serde_json::Value>(&args)
                            .unwrap_or_else(|_| serde_json::json!({})),
                    }));
                    events.push(BackendEvent::ToolCallDelta {
                        call_id,
                        name: Some(name),
                        args_delta: args,
                    });
                }
                events.push(BackendEvent::RawPayload(
                    serde_json::json!({ "content": content }),
                ));
                events.push(BackendEvent::Done {
                    stop_reason: StopReason::ToolUse,
                });
            }
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
