//! OpenAI Responses streaming decoder (master spec s7.3).
//!
//! Pure: SSE events in, `BackendEvent`s out. No network, no clock.
//!
//! The rule that shapes this file: with `store: false`, the client is the only
//! thing holding the reasoning state, and a reasoning item's
//! `encrypted_content` is **incomplete while the item is in progress**. It is
//! only whole on `response.output_item.done`. Emitting from
//! `response.output_item.added` would capture a truncated blob that the API
//! then rejects on the next turn — the same failure mode as reading an
//! Anthropic thinking block before its signature arrives.

use serde_json::{Value, json};
use ws_context::{BackendError, BackendEvent, ProviderKind, ProviderOpaque, StopReason};

use crate::sse::SseEvent;

#[derive(Debug)]
pub struct OpenAiDecoder {
    backend: ws_context::BackendId,
    model_id: String,
    /// Completed output items in arrival order, for the raw payload.
    output: Vec<Value>,
    /// Index among provider-native items only, for replay ordering.
    opaque_position: u32,
    input_tokens: u32,
    output_tokens: u32,
    stop_reason: Option<StopReason>,
    started: bool,
    done: bool,
}

impl OpenAiDecoder {
    #[must_use]
    pub fn new(backend: ws_context::BackendId, model_id: impl Into<String>) -> Self {
        Self {
            backend,
            model_id: model_id.into(),
            output: Vec::new(),
            opaque_position: 0,
            input_tokens: 0,
            output_tokens: 0,
            stop_reason: None,
            started: false,
            done: false,
        }
    }

    /// True once content has been emitted; a retry after this would duplicate.
    #[must_use]
    pub fn has_started(&self) -> bool {
        self.started
    }

    /// The reassembled response, stored verbatim for same-model replay.
    #[must_use]
    pub fn raw_payload(&self) -> Value {
        json!({
            "model": self.model_id,
            "output": self.output,
            "usage": {
                "input_tokens": self.input_tokens,
                "output_tokens": self.output_tokens,
            },
        })
    }

    fn protocol_error(&self, message: impl Into<String>) -> BackendEvent {
        BackendEvent::Error(BackendError::Protocol {
            backend: self.backend.clone(),
            message: message.into(),
        })
    }

    fn decode(&mut self, ev: &SseEvent) -> Vec<BackendEvent> {
        if self.done {
            return Vec::new();
        }
        let value: Value = match serde_json::from_str(&ev.data) {
            Ok(v) => v,
            Err(e) => {
                self.done = true;
                return vec![self.protocol_error(format!("malformed event JSON: {e}"))];
            }
        };

        let kind = value
            .get("type")
            .and_then(Value::as_str)
            .or(ev.event.as_deref())
            .unwrap_or("");

        match kind {
            "response.created" | "response.in_progress" => self.on_created(&value),
            "response.output_text.delta" => {
                let text = value.get("delta").and_then(Value::as_str).unwrap_or("");
                self.started = true;
                vec![BackendEvent::TextDelta(text.to_owned())]
            }
            // Reasoning *summary* text is human-readable and shown in the UI.
            // It is not the replayable artefact; encrypted_content is.
            "response.reasoning_summary_text.delta" => {
                let text = value.get("delta").and_then(Value::as_str).unwrap_or("");
                self.started = true;
                vec![BackendEvent::ReasoningDelta(text.to_owned())]
            }
            // Deliberately ignored: the item here may carry a partial
            // encrypted_content, and storing that would poison replay.
            "response.output_item.added" => Vec::new(),
            "response.output_item.done" => self.on_item_done(&value),
            "response.completed" | "response.incomplete" => self.on_completed(&value),
            "response.failed" | "error" => self.on_error(&value),
            other => {
                tracing::debug!(event = other, "ignoring unrecognised OpenAI event");
                Vec::new()
            }
        }
    }

    fn on_created(&mut self, v: &Value) -> Vec<BackendEvent> {
        // Prefer the model the response reports: an alias resolves to a
        // concrete id, and replay must be keyed to the concrete one.
        if let Some(id) = v.pointer("/response/model").and_then(Value::as_str) {
            self.model_id = id.to_owned();
        }
        Vec::new()
    }

    fn on_item_done(&mut self, v: &Value) -> Vec<BackendEvent> {
        let Some(item) = v.get("item") else {
            return vec![self.protocol_error("output_item.done without an item")];
        };
        let item_type = item
            .get("type")
            .and_then(Value::as_str)
            .unwrap_or("unknown");

        // Every item is kept, recognised or not: dropping one produces an
        // incomplete input on the next turn (master spec s7.3).
        self.output.push(item.clone());

        if item_type != "reasoning" {
            return Vec::new();
        }
        // A reasoning item with no encrypted content is not replayable. Keep it
        // in the raw payload, but do not offer it as an opaque block that the
        // context builder would then try to send back.
        let has_encrypted = item
            .get("encrypted_content")
            .and_then(Value::as_str)
            .is_some_and(|s| !s.is_empty());
        if !has_encrypted {
            tracing::debug!("reasoning item arrived without encrypted_content");
            return Vec::new();
        }

        let position = self.opaque_position;
        self.opaque_position += 1;
        vec![BackendEvent::OpaqueBlock(ProviderOpaque {
            provider: ProviderKind::OpenAi,
            model_id: self.model_id.clone(),
            kind: "openai.reasoning_item".to_owned(),
            position,
            payload: item.clone(),
        })]
    }

    fn on_completed(&mut self, v: &Value) -> Vec<BackendEvent> {
        if let Some(usage) = v.pointer("/response/usage") {
            self.input_tokens = usage
                .get("input_tokens")
                .and_then(Value::as_u64)
                .unwrap_or(u64::from(self.input_tokens)) as u32;
            self.output_tokens = usage
                .get("output_tokens")
                .and_then(Value::as_u64)
                .unwrap_or(u64::from(self.output_tokens)) as u32;
        }

        // `incomplete` carries a reason such as hitting the output cap.
        let reason = v
            .pointer("/response/incomplete_details/reason")
            .and_then(Value::as_str);
        self.stop_reason = Some(match reason {
            Some("max_output_tokens") => StopReason::MaxTokens,
            Some(other) => StopReason::Other(other.to_owned()),
            None => StopReason::EndTurn,
        });

        // The response object holds the authoritative output array. Prefer it
        // over the items assembled from the stream, which can miss anything
        // this build did not recognise.
        if let Some(output) = v.pointer("/response/output").and_then(Value::as_array) {
            if !output.is_empty() {
                self.output = output.clone();
            }
        }

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

    fn on_error(&mut self, v: &Value) -> Vec<BackendEvent> {
        self.done = true;
        let message = v
            .pointer("/response/error/message")
            .or_else(|| v.pointer("/error/message"))
            .or_else(|| v.get("message"))
            .and_then(Value::as_str)
            .unwrap_or("unknown error")
            .to_owned();
        let code = v
            .pointer("/response/error/code")
            .or_else(|| v.pointer("/error/code"))
            .and_then(Value::as_str)
            .unwrap_or("");

        let status = match code {
            "rate_limit_exceeded" => 429,
            "server_error" => 500,
            _ => 400,
        };
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
        self.done = true;
        vec![self.protocol_error("stream ended before response.completed")]
    }
}

impl crate::stream::StreamDecoder for OpenAiDecoder {
    fn on_event(&mut self, ev: &SseEvent) -> Vec<BackendEvent> {
        self.decode(ev)
    }

    fn finish(&mut self) -> Vec<BackendEvent> {
        self.finish_inner()
    }
}
