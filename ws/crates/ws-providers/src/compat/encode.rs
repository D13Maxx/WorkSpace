//! Building an OpenAI-compatible Chat Completions body (spec s7.5).
//!
//! The plainest of the four shapes: a flat `messages` array with a leading
//! `system` message. Nothing provider-native is ever sent back, because this
//! protocol has nothing signed to send. Tools go as `functions`; a call goes
//! back as the assistant's `tool_calls`, its result as a `tool` message.

use base64::Engine as _;
use serde_json::{Value, json};
use ws_context::{BackendRequest, Part, RenderedMessage, Role};

fn role_str(role: Role) -> &'static str {
    match role {
        Role::Assistant => "assistant",
        Role::System => "system",
        Role::User | Role::Tool => "user",
    }
}

/// Whether any part needs the array form of `content`.
///
/// Plain text goes as a bare string: many compatible servers accept only that
/// and reject the array form outright.
fn needs_array_content(msg: &RenderedMessage) -> bool {
    msg.parts.iter().any(|p| matches!(p, Part::Image { .. }))
}

/// A tool result as Chat Completions takes it: plain text, an error marked.
fn result_text(content: &[Part], is_error: bool) -> String {
    let body = content
        .iter()
        .filter_map(Part::as_text)
        .collect::<Vec<_>>()
        .join("\n");
    if is_error {
        format!("Error: {body}")
    } else {
        body
    }
}

/// One stored message as Chat Completions messages: its tool results first
/// (each its own `tool` message, tied to its call by id), then its content
/// with any tool calls it made.
fn encode_message(msg: &RenderedMessage, req: &BackendRequest) -> Vec<Value> {
    let mut out: Vec<Value> = msg
        .parts
        .iter()
        .filter_map(|p| match p {
            Part::ToolResult {
                call_id,
                content,
                is_error,
            } => Some(json!({
                "role": "tool",
                "tool_call_id": call_id,
                "content": result_text(content, *is_error),
            })),
            _ => None,
        })
        .collect();
    let calls: Vec<Value> = msg
        .parts
        .iter()
        .filter_map(|p| match p {
            Part::ToolCall {
                call_id,
                name,
                args_json,
            } => Some(json!({
                "id": call_id,
                "type": "function",
                "function": {"name": name, "arguments": args_json},
            })),
            _ => None,
        })
        .collect();
    match encode_content(msg, req) {
        Some(mut m) => {
            if !calls.is_empty() {
                m["tool_calls"] = Value::Array(calls);
            }
            out.push(m);
        }
        None if !calls.is_empty() => {
            out.push(json!({"role": "assistant", "content": Value::Null, "tool_calls": calls}))
        }
        None => {}
    }
    out
}

fn encode_content(msg: &RenderedMessage, req: &BackendRequest) -> Option<Value> {
    if needs_array_content(msg) {
        let mut content = Vec::new();
        for part in &msg.parts {
            match part {
                Part::Text { text } => content.push(json!({"type": "text", "text": text})),
                Part::Image { blob, mime, name } => match req.blobs.get(blob) {
                    Some(inline) => {
                        let b64 = base64::engine::general_purpose::STANDARD.encode(&inline.bytes);
                        content.push(json!({
                            "type": "image_url",
                            "image_url": {"url": format!("data:{};base64,{}", inline.mime, b64)},
                        }));
                    }
                    None => {
                        let label = name.as_deref().unwrap_or("image");
                        tracing::warn!(%blob, %mime, "image blob was not resolved");
                        content.push(json!({
                            "type": "text",
                            "text": format!("[Image \"{label}\" could not be loaded]"),
                        }));
                    }
                },
                Part::File { name, .. } => content.push(json!({
                    "type": "text",
                    "text": format!("[File \"{name}\" is not readable by this model]"),
                })),
                // Reasoning is never sent (s6.3), and nothing here is
                // replayable (s7.5), so opaque parts are dropped too.
                Part::Reasoning { .. }
                | Part::Opaque(_)
                | Part::ToolCall { .. }
                | Part::ToolResult { .. } => {}
            }
        }
        if content.is_empty() {
            return None;
        }
        return Some(json!({"role": role_str(msg.role), "content": content}));
    }

    let mut text = String::new();
    for part in &msg.parts {
        match part {
            Part::Text { text: t } => text.push_str(t),
            Part::File { name, .. } => {
                text.push_str(&format!("[File \"{name}\" is not readable by this model]"));
            }
            _ => {}
        }
    }
    if text.is_empty() {
        return None;
    }
    Some(json!({"role": role_str(msg.role), "content": text}))
}

/// Build the JSON body for `POST {base}/chat/completions`.
#[must_use]
pub fn build_body(req: &BackendRequest, stream: bool) -> Value {
    let mut messages = Vec::new();
    if let Some(system) = &req.view.system {
        if !system.is_empty() {
            messages.push(json!({"role": "system", "content": system}));
        }
    }
    messages.extend(
        req.view
            .messages
            .iter()
            .flat_map(|m| encode_message(m, req)),
    );

    let mut body = json!({
        "model": req.model.model_id,
        "messages": messages,
        "max_tokens": req.max_output_tokens,
        "stream": stream,
        // Ask for usage on the final chunk. Servers that do not understand
        // this generally ignore it.
        "stream_options": {"include_usage": true},
    });
    if !req.tools.is_empty() {
        body["tools"] = req
            .tools
            .iter()
            .map(|t| json!({"type": "function", "function": {"name": t.name, "description": t.description, "parameters": t.input_schema}}))
            .collect();
    }
    body
}

