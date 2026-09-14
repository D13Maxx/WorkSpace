//! OpenAI-compatible (Chat Completions) streaming decoder (spec s7.5).
//!
//! The defining property of this adapter is what it does *not* do: it never
//! emits an opaque block. Chat Completions has no signed reasoning to replay,
//! and the reasoning fields that some servers add are non-standard and carry
//! no signature. They are shown to the user and then dropped.
//!
//! Emitting them as opaque would be worse than useless: the context builder
//! would faithfully replay a field the next server has never heard of.

use serde_json::{Value, json};
use ws_context::{BackendError, BackendEvent, StopReason};

use crate::sse::SseEvent;

/// Delta fields used by servers that expose reasoning. None is standard, and
/// none is replayable; they exist only so the user can see the thinking.
const REASONING_FIELDS: [&str; 3] = ["reasoning_content", "reasoning", "thinking"];

#[derive(Debug)]
pub struct CompatDecoder {
    backend: ws_context::BackendId,
    model_id: String,
    text: String,
    reasoning: String,
    input_tokens: u32,
    output_tokens: u32,
    stop_reason: Option<StopReason>,
    started: bool,
    done: bool,
}

impl CompatDecoder {
    #[must_use]
    pub fn new(backend: ws_context::BackendId, model_id: impl Into<String>) -> Self {
        Self {
            backend,
            model_id: model_id.into(),
            text: String::new(),
            reasoning: String::new(),
            input_tokens: 0,
            output_tokens: 0,
            stop_reason: None,
            started: false,
            done: false,
        }
    }

    #[must_use]
    pub fn has_started(&self) -> bool {
        self.started
    }

    /// The reassembled reply.
    ///
    /// Stored for the record, but this adapter's capabilities declare no
    /// opaque replay, so the context builder will never send it back.
    #[must_use]
    pub fn raw_payload(&self) -> Value {
        json!({
            "model": self.model_id,
            "choices": [{
                "index": 0,
                "message": {"role": "assistant", "content": self.text},
                "finish_reason": match self.stop_reason {
                    Some(StopReason::MaxTokens) => "length",
                    _ => "stop",
                },
            }],
            "usage": {
                "prompt_tokens": self.input_tokens,
                "completion_tokens": self.output_tokens,
            },
        })
    }

    fn protocol_error(&self, message: impl Into<String>) -> BackendEvent {
        BackendEvent::Error(BackendError::Protocol {
            backend: self.backend.clone(),
            message: message.into(),
        })
    }

    fn finish_now(&mut self) -> Vec<BackendEvent> {
        self.done = true;
        let mut out = Vec::new();
        if self.input_tokens > 0 || self.output_tokens > 0 {
            out.push(BackendEvent::Usage {
                input_tokens: self.input_tokens,
                output_tokens: self.output_tokens,
            });
        }
        out.push(BackendEvent::RawPayload(self.raw_payload()));
        out.push(BackendEvent::Done {
            stop_reason: self.stop_reason.clone().unwrap_or(StopReason::EndTurn),
        });
        out
    }

    fn decode(&mut self, ev: &SseEvent) -> Vec<BackendEvent> {
        if self.done {
            return Vec::new();
        }
        let data = ev.data.trim();
        if data.is_empty() {
            return Vec::new();
        }
        // Chat Completions signals the end with a literal sentinel rather than
        // by closing the body.
        if data == "[DONE]" {
            return self.finish_now();
        }

        let value: Value = match serde_json::from_str(data) {
            Ok(v) => v,
            Err(e) => {
                self.done = true;
                return vec![self.protocol_error(format!("malformed chunk JSON: {e}"))];
            }
        };

        if value.get("error").is_some() {
            return self.on_error(&value);
        }

        if let Some(id) = value.get("model").and_then(Value::as_str) {
            self.model_id = id.to_owned();
        }

        let mut out = Vec::new();

        if let Some(delta) = value.pointer("/choices/0/delta") {
            // A server may send reasoning and content in the same delta.
            for field in REASONING_FIELDS {
                if let Some(t) = delta.get(field).and_then(Value::as_str) {
                    if !t.is_empty() {
                        self.reasoning.push_str(t);
                        self.started = true;
                        out.push(BackendEvent::ReasoningDelta(t.to_owned()));
                    }
                }
            }
            if let Some(t) = delta.get("content").and_then(Value::as_str) {
                if !t.is_empty() {
                    self.text.push_str(t);
                    self.started = true;
                    out.push(BackendEvent::TextDelta(t.to_owned()));
                }
            }
        }

        if let Some(usage) = value.get("usage").filter(|u| !u.is_null()) {
            if let Some(t) = usage.get("prompt_tokens").and_then(Value::as_u64) {
                self.input_tokens = t as u32;
            }
            if let Some(t) = usage.get("completion_tokens").and_then(Value::as_u64) {
                self.output_tokens = t as u32;
            }
        }

        if let Some(reason) = value
            .pointer("/choices/0/finish_reason")
            .and_then(Value::as_str)
        {
            self.stop_reason = Some(match reason {
                "stop" => StopReason::EndTurn,
                "length" => StopReason::MaxTokens,
                "tool_calls" | "function_call" => StopReason::ToolUse,
                "content_filter" => StopReason::Other("content_filter".to_owned()),
                other => StopReason::Other(other.to_owned()),
            });
            // Do not finish here: many servers send a final usage-only chunk
            // after the finish_reason, and [DONE] is the real terminator.
        }

        out
    }

    fn on_error(&mut self, v: &Value) -> Vec<BackendEvent> {
        self.done = true;
        let message = v
            .pointer("/error/message")
            .and_then(Value::as_str)
            .unwrap_or("unknown error")
            .to_owned();
        let status = v
            .pointer("/error/code")
            .and_then(Value::as_u64)
            .and_then(|c| u16::try_from(c).ok())
            .unwrap_or(400);
        vec![BackendEvent::Error(BackendError::Http {
            backend: self.backend.clone(),
            status,
            message,
        })]
    }

    fn finish_inner(&mut self) -> Vec<BackendEvent> {
        if self.done {
            return Vec::new();
        }
        // A body that simply ends after a finish_reason is common enough among
        // compatible servers to treat as a clean close rather than an error.
        if self.stop_reason.is_some() {
            return self.finish_now();
        }
        self.done = true;
        vec![self.protocol_error("stream ended before any finish_reason or [DONE]")]
    }
}

impl crate::stream::StreamDecoder for CompatDecoder {
    fn on_event(&mut self, ev: &SseEvent) -> Vec<BackendEvent> {
        self.decode(ev)
    }

    fn finish(&mut self) -> Vec<BackendEvent> {
        self.finish_inner()
    }
}
