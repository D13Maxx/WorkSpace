//! Gemini streaming decoder (master spec s7.4).
//!
//! Pure: SSE events in, `BackendEvent`s out.
//!
//! Gemini's stream is shaped unlike the other two. There are no named event
//! types and no state machine: every `data:` line is a whole
//! `GenerateContentResponse`, and the new content is in
//! `candidates[0].content.parts`.
//!
//! Two things about parts are easy to get backwards, and getting either wrong
//! silently breaks replay:
//!
//! - **`thought: true` is what marks thinking content.** It is the only
//!   reliable marker.
//! - **`thoughtSignature` is a separate concern.** It can appear on any part
//!   type, thinking or not, and it is the piece that must be returned exactly
//!   as received. Treating "has a signature" as "is thinking" would both
//!   mislabel content and drop signatures from ordinary parts.

use serde_json::{Value, json};
use ws_context::{BackendError, BackendEvent, ProviderKind, ProviderOpaque, StopReason};

use crate::sse::SseEvent;

/// How an accumulated part is being built, so consecutive text fragments merge
/// rather than producing one part per chunk.
#[derive(Debug, PartialEq)]
enum Slot {
    /// A part carrying a signature: preserved exactly, never merged into.
    Verbatim,
    Text,
    Thought,
}

#[derive(Debug)]
pub struct GeminiDecoder {
    backend: ws_context::BackendId,
    model_id: String,
    /// The assembled `content.parts`, in order.
    parts: Vec<Value>,
    /// What each accumulated part is, parallel to `parts`.
    slots: Vec<Slot>,
    opaque_position: u32,
    input_tokens: u32,
    output_tokens: u32,
    stop_reason: Option<StopReason>,
    started: bool,
    done: bool,
}

impl GeminiDecoder {
    #[must_use]
    pub fn new(backend: ws_context::BackendId, model_id: impl Into<String>) -> Self {
        Self {
            backend,
            model_id: model_id.into(),
            parts: Vec::new(),
            slots: Vec::new(),
            opaque_position: 0,
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

    /// The reassembled model turn, stored verbatim for same-model replay.
    #[must_use]
    pub fn raw_payload(&self) -> Value {
        json!({
            "model": self.model_id,
            "content": { "role": "model", "parts": self.parts },
            "usageMetadata": {
                "promptTokenCount": self.input_tokens,
                "candidatesTokenCount": self.output_tokens,
            },
        })
    }

    fn protocol_error(&self, message: impl Into<String>) -> BackendEvent {
        BackendEvent::Error(BackendError::Protocol {
            backend: self.backend.clone(),
            message: message.into(),
        })
    }

    /// Append `text` to the trailing part if it is the same kind, else start a
    /// new one. A part holding a signature is never appended to.
    fn accumulate(&mut self, slot: Slot, text: &str, thought: bool) {
        if self.slots.last() == Some(&slot) {
            if let Some(last) = self.parts.last_mut() {
                if let Some(Value::String(s)) = last.get_mut("text") {
                    s.push_str(text);
                    return;
                }
            }
        }
        let mut part = json!({ "text": text });
        if thought {
            part["thought"] = json!(true);
        }
        self.parts.push(part);
        self.slots.push(slot);
    }

    fn decode(&mut self, ev: &SseEvent) -> Vec<BackendEvent> {
        if self.done {
            return Vec::new();
        }
        // Gemini closes the stream by ending the body, not with a sentinel.
        if ev.data.trim().is_empty() {
            return Vec::new();
        }

        let value: Value = match serde_json::from_str(&ev.data) {
            Ok(v) => v,
            Err(e) => {
                self.done = true;
                return vec![self.protocol_error(format!("malformed chunk JSON: {e}"))];
            }
        };

        // An error can arrive in place of a candidate, mid-stream.
        if value.get("error").is_some() {
            return self.on_error(&value);
        }

        let mut out = Vec::new();

        if let Some(id) = value.get("modelVersion").and_then(Value::as_str) {
            self.model_id = id.to_owned();
        }

        if let Some(parts) = value
            .pointer("/candidates/0/content/parts")
            .and_then(Value::as_array)
        {
            for part in parts {
                out.extend(self.on_part(part));
            }
        }

        if let Some(usage) = value.get("usageMetadata") {
            if let Some(t) = usage.get("promptTokenCount").and_then(Value::as_u64) {
                self.input_tokens = t as u32;
            }
            if let Some(t) = usage.get("candidatesTokenCount").and_then(Value::as_u64) {
                self.output_tokens = t as u32;
            }
        }

        // finishReason on the candidate ends the turn.
        if let Some(reason) = value
            .pointer("/candidates/0/finishReason")
            .and_then(Value::as_str)
        {
            self.stop_reason = Some(match reason {
                "STOP" => StopReason::EndTurn,
                "MAX_TOKENS" => StopReason::MaxTokens,
                other => StopReason::Other(other.to_owned()),
            });
            self.done = true;
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
        }

        out
    }

    fn on_part(&mut self, part: &Value) -> Vec<BackendEvent> {
        let text = part.get("text").and_then(Value::as_str).unwrap_or("");
        let is_thought = part
            .get("thought")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        let signature = part
            .get("thoughtSignature")
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty());

        let mut out = Vec::new();

        // A signature makes the part replayable, whatever kind of part it is.
        // It is kept byte-for-byte and never merged with a neighbour.
        if let Some(_sig) = signature {
            self.parts.push(part.clone());
            self.slots.push(Slot::Verbatim);

            let position = self.opaque_position;
            self.opaque_position += 1;
            out.push(BackendEvent::OpaqueBlock(ProviderOpaque {
                provider: ProviderKind::Gemini,
                model_id: self.model_id.clone(),
                kind: "gemini.part_with_signature".to_owned(),
                position,
                payload: part.clone(),
            }));

            // A signed part can still carry visible text, which the user
            // should see even though the part itself is replayed verbatim.
            if !text.is_empty() {
                self.started = true;
                out.push(if is_thought {
                    BackendEvent::ReasoningDelta(text.to_owned())
                } else {
                    BackendEvent::TextDelta(text.to_owned())
                });
            }
            return out;
        }

        if text.is_empty() {
            return out;
        }
        self.started = true;
        if is_thought {
            self.accumulate(Slot::Thought, text, true);
            out.push(BackendEvent::ReasoningDelta(text.to_owned()));
        } else {
            self.accumulate(Slot::Text, text, false);
            out.push(BackendEvent::TextDelta(text.to_owned()));
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
        self.done = true;
        vec![self.protocol_error("stream ended without a finishReason")]
    }
}

impl crate::stream::StreamDecoder for GeminiDecoder {
    fn on_event(&mut self, ev: &SseEvent) -> Vec<BackendEvent> {
        self.decode(ev)
    }

    fn finish(&mut self) -> Vec<BackendEvent> {
        self.finish_inner()
    }
}