#[cfg(test)]
mod tests {
    // Spec s14 permits unwrap/expect in tests.
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;
    use std::collections::BTreeMap;
    use ws_context::{ModelRef, MsgId, ProviderKind, ProviderOpaque, RenderedView, ThinkingMode};

    fn request(messages: Vec<RenderedMessage>, system: Option<&str>) -> BackendRequest {
        BackendRequest {
            model: ModelRef::new("local-llama", "llama-3-8b"),
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
    fn the_system_prompt_becomes_a_leading_system_message() {
        let body = build_body(&request(vec![user("hi")], Some("be brief")), true);
        assert_eq!(body["messages"][0]["role"], "system");
        assert_eq!(body["messages"][0]["content"], "be brief");
        assert_eq!(body["messages"][1]["role"], "user");
    }

    #[test]
    fn plain_text_is_sent_as_a_string_not_an_array() {
        // Many compatible servers accept only the string form.
        let body = build_body(&request(vec![user("hello")], None), true);
        assert_eq!(body["messages"][0]["content"], "hello");
        assert!(body["messages"][0]["content"].is_string());
    }

    #[test]
    fn an_image_switches_the_message_to_array_content() {
        let blob = ws_context::BlobRef::new("a".repeat(64));
        let mut req = request(
            vec![RenderedMessage {
                source_id: MsgId::new(),
                role: Role::User,
                parts: vec![
                    Part::text("what is this"),
                    Part::Image {
                        blob: blob.clone(),
                        mime: "image/png".to_owned(),
                        name: None,
                    },
                ],
            }],
            None,
        );
        req.blobs.insert(
            blob,
            ws_context::InlineBlob {
                mime: "image/png".to_owned(),
                bytes: vec![1, 2, 3],
            },
        );

        let content = &build_body(&req, true)["messages"][0]["content"];
        assert!(content.is_array());
        assert_eq!(content[0]["type"], "text");
        assert_eq!(content[1]["type"], "image_url");
        assert_eq!(content[1]["image_url"]["url"], "data:image/png;base64,AQID");
    }

    #[test]
    fn nothing_provider_native_is_ever_sent_back() {
        // Master spec s7.5: this protocol has no signed reasoning, so an
        // opaque part from any provider must be dropped rather than replayed
        // to a server that has never heard of it.
        let id = MsgId::new();
        let mut req = request(
            vec![RenderedMessage {
                source_id: id,
                role: Role::Assistant,
                parts: vec![
                    Part::Opaque(ProviderOpaque {
                        provider: ProviderKind::Anthropic,
                        model_id: "claude-x".to_owned(),
                        kind: "anthropic.thinking".to_owned(),
                        position: 0,
                        payload: json!({"type": "thinking", "signature": "sig-1"}),
                    }),
                    Part::text("the answer"),
                ],
            }],
            None,
        );
        // Even a stored raw payload must not be resurrected here.
        req.raw_payloads
            .insert(id, json!({"choices": [{"message": {"content": "x"}}]}));

        let body = build_body(&req, true);
        let serialised = body.to_string();
        assert!(!serialised.contains("sig-1"), "no signature may be sent");
        assert!(
            !serialised.contains("thinking"),
            "no opaque block may be sent"
        );
        assert_eq!(body["messages"][0]["content"], "the answer");
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
        assert_eq!(build_body(&req, true)["messages"][0]["content"], "visible");
    }

    #[test]
    fn tools_calls_and_results_go_in_the_chat_completions_shape() {
        let mut req = request(
            vec![
                user("read a.txt"),
                RenderedMessage {
                    source_id: MsgId::new(),
                    role: Role::Assistant,
                    parts: vec![Part::ToolCall {
                        call_id: "call_1".to_owned(),
                        name: "read_file".to_owned(),
                        args_json: r#"{"path":"a.txt"}"#.to_owned(),
                    }],
                },
                RenderedMessage {
                    source_id: MsgId::new(),
                    role: Role::Tool,
                    parts: vec![Part::ToolResult {
                        call_id: "call_1".to_owned(),
                        content: vec![Part::text("no such file")],
                        is_error: true,
                    }],
                },
            ],
            None,
        );
        req.tools.push(ws_context::ToolSpec {
            name: "read_file".to_owned(),
            description: "Read a file".to_owned(),
            input_schema: json!({"type": "object", "properties": {"path": {"type": "string"}}}),
        });
        let body = build_body(&req, true);
        assert_eq!(body["tools"][0]["type"], "function");
        assert_eq!(body["tools"][0]["function"]["name"], "read_file");
        assert_eq!(body["tools"][0]["function"]["parameters"]["type"], "object");
        let m = &body["messages"];
        assert_eq!(m[1]["role"], "assistant");
        assert!(m[1]["content"].is_null());
        assert_eq!(m[1]["tool_calls"][0]["id"], "call_1");
        assert_eq!(
            m[1]["tool_calls"][0]["function"]["arguments"],
            r#"{"path":"a.txt"}"#
        );
        assert_eq!(
            m[2],
            json!({"role": "tool", "tool_call_id": "call_1", "content": "Error: no such file"})
        );
        // Without tools offered, the body has no tools key at all.
        assert!(
            build_body(&request(vec![user("hi")], None), true)
                .get("tools")
                .is_none()
        );
    }

    #[test]
    fn a_message_with_nothing_sendable_is_omitted() {
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
        assert_eq!(
            build_body(&req, true)["messages"].as_array().unwrap().len(),
            1
        );
    }
}
