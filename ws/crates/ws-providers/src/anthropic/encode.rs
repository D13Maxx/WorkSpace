//! Building an Anthropic request body (master spec s7.2).
//!
//! Pure: a `BackendRequest` in, JSON out. Kept separate from the HTTP client so
//! the wire shape — including the replay rules, which are the easy part to get
//! wrong — is testable without a network.

use base64::Engine as _;
use serde_json::{Value, json};
use ws_context::{BackendRequest, Part, RenderedMessage, Role, ThinkingMode};

/// The `anthropic-version` header value this build targets.
pub const API_VERSION: &str = "2023-06-01";

/// Anthropic accepts only `user` and `assistant` in `messages`; the system
/// prompt is a separate top-level field.
fn role_str(role: Role) -> &'static str {
    match role {
        Role::Assistant => "assistant",
        // System messages never reach here: the context builder puts the system
        // prompt in `RenderedView::system`. Tool results are sent as user
        // content, which is Anthropic's shape for them.
        Role::User | Role::System | Role::Tool => "user",
    }
}

fn encode_part(part: &Part, req: &BackendRequest) -> Option<Value> {
    match part {
        Part::Text { text } => Some(json!({"type": "text", "text": text})),

        Part::Image { blob, mime, name } => match req.blobs.get(blob) {
            Some(inline) => Some(json!({
                "type": "image",
                "source": {
                    "type": "base64",
                    "media_type": inline.mime,
                    "data": base64::engine::general_purpose::STANDARD.encode(&inline.bytes),
                }
            })),
            // The orchestrator should have resolved this. Say so in the
            // transcript rather than dropping it silently, so a missing blob
            // is visible to the user instead of quietly changing the question.
            None => {
                let label = name.as_deref().unwrap_or("image");
                tracing::warn!(%blob, %mime, "image blob was not resolved for the request");
                Some(json!({
                    "type": "text",
                    "text": format!("[Image \"{label}\" could not be loaded]"),
                }))
            }
        },

        // A provider-native block is replayed exactly as it was received.
        Part::Opaque(o) => Some(o.payload.clone()),

        // Never sent to any model (master spec s6.3). The replayable form of
        // reasoning is the opaque block above.
        Part::Reasoning { .. } => None,

        Part::File { name, .. } => Some(json!({
            "type": "text",
            "text": format!("[File \"{name}\" — not readable by this model]"),
        })),

        // Unused in M1; shaped now so tool support needs no format change.
        Part::ToolCall {
            call_id,
            name,
            args_json,
        } => Some(json!({
            "type": "tool_use",
            "id": call_id,
            "name": name,
            "input": serde_json::from_str::<Value>(args_json).unwrap_or(json!({})),
        })),
        Part::ToolResult {
            call_id,
            content,
            is_error,
        } => {
            let inner: Vec<Value> = content.iter().filter_map(|p| encode_part(p, req)).collect();
            Some(json!({
                "type": "tool_result",
                "tool_use_id": call_id,
                "is_error": is_error,
                "content": inner,
            }))
        }
    }
}

fn encode_message(msg: &RenderedMessage, req: &BackendRequest) -> Option<Value> {
    // Replay an assistant turn from the provider's own stored response rather
    // than rebuilding it. Anthropic signs thinking blocks against the exact
    // content they were produced with, and a reconstruction that reorders or
    // omits one is rejected (master spec s5.2).
    if msg.role == Role::Assistant {
        if let Some(raw) = req.raw_payloads.get(&msg.source_id) {
            if let Some(content) = raw.get("content").filter(|c| c.is_array()) {
                return Some(json!({"role": "assistant", "content": content}));
            }
        }
    }

    let content: Vec<Value> = msg
        .parts
        .iter()
        .filter_map(|p| encode_part(p, req))
        .collect();
    if content.is_empty() {
        // An empty content array is rejected; such a message carried nothing
        // sendable (reasoning only, say) and is simply omitted.
        return None;
    }
    Some(json!({"role": role_str(msg.role), "content": content}))
}

fn thinking_value(mode: ThinkingMode) -> Option<Value> {
    match mode {
        ThinkingMode::Off => None,
        ThinkingMode::Adaptive => Some(json!({"type": "adaptive"})),
        ThinkingMode::Enabled { budget_tokens } => Some(json!({
            "type": "enabled",
            "budget_tokens": budget_tokens,
        })),
    }
}

