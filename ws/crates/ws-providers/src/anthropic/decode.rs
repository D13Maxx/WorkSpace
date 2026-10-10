//! Anthropic streaming decoder (master spec s7.2).
//!
//! A pure state machine: SSE events in, `BackendEvent`s out. No network, no
//! clock, no I/O, so the whole protocol is testable from recorded fixtures.
//!
//! Two rules from the spec shape the design:
//!
//! - A thinking block's signature arrives in its own delta shortly before the
//!   block closes. The block is only valid whole, so nothing is emitted until
//!   `content_block_stop`, at which point one `OpaqueBlock` carries the
//!   complete block.
//! - Content blocks are never filtered by type when building the raw payload.
//!   Dropping a `redacted_thinking` block it does not recognise is how a
//!   client ends up sending a reply back that the API then rejects.
//!
//! The event and field names follow Anthropic's documented stream shape. The
//! live smoke test behind `live-tests` is what verifies them against the real
//! API; the fixtures here verify the state machine.

use serde_json::{Map, Value, json};
use std::collections::BTreeMap;
use ws_context::{BackendError, BackendEvent, ProviderKind, ProviderOpaque, StopReason};

use crate::sse::SseEvent;

/// Block types whose content must be replayed verbatim to the provider.
fn is_provider_native(block_type: &str) -> bool {
    matches!(block_type, "thinking" | "redacted_thinking")
}

fn opaque_kind(block_type: &str) -> String {
    format!("anthropic.{block_type}")
}

/// One content block being assembled from its deltas.
#[derive(Debug, Clone)]
struct Block {
    block_type: String,
    /// The block as it arrived at `content_block_start`, with delta text
    /// folded back in as it streams.
    value: Map<String, Value>,
}

impl Block {
    fn append(&mut self, field: &str, text: &str) {
        match self.value.get_mut(field) {
            Some(Value::String(s)) => s.push_str(text),
            _ => {
                self.value
                    .insert(field.to_owned(), Value::String(text.to_owned()));
            }
        }
    }

    fn set(&mut self, field: &str, text: &str) {
        self.value
            .insert(field.to_owned(), Value::String(text.to_owned()));
    }
}

#[derive(Debug)]
pub struct AnthropicDecoder {
    backend: ws_context::BackendId,
    model_id: String,
    /// Blocks by their `index`, so out-of-order deltas cannot corrupt one.
    blocks: BTreeMap<u32, Block>,
    /// Completed blocks in arrival order, for the raw payload.
    finished: Vec<Value>,
    /// Counter for `ProviderOpaque::position`: the index among provider-native
    /// blocks only, which is what replay ordering depends on.
    opaque_position: u32,
    stop_reason: Option<StopReason>,
    input_tokens: u32,
    output_tokens: u32,
    /// Set once any content has been emitted, which forbids a retry
    /// (master spec s7.1).
    started: bool,
    done: bool,
}

impl AnthropicDecoder {
    #[must_use]
    pub fn new(backend: ws_context::BackendId, model_id: impl Into<String>) -> Self {
        Self {
            backend,
            model_id: model_id.into(),
            blocks: BTreeMap::new(),
            finished: Vec::new(),
            opaque_position: 0,
            stop_reason: None,
            input_tokens: 0,
            output_tokens: 0,
            started: false,
            done: false,
        }
    }

    /// True once any byte of content has been emitted. After this, a failure
    /// must not be retried, because resending would duplicate the reply.
    #[must_use]
    pub fn has_started(&self) -> bool {
        self.started
    }

