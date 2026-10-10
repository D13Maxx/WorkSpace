//! Building a Gemini request body (master spec s7.4).
//!
//! Roles are `user` and `model`, the system prompt is a separate
//! `systemInstruction`, and a signed part must go back at the same position it
//! came from — so replay sends the stored `content.parts` array rather than a
//! reconstruction.

use base64::Engine as _;
use serde_json::{Value, json};
use ws_context::{BackendRequest, Part, RenderedMessage, Role, ThinkingMode};

fn role_str(role: Role) -> &'static str {
    match role {
        Role::Assistant => "model",
        // Gemini has no system role inside `contents`; the system prompt is a
        // top-level field. Tool results ride as user content until M2.
        Role::User | Role::System | Role::Tool => "user",
    }
}

fn encode_part(part: &Part, req: &BackendRequest) -> Option<Value> {
    match part {
        Part::Text { text } => Some(json!({ "text": text })),

        Part::Image { blob, mime, name } => match req.blobs.get(blob) {
            Some(inline) => Some(json!({
                "inlineData": {
                    "mimeType": inline.mime,
                    "data": base64::engine::general_purpose::STANDARD.encode(&inline.bytes),
                }
            })),
            None => {
                let label = name.as_deref().unwrap_or("image");
                tracing::warn!(%blob, %mime, "image blob was not resolved for the request");
                Some(json!({ "text": format!("[Image \"{label}\" could not be loaded]") }))
            }
        },

        // Replayed exactly, including its thoughtSignature.
        Part::Opaque(o) => Some(o.payload.clone()),

        // Never sent to any model (master spec s6.3).
        Part::Reasoning { .. } => None,

        Part::File { name, .. } => Some(json!({
            "text": format!("[File \"{name}\" is not readable by this model]")
        })),

        Part::ToolCall { .. } | Part::ToolResult { .. } => None,
    }
}

fn encode_message(msg: &RenderedMessage, req: &BackendRequest) -> Option<Value> {
    // Replay the model's own stored turn rather than rebuilding it: a
    // thoughtSignature is validated against the exact parts array it came in,
    // so position and neighbours matter (master spec s7.4).
    if msg.role == Role::Assistant {
        if let Some(raw) = req.raw_payloads.get(&msg.source_id) {
            if let Some(parts) = raw.pointer("/content/parts").filter(|p| p.is_array()) {
                if !parts.as_array().is_some_and(Vec::is_empty) {
                    return Some(json!({"role": "model", "parts": parts}));
                }
            }
        }
    }

    let parts: Vec<Value> = msg
        .parts
        .iter()
        .filter_map(|p| encode_part(p, req))
        .collect();
    if parts.is_empty() {
        return None;
    }
    Some(json!({"role": role_str(msg.role), "parts": parts}))
}

