//! Building an OpenAI Responses request body (master spec s7.3).
//!
//! Stateless by design: `store: false`, with
//! `include: ["reasoning.encrypted_content"]` so the client holds the
//! reasoning state. That makes replay the client's responsibility, and the
//! shape it must resend is the provider's own output items — a reasoning item
//! and the message it produced, **kept together and in order**. Splitting the
//! pair, or sending the message without its reasoning item, is rejected.

use base64::Engine as _;
use serde_json::{Value, json};
use ws_context::{BackendRequest, Part, RenderedMessage, Role};

/// Content blocks use different type names on input and output, so a message
/// being *sent* and the same message coming *back* are not interchangeable.
fn input_text_kind(role: Role) -> &'static str {
    match role {
        Role::Assistant => "output_text",
        _ => "input_text",
    }
}

fn role_str(role: Role) -> &'static str {
    match role {
        Role::Assistant => "assistant",
        Role::System => "system",
        // Tool results are carried as user content until M2 adds tool calling.
        Role::User | Role::Tool => "user",
    }
}

fn encode_part(part: &Part, req: &BackendRequest, role: Role) -> Option<Value> {
    let text_kind = input_text_kind(role);
    match part {
        Part::Text { text } => Some(json!({"type": text_kind, "text": text})),

        Part::Image { blob, mime, name } => match req.blobs.get(blob) {
            Some(inline) => {
                let b64 = base64::engine::general_purpose::STANDARD.encode(&inline.bytes);
                Some(json!({
                    "type": "input_image",
                    "image_url": format!("data:{};base64,{}", inline.mime, b64),
                }))
            }
            None => {
                let label = name.as_deref().unwrap_or("image");
                tracing::warn!(%blob, %mime, "image blob was not resolved for the request");
                Some(json!({
                    "type": text_kind,
                    "text": format!("[Image \"{label}\" could not be loaded]"),
                }))
            }
        },

        // Never sent to any model (master spec s6.3). The replayable form of
        // reasoning is the encrypted item, handled in `encode_message`.
        Part::Reasoning { .. } => None,

        // An opaque part reaching here is a bare reasoning item. It is emitted
        // as its own input item rather than as message content, so it is
        // skipped at the content level.
        Part::Opaque(_) => None,

        Part::File { name, .. } => Some(json!({
            "type": text_kind,
            "text": format!("[File \"{name}\" is not readable by this model]"),
        })),

        Part::ToolCall { .. } | Part::ToolResult { .. } => None,
    }
}

/// Append one canonical message to `input` as one or more Responses items.
fn push_message(input: &mut Vec<Value>, msg: &RenderedMessage, req: &BackendRequest) {
    // Replay from the provider's own stored response when there is one. The
    // output array already holds the reasoning items and the message in the
    // order the API produced them, which is the order it requires back.
    if msg.role == Role::Assistant {
        if let Some(raw) = req.raw_payloads.get(&msg.source_id) {
            if let Some(items) = raw.get("output").and_then(Value::as_array) {
                if !items.is_empty() {
                    input.extend(items.iter().cloned());
                    return;
                }
            }
        }
        // No stored response: emit any opaque reasoning items first, so a
        // reasoning item never trails the message it belongs to.
        for part in &msg.parts {
            if let Part::Opaque(o) = part {
                input.push(o.payload.clone());
            }
        }
    }

    let content: Vec<Value> = msg
        .parts
        .iter()
        .filter_map(|p| encode_part(p, req, msg.role))
        .collect();
    if content.is_empty() {
        return;
    }
    input.push(json!({
        "type": "message",
        "role": role_str(msg.role),
        "content": content,
    }));
}

/// Build the JSON body for `POST /v1/responses`.
#[must_use]
pub fn build_body(req: &BackendRequest, stream: bool) -> Value {
    let mut input = Vec::new();
    for msg in &req.view.messages {
        push_message(&mut input, msg, req);
    }

    let mut body = json!({
        "model": req.model.model_id,
        "input": input,
        "max_output_tokens": req.max_output_tokens,
        "stream": stream,
        // Stateless: the conversation lives in this app's database, not on
        // OpenAI's servers (master spec s7.3).
        "store": false,
        "include": ["reasoning.encrypted_content"],
    });

    if let Some(system) = &req.view.system {
        if !system.is_empty() {
            body["instructions"] = json!(system);
        }
    }

    // Reasoning settings only apply to models that reason; sending them to a
    // model that does not is an error, so the profile decides.
    match req.thinking {
        ws_context::ThinkingMode::Off => {}
        ws_context::ThinkingMode::Adaptive => {
            body["reasoning"] = json!({"effort": "medium", "summary": "auto"});
        }
        ws_context::ThinkingMode::Enabled { budget_tokens } => {
            // The Responses API takes an effort level, not a token budget, so
            // the budget is mapped onto the nearest level.
            let effort = if budget_tokens >= 8192 {
                "high"
            } else if budget_tokens >= 2048 {
                "medium"
            } else {
                "low"
            };
            body["reasoning"] = json!({"effort": effort, "summary": "auto"});
        }
    }

    body
}

