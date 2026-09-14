//! OpenAI Responses decoder tests over recorded streams (master spec s7.6).

// Spec s14 permits unwrap/expect in tests.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::collections::BTreeMap;
use ws_context::{
    BackendError, BackendEvent, BackendId, BackendRequest, ModelRef, MsgId, Part, ProviderKind,
    RenderedMessage, RenderedView, Role, StopReason, ThinkingMode,
};
use ws_providers::openai::build_body;
use ws_providers::stream::decode_body;
use ws_providers::{OpenAiDecoder, StreamDecoder as _};

fn fixture(name: &str) -> String {
    let path = concat!(env!("CARGO_MANIFEST_DIR"), "/../../fixtures/sse/openai/");
    std::fs::read_to_string(format!("{path}{name}"))
        .unwrap_or_else(|e| panic!("fixture {name}: {e}"))
}

fn decode_chunked(name: &str, chunk_size: usize) -> (Vec<BackendEvent>, OpenAiDecoder) {
    let body = fixture(name);
    let mut decoder = OpenAiDecoder::new(BackendId::new("openai"), "gpt-test-2026");
    let events = decode_body(&body, &mut decoder, chunk_size);
    (events, decoder)
}

fn decode(name: &str) -> (Vec<BackendEvent>, OpenAiDecoder) {
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

    assert_eq!(text_of(&events), "Hello, world");
    assert!(opaque_of(&events).is_empty());
    assert!(matches!(
        events.last(),
        Some(BackendEvent::Done {
            stop_reason: StopReason::EndTurn
        })
    ));
    assert!(
        events
            .iter()
            .any(|e| matches!(e, BackendEvent::Usage { .. })),
        "usage must be reported"
    );
}

#[test]
fn a_reasoning_item_is_emitted_with_its_encrypted_content() {
    let (events, _) = decode("reasoning_with_encrypted.sse");

    assert_eq!(reasoning_of(&events), "**Working through it**");
    assert_eq!(text_of(&events), "The answer is 5461.");

    let opaque = opaque_of(&events);
    assert_eq!(opaque.len(), 1);
    assert_eq!(opaque[0].provider, ProviderKind::OpenAi);
    assert_eq!(opaque[0].kind, "openai.reasoning_item");
    assert_eq!(
        opaque[0]
            .payload
            .get("encrypted_content")
            .and_then(|v| v.as_str()),
        Some("gAAAAABnQe9mFULL-ENCRYPTED-PAYLOAD-abcdef123456")
    );
}

#[test]
fn the_encrypted_content_comes_from_the_done_event_not_the_added_one() {
    // The trap this decoder exists to avoid. `output_item.added` carries a
    // truncated encrypted_content; only `output_item.done` has the whole
    // thing. Capturing the partial one stores a blob the API later rejects.
    let (events, decoder) = decode("partial_encrypted_on_added.sse");

    let opaque = opaque_of(&events);
    assert_eq!(opaque.len(), 1, "one reasoning item, emitted once");

    let encrypted = opaque[0]
        .payload
        .get("encrypted_content")
        .and_then(|v| v.as_str())
        .unwrap();
    assert_eq!(
        encrypted, "TRUNCATED-DO-NOT-USE-THIS-PREFIX-COMPLETE-PAYLOAD-xyz789",
        "the complete payload must be used"
    );
    assert_ne!(
        encrypted, "TRUNCATED-DO-NOT",
        "the partial payload from output_item.added must never be stored"
    );

    // And the stored response must carry the complete one too.
    let raw = decoder.raw_payload();
    let stored = raw["output"][0]["encrypted_content"].as_str().unwrap();
    assert!(stored.ends_with("xyz789"));
}

#[test]
fn hitting_the_output_cap_is_reported_as_max_tokens() {
    let (events, _) = decode("incomplete_max_tokens.sse");

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
    assert!(
        decoder.has_started(),
        "content was emitted, so a retry would duplicate it"
    );

    let Some(BackendEvent::Error(BackendError::Http { status, .. })) = events.last() else {
        panic!("expected a trailing HTTP error, got {:?}", events.last());
    };
    assert_eq!(*status, 429);
}

#[test]
fn a_stream_cut_short_reports_a_protocol_error_rather_than_completing() {
    let body = fixture("text_only.sse");
    let truncated = &body[..body.find("event: response.completed").unwrap()];

    let mut decoder = OpenAiDecoder::new(BackendId::new("openai"), "gpt-test-2026");
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
        "reasoning_with_encrypted.sse",
        "partial_encrypted_on_added.sse",
        "incomplete_max_tokens.sse",
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

/// Round trip (master spec s7.6): a recorded response, stored, then replayed
/// must produce the input shape the API documents.
#[test]
fn a_stored_response_round_trips_into_the_next_requests_input() {
    let (_, decoder) = decode("reasoning_with_encrypted.sse");
    let raw = decoder.raw_payload();

    let assistant_id = MsgId::new();
    let mut req = BackendRequest {
        model: ModelRef::new("openai", "gpt-test-2026"),
        view: RenderedView {
            system: None,
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
    };
    req.raw_payloads.insert(assistant_id, raw);

    let body = build_body(&req, true);
    let input = body["input"].as_array().unwrap();

    // user, reasoning, message, user — the reasoning item and the message it
    // produced must arrive together and in that order.
    assert_eq!(input.len(), 4, "got {input:#?}");
    assert_eq!(input[0]["type"], "message");
    assert_eq!(input[0]["role"], "user");
    assert_eq!(input[1]["type"], "reasoning");
    assert_eq!(
        input[1]["encrypted_content"],
        "gAAAAABnQe9mFULL-ENCRYPTED-PAYLOAD-abcdef123456"
    );
    assert_eq!(input[2]["type"], "message");
    assert_eq!(input[2]["role"], "assistant");
    assert_eq!(input[3]["role"], "user");

    // Stateless, and asking for the encrypted content back again.
    assert_eq!(body["store"], false);
    assert_eq!(body["include"][0], "reasoning.encrypted_content");
}

#[test]
fn the_raw_payload_records_the_model_the_response_reports() {
    let (_, decoder) = decode("text_only.sse");
    assert_eq!(
        decoder.raw_payload()["model"].as_str(),
        Some("gpt-test-2026")
    );
}