/// Build the JSON body for `POST /v1/messages`.
#[must_use]
pub fn build_body(req: &BackendRequest, stream: bool) -> Value {
    let messages: Vec<Value> = req
        .view
        .messages
        .iter()
        .filter_map(|m| encode_message(m, req))
        .collect();

    let mut body = json!({
        "model": req.model.model_id,
        "max_tokens": req.max_output_tokens,
        "messages": messages,
        "stream": stream,
    });

    if let Some(system) = &req.view.system {
        if !system.is_empty() {
            body["system"] = json!(system);
        }
    }
    if !req.tools.is_empty() {
        body["tools"] = req
            .tools
            .iter()
            .map(|t| json!({"name": t.name, "description": t.description, "input_schema": t.input_schema}))
            .collect();
    }
    if let Some(thinking) = thinking_value(req.thinking) {
        body["thinking"] = thinking;
        // Thinking requires temperature to be left at its default; the API
        // rejects an explicit temperature alongside it.
    }
    body
}

#[cfg(test)]
mod tests {
    // Spec s14 permits unwrap/expect in tests.
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;
    use std::collections::BTreeMap;
    use ws_context::{
        BlobRef, InlineBlob, ModelRef, MsgId, ProviderKind, ProviderOpaque, RenderedView,
    };

    fn request(messages: Vec<RenderedMessage>, system: Option<&str>) -> BackendRequest {
        BackendRequest {
            model: ModelRef::new("anthropic", "claude-test-1"),
            view: RenderedView {
                system: system.map(str::to_owned),
                messages,
            },
            max_output_tokens: 1024,
            thinking: ThinkingMode::Off,
            raw_payloads: BTreeMap::new(),
            blobs: BTreeMap::new(),
            tools: Vec::new(),
        }
    }

    fn user(text: &str) -> RenderedMessage {
        RenderedMessage {
            source_id: MsgId::new(),
            role: Role::User,
            parts: vec![Part::text(text)],
        }
    }

    #[test]
    fn a_simple_turn_becomes_the_documented_shape() {
        let body = build_body(&request(vec![user("hello")], Some("be brief")), true);

        assert_eq!(body["model"], "claude-test-1");
        assert_eq!(body["max_tokens"], 1024);
        assert_eq!(body["stream"], true);
        assert_eq!(body["system"], "be brief");
        assert_eq!(body["messages"][0]["role"], "user");
        assert_eq!(body["messages"][0]["content"][0]["type"], "text");
        assert_eq!(body["messages"][0]["content"][0]["text"], "hello");
    }

    #[test]
    fn no_system_field_is_sent_when_there_is_no_system_prompt() {
        let body = build_body(&request(vec![user("hi")], None), true);
        assert!(body.get("system").is_none());
    }

    #[test]
    fn thinking_is_omitted_when_off_and_shaped_correctly_otherwise() {
        let mut req = request(vec![user("hi")], None);
        assert!(build_body(&req, true).get("thinking").is_none());

        req.thinking = ThinkingMode::Adaptive;
        assert_eq!(build_body(&req, true)["thinking"]["type"], "adaptive");

        req.thinking = ThinkingMode::Enabled {
            budget_tokens: 4096,
        };
        let body = build_body(&req, true);
        assert_eq!(body["thinking"]["type"], "enabled");
        assert_eq!(body["thinking"]["budget_tokens"], 4096);
    }

    #[test]
    fn an_assistant_turn_is_replayed_from_the_stored_provider_response() {
        // The whole point of keeping raw payloads: send back exactly what came
        // out, not a reconstruction.
        let id = MsgId::new();
        let mut req = request(
            vec![RenderedMessage {
                source_id: id,
                role: Role::Assistant,
                parts: vec![Part::text("rebuilt text that must not be used")],
            }],
            None,
        );
        req.raw_payloads.insert(
            id,
            json!({"content": [
                {"type": "thinking", "thinking": "step", "signature": "sig-abc"},
                {"type": "text", "text": "original text"}
            ]}),
        );

        let body = build_body(&req, true);
        let content = &body["messages"][0]["content"];
        assert_eq!(content[0]["type"], "thinking");
        assert_eq!(content[0]["signature"], "sig-abc");
        assert_eq!(content[1]["text"], "original text");
        assert_eq!(
            content.as_array().unwrap().len(),
            2,
            "the stored response is sent whole, in order"
        );
    }

