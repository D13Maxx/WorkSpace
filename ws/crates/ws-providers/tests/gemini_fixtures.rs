//! Gemini decoder tests over recorded streams.

// Tests may unwrap and expect: a panic is the failure report.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::collections::BTreeMap;
use ws_context::{
    BackendError, BackendEvent, BackendId, BackendRequest, ModelRef, MsgId, Part, ProviderKind,
    RenderedMessage, RenderedView, Role, StopReason, ThinkingMode,
};
use ws_providers::gemini::build_body;
use ws_providers::stream::decode_body;
use ws_providers::{GeminiDecoder, StreamDecoder as _};

fn fixture(name: &str) -> String {
    let path = concat!(env!("CARGO_MANIFEST_DIR"), "/../../fixtures/sse/gemini/");
    std::fs::read_to_string(format!("{path}{name}"))
        .unwrap_or_else(|e| panic!("fixture {name}: {e}"))
}

fn decode_chunked(name: &str, chunk_size: usize) -> (Vec<BackendEvent>, GeminiDecoder) {
    let body = fixture(name);
    let mut decoder = GeminiDecoder::new(BackendId::new("gemini"), "gemini-test-2026");
    let events = decode_body(&body, &mut decoder, chunk_size);
    (events, decoder)
}

fn decode(name: &str) -> (Vec<BackendEvent>, GeminiDecoder) {
    decode_chunked(name, usize::MAX)
}

fn text_of(events: &[BackendEvent]) -> String {
    events
        .iter()
        .filter_map(|e| match e {
            BackendEvent::TextDelta(t) => Some(t.as_str()),
            _ => None,
        })
        .collect()
}

fn reasoning_of(events: &[BackendEvent]) -> String {
    events
        .iter()
        .filter_map(|e| match e {
            BackendEvent::ReasoningDelta(t) => Some(t.as_str()),
            _ => None,
        })
        .collect()
}

fn opaque_of(events: &[BackendEvent]) -> Vec<&ws_context::ProviderOpaque> {
    events
        .iter()
        .filter_map(|e| match e {
            BackendEvent::OpaqueBlock(o) => Some(o),
            _ => None,
        })
        .collect()
}

#[test]
fn a_plain_reply_streams_its_text_and_finishes() {
    let (events, _) = decode("text_only.sse");

    assert_eq!(text_of(&events), "Hello, world!");
    assert!(opaque_of(&events).is_empty());
    assert!(matches!(
        events.last(),
        Some(BackendEvent::Done {
            stop_reason: StopReason::EndTurn
        })
    ));
}

#[test]
fn consecutive_text_chunks_merge_into_one_part() {
    // Gemini sends a whole response per event, so a naive decoder produces one
    // part per chunk and replays a fragmented parts array.
    let (_, decoder) = decode("text_only.sse");
    let raw = decoder.raw_payload();
    let parts = raw["content"]["parts"].as_array().unwrap();
    assert_eq!(parts.len(), 1, "three chunks, one part: {parts:#?}");
    assert_eq!(parts[0]["text"], "Hello, world!");
}

#[test]
fn thought_parts_are_reasoning_and_their_signature_is_replayable() {
    let (events, decoder) = decode("thinking_with_signature.sse");

    assert_eq!(reasoning_of(&events), "Let me multiply 127 by 43.");
    assert_eq!(text_of(&events), "The answer is 5461.");

    let opaque = opaque_of(&events);
    assert_eq!(opaque.len(), 1);
    assert_eq!(opaque[0].provider, ProviderKind::Gemini);
    assert_eq!(opaque[0].kind, "gemini.part_with_signature");
    assert_eq!(
        opaque[0]
            .payload
            .get("thoughtSignature")
            .and_then(|v| v.as_str()),
        Some("CtEBAVSoXO9sig-thinking-abc123")
    );

    // The signed part is kept byte-for-byte and not merged with its
    // neighbours, because the signature is validated against the exact array.
    let parts = decoder.raw_payload()["content"]["parts"]
        .as_array()
        .unwrap()
        .clone();
    let signed: Vec<_> = parts
        .iter()
        .filter(|p| p.get("thoughtSignature").is_some())
        .collect();
    assert_eq!(signed.len(), 1);
}

#[test]
fn a_signature_on_an_ordinary_part_is_kept_and_its_text_stays_visible() {
    // The case that breaks a naive decoder. `thoughtSignature` can appear on
    // any part; it is not a marker of thinking. Keying on it would hide
    // ordinary text as reasoning, and ignoring it on non-thought parts would
    // drop a signature that must be replayed.
    let (events, decoder) = decode("signature_on_plain_part.sse");

    assert_eq!(
        text_of(&events),
        "Here is the plan: step one.",
        "signed text is still the user's answer, not reasoning"
    );
    assert_eq!(
        reasoning_of(&events),
        "",
        "a signature alone must not mark content as thinking"
    );

    let opaque = opaque_of(&events);
    assert_eq!(opaque.len(), 1, "the signature must still be captured");
    assert_eq!(
        opaque[0]
            .payload
            .get("thoughtSignature")
            .and_then(|v| v.as_str()),
        Some("CtEBsig-on-ordinary-part-xyz789")
    );
    assert!(
        opaque[0].payload.get("thought").is_none(),
        "the stored part must not claim to be a thought"
    );

    let parts = decoder.raw_payload()["content"]["parts"]
        .as_array()
        .unwrap()
        .clone();
    assert_eq!(
        parts[0]["thoughtSignature"],
        "CtEBsig-on-ordinary-part-xyz789"
    );
}