    /// The reassembled response content, to be stored verbatim for same-model
    /// replay. Includes every block type, recognised or not.
    #[must_use]
    pub fn raw_payload(&self) -> Value {
        json!({
            "model": self.model_id,
            "content": self.finished,
            "stop_reason": self.stop_reason,
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

    /// Feed one SSE event.
    fn decode(&mut self, ev: &SseEvent) -> Vec<BackendEvent> {
        if self.done {
            return Vec::new();
        }
        // Anthropic sends bare `ping` events with no useful payload.
        if ev.event.as_deref() == Some("ping") {
            return Vec::new();
        }
        let value: Value = match serde_json::from_str(&ev.data) {
            Ok(v) => v,
            Err(e) => {
                self.done = true;
                return vec![self.protocol_error(format!("malformed event JSON: {e}"))];
            }
        };

        // Prefer the payload's own `type`; the SSE `event:` line mirrors it.
        let kind = value
            .get("type")
            .and_then(Value::as_str)
            .or(ev.event.as_deref())
            .unwrap_or("");

        match kind {
            "message_start" => self.on_message_start(&value),
            "content_block_start" => self.on_block_start(&value),
            "content_block_delta" => self.on_block_delta(&value),
            "content_block_stop" => self.on_block_stop(&value),
            "message_delta" => self.on_message_delta(&value),
            "message_stop" => self.on_message_stop(),
            "error" => self.on_error(&value),
            // Unknown event types are ignored rather than fatal: the API may
            // add events, and an old build should degrade, not break.
            other => {
                tracing::debug!(event = other, "ignoring unrecognised Anthropic event");
                Vec::new()
            }
        }
    }

    fn on_message_start(&mut self, v: &Value) -> Vec<BackendEvent> {
        if let Some(usage) = v.pointer("/message/usage") {
            self.input_tokens = usage
                .get("input_tokens")
                .and_then(Value::as_u64)
                .unwrap_or(0) as u32;
            self.output_tokens = usage
                .get("output_tokens")
                .and_then(Value::as_u64)
                .unwrap_or(0) as u32;
        }
        // Trust the model id the response reports over the one requested: an
        // alias like "-latest" resolves to a concrete id, and replay must be
        // keyed to the concrete one.
        if let Some(id) = v.pointer("/message/model").and_then(Value::as_str) {
            self.model_id = id.to_owned();
        }
        Vec::new()
    }

    fn on_block_start(&mut self, v: &Value) -> Vec<BackendEvent> {
        let Some(index) = v.get("index").and_then(Value::as_u64) else {
            return vec![self.protocol_error("content_block_start without an index")];
        };
        let block = v.get("content_block").cloned().unwrap_or(json!({}));
        let block_type = block
            .get("type")
            .and_then(Value::as_str)
            .unwrap_or("unknown")
            .to_owned();
        let value = block.as_object().cloned().unwrap_or_default();

        self.blocks
            .insert(index as u32, Block { block_type, value });
        Vec::new()
    }

    fn on_block_delta(&mut self, v: &Value) -> Vec<BackendEvent> {
        let Some(index) = v.get("index").and_then(Value::as_u64) else {
            return vec![self.protocol_error("content_block_delta without an index")];
        };
        let index = index as u32;
        let Some(delta) = v.get("delta") else {
            return vec![self.protocol_error("content_block_delta without a delta")];
        };
        let delta_type = delta.get("type").and_then(Value::as_str).unwrap_or("");

        let Some(block) = self.blocks.get_mut(&index) else {
            return vec![self.protocol_error(format!("delta for unknown block {index}"))];
        };

        match delta_type {
            "text_delta" => {
                let text = delta.get("text").and_then(Value::as_str).unwrap_or("");
                block.append("text", text);
                self.started = true;
                vec![BackendEvent::TextDelta(text.to_owned())]
            }
            "thinking_delta" => {
                let text = delta.get("thinking").and_then(Value::as_str).unwrap_or("");
                block.append("thinking", text);
                self.started = true;
                vec![BackendEvent::ReasoningDelta(text.to_owned())]
            }
            // The signature closes the block cryptographically. It is stored,
            // never shown, and never emitted on its own.
            "signature_delta" => {
                let sig = delta.get("signature").and_then(Value::as_str).unwrap_or("");
                block.set("signature", sig);
                Vec::new()
            }
            "input_json_delta" => {
                let partial = delta
                    .get("partial_json")
                    .and_then(Value::as_str)
                    .unwrap_or("");
                // Accumulated only. The call is emitted whole at block stop,
                // because a call with no arguments may send no delta at all.
                block.append("partial_json", partial);
                Vec::new()
            }
            other => {
                tracing::debug!(delta = other, "ignoring unrecognised Anthropic delta");
                Vec::new()
            }
        }
    }

    fn on_block_stop(&mut self, v: &Value) -> Vec<BackendEvent> {
        let Some(index) = v.get("index").and_then(Value::as_u64) else {
            return vec![self.protocol_error("content_block_stop without an index")];
        };
        let Some(mut block) = self.blocks.remove(&(index as u32)) else {
            return vec![self.protocol_error(format!("stop for unknown block {index}"))];
        };

        let mut events = Vec::new();
        if block.block_type == "tool_use" {
            // The arguments stream as `partial_json` text while `input` stays the
            // `{}` from block start. Replay sends the raw block back verbatim, so
            // folding them in here is what keeps the arguments on the next turn.
            let args = block
                .value
                .remove("partial_json")
                .and_then(|v| v.as_str().map(str::to_owned))
                .filter(|a| !a.trim().is_empty())
                .unwrap_or_else(|| "{}".to_owned());
            if let Ok(input) = serde_json::from_str::<Value>(&args) {
                block.value.insert("input".to_owned(), input);
            }
            let field = |k: &str| {
                block
                    .value
                    .get(k)
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_owned()
            };
            // Unparseable arguments are passed through as-is, so the tool
            // layer can report them to the model instead of silently using {}.
            events.push(BackendEvent::ToolCallDelta {
                call_id: field("id"),
                name: Some(field("name")),
                args_delta: args,
            });
        }

        let complete = Value::Object(block.value);
        // Every block type is kept, including ones this build does not
        // understand (master spec s7.2).
        self.finished.push(complete.clone());

        if !is_provider_native(&block.block_type) {
            return events;
        }
        let position = self.opaque_position;
        self.opaque_position += 1;
        vec![BackendEvent::OpaqueBlock(ProviderOpaque {
            provider: ProviderKind::Anthropic,
            model_id: self.model_id.clone(),
            kind: opaque_kind(&block.block_type),
            position,
            payload: complete,
        })]
    }

    fn on_message_delta(&mut self, v: &Value) -> Vec<BackendEvent> {
        if let Some(reason) = v.pointer("/delta/stop_reason").and_then(Value::as_str) {
            self.stop_reason = Some(match reason {
                "end_turn" => StopReason::EndTurn,
                "max_tokens" => StopReason::MaxTokens,
                "stop_sequence" => StopReason::StopSequence,
                "tool_use" => StopReason::ToolUse,
                other => StopReason::Other(other.to_owned()),
            });
        }
        let mut out = Vec::new();
        if let Some(usage) = v.get("usage") {
            if let Some(t) = usage.get("output_tokens").and_then(Value::as_u64) {
                self.output_tokens = t as u32;
            }
            if let Some(t) = usage.get("input_tokens").and_then(Value::as_u64) {
                self.input_tokens = t as u32;
            }
            out.push(BackendEvent::Usage {
                input_tokens: self.input_tokens,
                output_tokens: self.output_tokens,
            });
        }
        out
    }

    fn on_message_stop(&mut self) -> Vec<BackendEvent> {
        self.done = true;
        vec![
            BackendEvent::RawPayload(self.raw_payload()),
            BackendEvent::Done {
                stop_reason: self.stop_reason.clone().unwrap_or(StopReason::EndTurn),
            },
        ]
    }

    fn on_error(&mut self, v: &Value) -> Vec<BackendEvent> {
        self.done = true;
        let message = v
            .pointer("/error/message")
            .and_then(Value::as_str)
            .unwrap_or("unknown error")
            .to_owned();
        let error_type = v
            .pointer("/error/type")
            .and_then(Value::as_str)
            .unwrap_or("");

        // Mid-stream overload is the one error worth distinguishing: it maps
        // to a retryable status, though s7.1 still forbids retrying once
        // content has started.
        let status = match error_type {
            "overloaded_error" => 529,
            "rate_limit_error" => 429,
            "api_error" => 500,
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
        vec![self.protocol_error("stream ended before message_stop")]
    }
}

impl crate::stream::StreamDecoder for AnthropicDecoder {
    fn on_event(&mut self, ev: &SseEvent) -> Vec<BackendEvent> {
        self.decode(ev)
    }

    fn finish(&mut self) -> Vec<BackendEvent> {
        self.finish_inner()
    }
}
