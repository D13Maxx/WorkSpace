//! Decoder tests over recorded streams (master spec s7.6).
//!
//! These run the real SSE parser and the real decoder against raw
//! `text/event-stream` bodies, so the whole protocol path is covered without
//! touching the network.

// Spec s14 permits unwrap/expect in tests.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use ws_context::{BackendError, BackendEvent, BackendId, ProviderKind, StopReason};
use ws_providers::stream::decode_body;
use ws_providers::{AnthropicDecoder, SseParser, StreamDecoder as _};

fn fixture(name: &str) -> String {
    let path = concat!(env!("CARGO_MANIFEST_DIR"), "/../../fixtures/sse/anthropic/");
    std::fs::read_to_string(format!("{path}{name}"))
        .unwrap_or_else(|e| panic!("fixture {name}: {e}"))
}

/// Decode a whole fixture, feeding it in `chunk_size`-byte pieces so chunk
/// boundaries are exercised too.
fn decode_chunked(name: &str, chunk_size: usize) -> (Vec<BackendEvent>, AnthropicDecoder) {
    let body = fixture(name);
    let mut decoder = AnthropicDecoder::new(BackendId::new("anthropic"), "claude-test-20260101");
    let events = decode_body(&body, &mut decoder, chunk_size);
    (events, decoder)
}

fn decode(name: &str) -> (Vec<BackendEvent>, AnthropicDecoder) {
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
    assert!(
        opaque_of(&events).is_empty(),
        "no thinking, no opaque blocks"
    );
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
fn a_thinking_block_is_emitted_once_complete_with_its_signature() {
    let (events, _) = decode("thinking_with_signature.sse");

    assert_eq!(reasoning_of(&events), "Let me work through this.");
    assert_eq!(text_of(&events), "The answer is 4.");

    let opaque = opaque_of(&events);
    assert_eq!(opaque.len(), 1, "one thinking block");
    let block = opaque[0];
    assert_eq!(block.provider, ProviderKind::Anthropic);
    assert_eq!(block.kind, "anthropic.thinking");
    assert_eq!(block.position, 0);

    // The signature arrives in its own delta; it must be present in the block.
    assert_eq!(
        block.payload.get("signature").and_then(|v| v.as_str()),
        Some("EqQBCgIYAhIM1gbcDa9GJwZA2b3hGgxBXFDPXmYKUEBzNDoiQF6mXkQ=")
    );
    // And the thinking text must be the whole accumulation, not one delta.
    assert_eq!(
        block.payload.get("thinking").and_then(|v| v.as_str()),
        Some("Let me work through this.")
    );
}

#[test]
fn the_opaque_block_arrives_only_after_the_block_closes() {
    let (events, _) = decode("thinking_with_signature.sse");

    let first_opaque = events
        .iter()
        .position(|e| matches!(e, BackendEvent::OpaqueBlock(_)))
        .expect("an opaque block");
    let last_reasoning = events
        .iter()
        .rposition(|e| matches!(e, BackendEvent::ReasoningDelta(_)))
        .expect("reasoning deltas");

    assert!(
        first_opaque > last_reasoning,
        "the block must be emitted whole, after its final delta, because the \
         signature only arrives at the end"
    );
}

#[test]
fn a_redacted_thinking_block_is_carried_verbatim() {
    let (events, decoder) = decode("redacted_thinking.sse");

    let opaque = opaque_of(&events);
    assert_eq!(opaque.len(), 1);
    assert_eq!(opaque[0].kind, "anthropic.redacted_thinking");
    assert_eq!(
        opaque[0].payload.get("data").and_then(|v| v.as_str()),
        Some("EmwKAhgBEgy3va3pzix/LafPsn4aDFIT2Xlxh0L5L8rLVyIwxtE3rAFYGEqgxXg")
    );
    // No reasoning text is invented for a block whose content is withheld.
    assert_eq!(reasoning_of(&events), "");
    assert_eq!(text_of(&events), "Here is my reply.");

    // It must also survive into the stored raw payload.
    let raw = decoder.raw_payload();
    let content = raw.get("content").and_then(|v| v.as_array()).unwrap();
    assert_eq!(content.len(), 2, "redacted block plus text block");
    assert_eq!(
        content[0].get("type").and_then(|v| v.as_str()),
        Some("redacted_thinking")
    );
}

#[test]
fn an_unrecognised_block_type_is_preserved_rather_than_dropped() {
    // Master spec s7.2: never filter content blocks by type when storing raw
    // payloads. A build that drops what it does not understand will send back
    // an incomplete reply and be rejected.
    let (events, decoder) = decode("unknown_block_type.sse");

    assert_eq!(text_of(&events), "Still works.");
    let raw = decoder.raw_payload();
    let content = raw.get("content").and_then(|v| v.as_array()).unwrap();
    assert_eq!(content.len(), 2);
    assert_eq!(
        content[0].get("type").and_then(|v| v.as_str()),
        Some("some_future_block"),
        "the unknown block must still be in the raw payload"
    );
    assert_eq!(
        content[0].get("payload").and_then(|v| v.as_str()),
        Some("opaque-to-this-build")
    );
}

#[test]
fn an_error_after_text_has_streamed_is_reported_and_not_retryable() {
    let (events, decoder) = decode("error_mid_stream.sse");

    assert_eq!(text_of(&events), "Partial answer that");
    assert!(
        decoder.has_started(),
        "content was emitted, so a retry would duplicate it"
    );

    let Some(BackendEvent::Error(err)) = events.last() else {
        panic!("expected a trailing error, got {:?}", events.last());
    };
    match err {
        BackendError::Http { status, .. } => assert_eq!(*status, 529),
        other => panic!("expected an HTTP error, got {other:?}"),
    }
}

#[test]
fn a_stream_cut_short_reports_a_protocol_error_rather_than_completing() {
    let body = fixture("text_only.sse");
    // Cut the body before message_stop arrives.
    let truncated = &body[..body.find("event: message_delta").unwrap()];

    let mut parser = SseParser::new();
    let mut decoder = AnthropicDecoder::new(BackendId::new("anthropic"), "claude-test-20260101");
    let mut events = Vec::new();
    for sse in parser.push(truncated) {
        events.extend(decoder.on_event(&sse));
    }
    for sse in parser.finish() {
        events.extend(decoder.on_event(&sse));
    }
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
        "redacted_thinking.sse",
        "unknown_block_type.sse",
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

#[test]
fn the_raw_payload_records_the_model_the_response_reports() {
    // Replay is keyed to the concrete model id, not the alias that was asked
    // for, so an alias like "-latest" cannot silently key a replay wrong.
    let (_, decoder) = decode("text_only.sse");
    let raw = decoder.raw_payload();
    assert_eq!(
        raw.get("model").and_then(|v| v.as_str()),
        Some("claude-test-20260101")
    );
}