#[cfg(test)]
mod tests {
    // Spec s14 permits unwrap/expect in tests.
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;
    use std::collections::BTreeMap;
    use ws_context::{ModelRef, MsgId, RenderedView, ThinkingMode};

    fn request(messages: Vec<RenderedMessage>, system: Option<&str>) -> BackendRequest {
        BackendRequest {
            model: ModelRef::new("openai", "gpt-test-1"),
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
    fn the_request_is_stateless_and_asks_for_encrypted_reasoning() {
        // Master spec s7.3: the conversation lives here, not on their servers.
        let body = build_body(&request(vec![user("hi")], None), true);
        assert_eq!(body["store"], false);
        assert_eq!(body["include"][0], "reasoning.encrypted_content");
        assert_eq!(body["stream"], true);
    }

    #[test]
    fn the_system_prompt_becomes_instructions() {
        let body = build_body(&request(vec![user("hi")], Some("be brief")), true);
        assert_eq!(body["instructions"], "be brief");

        let body = build_body(&request(vec![user("hi")], None), true);
        assert!(body.get("instructions").is_none());
    }

    #[test]
    fn user_and_assistant_text_use_different_content_types() {
        // input_text on the way in, output_text on the way back: sending the
        // wrong one is rejected.
        let body = build_body(
            &request(
                vec![
                    user("question"),
                    RenderedMessage {
                        source_id: MsgId::new(),
                        role: Role::Assistant,
                        parts: vec![Part::text("answer")],
                    },
                ],
                None,
            ),
            true,
        );
        assert_eq!(body["input"][0]["content"][0]["type"], "input_text");
        assert_eq!(body["input"][1]["content"][0]["type"], "output_text");
    }

    #[test]
    fn a_stored_response_is_replayed_as_its_own_items_in_order() {
        // The reasoning item and the message it produced must go back
        // together, in the order the API emitted them.
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
            json!({"output": [
                {"type": "reasoning", "id": "rs_1", "encrypted_content": "enc-abc", "summary": []},
                {"type": "message", "role": "assistant",
                 "content": [{"type": "output_text", "text": "original"}]}
            ]}),
        );

        let body = build_body(&req, true);
        let input = body["input"].as_array().unwrap();
        assert_eq!(input.len(), 2, "the pair is sent whole");
        assert_eq!(input[0]["type"], "reasoning");
        assert_eq!(input[0]["encrypted_content"], "enc-abc");
        assert_eq!(input[1]["type"], "message");
        assert_eq!(input[1]["content"][0]["text"], "original");
    }

    #[test]
    fn a_reasoning_item_never_trails_the_message_it_belongs_to() {
        // Without a stored response, opaque items are emitted first.
        let req = request(
            vec![RenderedMessage {
                source_id: MsgId::new(),
                role: Role::Assistant,
                parts: vec![
                    Part::text("the answer"),
                    Part::Opaque(ws_context::ProviderOpaque {
                        provider: ws_context::ProviderKind::OpenAi,
                        model_id: "gpt-test-1".to_owned(),
                        kind: "openai.reasoning_item".to_owned(),
                        position: 0,
                        payload: json!({"type": "reasoning", "id": "rs_2",
                                        "encrypted_content": "enc-xyz"}),
                    }),
                ],
            }],
            None,
        );

        let body = build_body(&req, true);
        let input = body["input"].as_array().unwrap();
        assert_eq!(input[0]["type"], "reasoning");
        assert_eq!(input[1]["type"], "message");
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
        let content = body["input"][0]["content"].as_array().unwrap();
        assert_eq!(content.len(), 1);
        assert_eq!(content[0]["text"], "visible");
    }

    #[test]
    fn a_thinking_budget_maps_onto_an_effort_level() {
        // The Responses API takes an effort level, not a token budget.
        let mut req = request(vec![user("hi")], None);
        assert!(build_body(&req, true).get("reasoning").is_none());

        req.thinking = ThinkingMode::Adaptive;
        assert_eq!(build_body(&req, true)["reasoning"]["effort"], "medium");

        req.thinking = ThinkingMode::Enabled { budget_tokens: 512 };
        assert_eq!(build_body(&req, true)["reasoning"]["effort"], "low");

        req.thinking = ThinkingMode::Enabled {
            budget_tokens: 16_384,
        };
        assert_eq!(build_body(&req, true)["reasoning"]["effort"], "high");
    }

    #[test]
    fn an_image_is_inlined_as_a_data_url() {
        let blob = ws_context::BlobRef::new("a".repeat(64));
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
            ws_context::InlineBlob {
                mime: "image/png".to_owned(),
                bytes: vec![1, 2, 3],
            },
        );

        let body = build_body(&req, true);
        let part = &body["input"][0]["content"][0];
        assert_eq!(part["type"], "input_image");
        assert_eq!(part["image_url"], "data:image/png;base64,AQID");
    }
}