    #[test]
    fn an_opaque_part_is_sent_back_byte_for_byte() {
        let payload = json!({"type": "thinking", "thinking": "x", "signature": "sig-xyz"});
        let req = request(
            vec![RenderedMessage {
                source_id: MsgId::new(),
                role: Role::Assistant,
                parts: vec![
                    Part::Opaque(ProviderOpaque {
                        provider: ProviderKind::Anthropic,
                        model_id: "claude-test-1".to_owned(),
                        kind: "anthropic.thinking".to_owned(),
                        position: 0,
                        payload: payload.clone(),
                    }),
                    Part::text("answer"),
                ],
            }],
            None,
        );

        let body = build_body(&req, true);
        assert_eq!(body["messages"][0]["content"][0], payload);
    }

    #[test]
    fn reasoning_text_is_never_sent() {
        let req = request(
            vec![RenderedMessage {
                source_id: MsgId::new(),
                role: Role::Assistant,
                parts: vec![
                    Part::Reasoning {
                        visible_text: Some("internal".to_owned()),
                    },
                    Part::text("visible"),
                ],
            }],
            None,
        );

        let body = build_body(&req, true);
        let content = body["messages"][0]["content"].as_array().unwrap();
        assert_eq!(content.len(), 1);
        assert_eq!(content[0]["text"], "visible");
    }

    #[test]
    fn a_message_left_with_nothing_to_send_is_omitted_entirely() {
        let req = request(
            vec![
                user("real question"),
                RenderedMessage {
                    source_id: MsgId::new(),
                    role: Role::Assistant,
                    parts: vec![Part::Reasoning {
                        visible_text: Some("only reasoning".to_owned()),
                    }],
                },
            ],
            None,
        );

        let body = build_body(&req, true);
        assert_eq!(
            body["messages"].as_array().unwrap().len(),
            1,
            "an empty content array is rejected by the API"
        );
    }

    #[test]
    fn an_image_is_inlined_as_base64_from_the_resolved_blob() {
        let blob = BlobRef::new("a".repeat(64));
        let mut req = request(
            vec![RenderedMessage {
                source_id: MsgId::new(),
                role: Role::User,
                parts: vec![Part::Image {
                    blob: blob.clone(),
                    mime: "image/png".to_owned(),
                    name: Some("shot.png".to_owned()),
                }],
            }],
            None,
        );
        req.blobs.insert(
            blob,
            InlineBlob {
                mime: "image/png".to_owned(),
                bytes: vec![1, 2, 3],
            },
        );

        let body = build_body(&req, true);
        let part = &body["messages"][0]["content"][0];
        assert_eq!(part["type"], "image");
        assert_eq!(part["source"]["media_type"], "image/png");
        assert_eq!(part["source"]["data"], "AQID");
    }

    #[test]
    fn an_unresolved_image_becomes_a_visible_note_rather_than_vanishing() {
        let req = request(
            vec![RenderedMessage {
                source_id: MsgId::new(),
                role: Role::User,
                parts: vec![Part::Image {
                    blob: BlobRef::new("b".repeat(64)),
                    mime: "image/png".to_owned(),
                    name: Some("missing.png".to_owned()),
                }],
            }],
            None,
        );

        let body = build_body(&req, true);
        let part = &body["messages"][0]["content"][0];
        assert_eq!(part["type"], "text");
        assert!(
            part["text"].as_str().unwrap().contains("missing.png"),
            "a dropped image must be visible in the transcript"
        );
    }

    #[test]
    fn tools_are_sent_in_anthropics_shape_and_omitted_when_there_are_none() {
        let mut req = request(vec![], None);
        assert!(build_body(&req, true).get("tools").is_none());

        req.tools = vec![ws_context::ToolSpec {
            name: "serena__find_symbol".to_owned(),
            description: "Find a symbol".to_owned(),
            input_schema: json!({"type": "object", "properties": {"name_path": {"type": "string"}}}),
        }];
        let body = build_body(&req, true);
        assert_eq!(body["tools"][0]["name"], "serena__find_symbol");
        assert_eq!(body["tools"][0]["input_schema"]["type"], "object");
    }
}