#[test]
fn hitting_the_output_cap_is_reported_as_max_tokens() {
    let (events, _) = decode("max_tokens.sse");
    assert_eq!(text_of(&events), "It begins but never");
    assert!(matches!(
        events.last(),
        Some(BackendEvent::Done {
            stop_reason: StopReason::MaxTokens
        })
    ));
}

#[test]
fn an_error_after_text_has_streamed_is_reported_and_not_retryable() {
    let (events, decoder) = decode("error_mid_stream.sse");

    assert_eq!(text_of(&events), "Partial answer that");
    assert!(decoder.has_started());

    let Some(BackendEvent::Error(BackendError::Http {
        status, message, ..
    })) = events.last()
    else {
        panic!("expected a trailing HTTP error, got {:?}", events.last());
    };
    assert_eq!(*status, 429);
    assert!(message.contains("quota"));
}

#[test]
fn a_stream_ending_without_a_finish_reason_is_a_protocol_error() {
    let body = fixture("text_only.sse");
    let truncated = &body[..body.find("finishReason").unwrap()];

    let mut decoder = GeminiDecoder::new(BackendId::new("gemini"), "gemini-test-2026");
    let mut events = decode_body(truncated, &mut decoder, usize::MAX);
    events.extend(decoder.finish());

    assert!(
        !events
            .iter()
            .any(|e| matches!(e, BackendEvent::Done { .. })),
        "a truncated stream must not look like a clean finish"
    );
    assert!(matches!(
        events.last(),
        Some(BackendEvent::Error(BackendError::Protocol { .. }))
    ));
}

#[test]
fn decoding_is_identical_however_the_body_is_chunked() {
    for name in [
        "text_only.sse",
        "thinking_with_signature.sse",
        "signature_on_plain_part.sse",
        "max_tokens.sse",
    ] {
        let (whole, whole_dec) = decode(name);
        for chunk_size in [1, 3, 17, 64] {
            let (chunked, chunked_dec) = decode_chunked(name, chunk_size);
            assert_eq!(
                whole, chunked,
                "{name} decoded differently at chunk size {chunk_size}"
            );
            assert_eq!(
                whole_dec.raw_payload(),
                chunked_dec.raw_payload(),
                "{name} raw payload differed at chunk size {chunk_size}"
            );
        }
    }
}

/// Round trip: a recorded turn, stored, then replayed must
/// go back as the same parts array it arrived in.
#[test]
fn a_stored_turn_round_trips_into_the_next_requests_contents() {
    let (_, decoder) = decode("thinking_with_signature.sse");
    let raw = decoder.raw_payload();
    let original_parts = raw["content"]["parts"].clone();

    let assistant_id = MsgId::new();
    let mut req = BackendRequest {
        model: ModelRef::new("gemini", "gemini-test-2026"),
        view: RenderedView {
            system: Some("be brief".to_owned()),
            messages: vec![
                RenderedMessage {
                    source_id: MsgId::new(),
                    role: Role::User,
                    parts: vec![Part::text("what is 127 * 43")],
                },
                RenderedMessage {
                    source_id: assistant_id,
                    role: Role::Assistant,
                    parts: vec![Part::text("The answer is 5461.")],
                },
                RenderedMessage {
                    source_id: MsgId::new(),
                    role: Role::User,
                    parts: vec![Part::text("and plus one?")],
                },
            ],
        },
        max_output_tokens: 1024,
        thinking: ThinkingMode::Adaptive,
        raw_payloads: BTreeMap::new(),
        blobs: BTreeMap::new(),
        tools: Vec::new(),
    };
    req.raw_payloads.insert(assistant_id, raw.clone());

    let body = build_body(&req);
    let contents = body["contents"].as_array().unwrap();
    assert_eq!(contents.len(), 3);
    assert_eq!(contents[0]["role"], "user");
    assert_eq!(contents[1]["role"], "model");
    assert_eq!(contents[2]["role"], "user");

    // The model turn goes back exactly as stored, signature and position
    // intact: Gemini validates the signature against this array.
    assert_eq!(
        contents[1]["parts"], original_parts,
        "the stored parts array must be replayed unchanged"
    );
    assert_eq!(body["systemInstruction"]["parts"][0]["text"], "be brief");
}

#[test]
fn the_raw_payload_records_the_model_version_the_response_reports() {
    let (_, decoder) = decode("text_only.sse");
    assert_eq!(
        decoder.raw_payload()["model"].as_str(),
        Some("gemini-test-2026")
    );
}
