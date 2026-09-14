//! OpenAI-compatible decoder tests over recorded streams (master spec s7.6).

// Spec s14 permits unwrap/expect in tests.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use ws_context::{BackendError, BackendEvent, BackendId, StopReason};
use ws_providers::stream::decode_body;
use ws_providers::{CompatDecoder, StreamDecoder as _};

fn fixture(name: &str) -> String {
    let path = concat!(env!("CARGO_MANIFEST_DIR"), "/../../fixtures/sse/compat/");
    std::fs::read_to_string(format!("{path}{name}"))
        .unwrap_or_else(|e| panic!("fixture {name}: {e}"))
}

fn decode_chunked(name: &str, chunk_size: usize) -> (Vec<BackendEvent>, CompatDecoder) {
    let body = fixture(name);
    let mut decoder = CompatDecoder::new(BackendId::new("local-llama"), "llama-3-8b");
    let mut events = decode_body(&body, &mut decoder, chunk_size);
    events.extend(decoder.finish());
    (events, decoder)
}

fn decode(name: &str) -> (Vec<BackendEvent>, CompatDecoder) {
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

#[test]
fn a_plain_reply_streams_and_ends_on_the_done_sentinel() {
    let (events, _) = decode("text_only.sse");

    assert_eq!(text_of(&events), "Hello, world");
    assert!(matches!(
        events.last(),
        Some(BackendEvent::Done {
            stop_reason: StopReason::EndTurn
        })
    ));
}

#[test]
fn no_opaque_block_is_ever_emitted() {
    // Master spec s7.5: this protocol has nothing signed to replay. The
    // adapter must never hand the context builder something it would then try
    // to send back to a server that has never heard of it.
    for name in [
        "text_only.sse",
        "reasoning_content.sse",
        "no_done_sentinel.sse",
        "usage_after_finish.sse",
    ] {
        let (events, _) = decode(name);
        assert!(
            !events
                .iter()
                .any(|e| matches!(e, BackendEvent::OpaqueBlock(_))),
            "{name} produced an opaque block"
        );
    }
}

#[test]
fn a_non_standard_reasoning_field_is_shown_but_not_replayable() {
    let (events, _) = decode("reasoning_content.sse");

    assert_eq!(reasoning_of(&events), "Let me work through this.");
    assert_eq!(text_of(&events), "The answer is 4.");
    assert!(
        !events
            .iter()
            .any(|e| matches!(e, BackendEvent::OpaqueBlock(_))),
        "reasoning_content carries no signature, so nothing is replayable"
    );
}

#[test]
fn a_server_that_stops_without_the_sentinel_is_treated_as_a_clean_close() {
    // Common enough among compatible servers that treating it as a protocol
    // error would reject perfectly good replies.
    let (events, _) = decode("no_done_sentinel.sse");

    assert_eq!(text_of(&events), "Terse.");
    assert!(
        matches!(
            events.last(),
            Some(BackendEvent::Done {
                stop_reason: StopReason::EndTurn
            })
        ),
        "got {:?}",
        events.last()
    );
}

#[test]
fn usage_arriving_after_the_finish_reason_is_still_counted() {
    // This is why the decoder does not finish on finish_reason: the usage
    // chunk comes afterwards, and finishing early would lose it.
    let (events, _) = decode("usage_after_finish.sse");

    let usage = events.iter().find_map(|e| match e {
        BackendEvent::Usage {
            input_tokens,
            output_tokens,
        } => Some((*input_tokens, *output_tokens)),
        _ => None,
    });
    assert_eq!(usage, Some((14, 3)), "usage after finish_reason was lost");
    assert!(matches!(
        events.last(),
        Some(BackendEvent::Done {
            stop_reason: StopReason::MaxTokens
        })
    ));
}

#[test]
fn an_error_after_text_has_streamed_is_reported_and_the_text_kept() {
    let (events, decoder) = decode("error_mid_stream.sse");

    assert_eq!(text_of(&events), "Partial answer that");
    assert!(decoder.has_started());

    let Some(BackendEvent::Error(BackendError::Http { status, .. })) = events.last() else {
        panic!("expected a trailing HTTP error, got {:?}", events.last());
    };
    assert_eq!(*status, 500);
}

#[test]
fn a_stream_that_ends_with_nothing_at_all_is_a_protocol_error() {
    let mut decoder = CompatDecoder::new(BackendId::new("local-llama"), "llama-3-8b");
    let mut events = decode_body("data: {\"choices\":[]}\n\n", &mut decoder, usize::MAX);
    events.extend(decoder.finish());

    assert!(matches!(
        events.last(),
        Some(BackendEvent::Error(BackendError::Protocol { .. }))
    ));
}

#[test]
fn decoding_is_identical_however_the_body_is_chunked() {
    for name in [
        "text_only.sse",
        "reasoning_content.sse",
        "no_done_sentinel.sse",
        "usage_after_finish.sse",
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
fn the_raw_payload_records_the_model_the_server_reports() {
    let (_, decoder) = decode("reasoning_content.sse");
    assert_eq!(
        decoder.raw_payload()["model"].as_str(),
        Some("deepseek-test"),
        "the server's own model id, not the one that was asked for"
    );
}