/// Build the JSON body for `:streamGenerateContent`.
#[must_use]
pub fn build_body(req: &BackendRequest) -> Value {
    let contents: Vec<Value> = req
        .view
        .messages
        .iter()
        .filter_map(|m| encode_message(m, req))
        .collect();

    let mut generation = json!({ "maxOutputTokens": req.max_output_tokens });
    match req.thinking {
        // `includeThoughts` is what makes thought parts (and their signatures)
        // appear at all, so asking for thinking without it yields nothing to
        // replay.
        ThinkingMode::Off => {
            generation["thinkingConfig"] = json!({"thinkingBudget": 0});
        }
        ThinkingMode::Adaptive => {
            generation["thinkingConfig"] = json!({"includeThoughts": true, "thinkingBudget": -1});
        }
        ThinkingMode::Enabled { budget_tokens } => {
            generation["thinkingConfig"] = json!({
                "includeThoughts": true,
                "thinkingBudget": budget_tokens,
            });
        }
    }

    let mut body = json!({
        "contents": contents,
        "generationConfig": generation,
    });

    if let Some(system) = &req.view.system {
        if !system.is_empty() {
            body["systemInstruction"] = json!({ "parts": [{ "text": system }] });
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
    use ws_context::{ModelRef, MsgId, ProviderKind, ProviderOpaque, RenderedView};

    fn request(messages: Vec<RenderedMessage>, system: Option<&str>) -> BackendRequest {
        BackendRequest {
            model: ModelRef::new("gemini", "gemini-test-1"),
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
    fn roles_are_user_and_model() {
        let body = build_body(&request(
            vec![
                user("question"),
                RenderedMessage {
                    source_id: MsgId::new(),
                    role: Role::Assistant,
                    parts: vec![Part::text("answer")],
                },
            ],
            None,
        ));
        assert_eq!(body["contents"][0]["role"], "user");
        assert_eq!(body["contents"][1]["role"], "model");
        assert_eq!(body["contents"][0]["parts"][0]["text"], "question");
    }

    #[test]
    fn the_system_prompt_is_a_separate_instruction() {
        let body = build_body(&request(vec![user("hi")], Some("be brief")));
        assert_eq!(body["systemInstruction"]["parts"][0]["text"], "be brief");
        // And never a message in contents.
        assert_eq!(body["contents"].as_array().unwrap().len(), 1);

        let body = build_body(&request(vec![user("hi")], None));
        assert!(body.get("systemInstruction").is_none());
    }

    #[test]
    fn thinking_off_asks_for_a_zero_budget() {
        let body = build_body(&request(vec![user("hi")], None));
        assert_eq!(
            body["generationConfig"]["thinkingConfig"]["thinkingBudget"],
            0
        );
    }

    #[test]
    fn thinking_on_asks_for_thoughts_to_be_included() {
        // Without includeThoughts there are no thought parts, and so no
        // signatures to replay.
        let mut req = request(vec![user("hi")], None);
        req.thinking = ThinkingMode::Adaptive;
        let body = build_body(&req);
        assert_eq!(
            body["generationConfig"]["thinkingConfig"]["includeThoughts"],
            true
        );
        assert_eq!(
            body["generationConfig"]["thinkingConfig"]["thinkingBudget"],
            -1
        );

        req.thinking = ThinkingMode::Enabled {
            budget_tokens: 2048,
        };
        let body = build_body(&req);
        assert_eq!(
            body["generationConfig"]["thinkingConfig"]["thinkingBudget"],
            2048
        );
    }

    #[test]
    fn a_stored_turn_is_replayed_as_its_original_parts_array() {
        // A thoughtSignature is validated against the exact parts array it
        // arrived in, so the stored array goes back whole.
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
            json!({"content": {"role": "model", "parts": [
                {"text": "thinking", "thought": true, "thoughtSignature": "sig-1"},
                {"text": "the answer"}
            ]}}),
        );

        let body = build_body(&req);
        let parts = body["contents"][0]["parts"].as_array().unwrap();
        assert_eq!(parts.len(), 2);
        assert_eq!(parts[0]["thoughtSignature"], "sig-1");
        assert_eq!(parts[1]["text"], "the answer");
    }

    #[test]
    fn an_opaque_part_keeps_its_signature_when_sent_back() {
        let req = request(
            vec![RenderedMessage {
                source_id: MsgId::new(),
                role: Role::Assistant,
                parts: vec![
                    Part::Opaque(ProviderOpaque {
                        provider: ProviderKind::Gemini,
                        model_id: "gemini-test-1".to_owned(),
                        kind: "gemini.part_with_signature".to_owned(),
                        position: 0,
                        payload: json!({"text": "step", "thoughtSignature": "sig-xyz"}),
                    }),
                    Part::text("answer"),
                ],
            }],
            None,
        );
        let body = build_body(&req);
        let parts = body["contents"][0]["parts"].as_array().unwrap();
        assert_eq!(parts[0]["thoughtSignature"], "sig-xyz");
        assert_eq!(parts[1]["text"], "answer");
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
        let parts = build_body(&req)["contents"][0]["parts"]
            .as_array()
            .unwrap()
            .clone();
        assert_eq!(parts.len(), 1);
        assert_eq!(parts[0]["text"], "visible");
    }

    #[test]
    fn an_image_is_sent_as_inline_data() {
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

        let part = &build_body(&req)["contents"][0]["parts"][0];
        assert_eq!(part["inlineData"]["mimeType"], "image/png");
        assert_eq!(part["inlineData"]["data"], "AQID");
    }
}
