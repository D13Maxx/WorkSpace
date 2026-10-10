//! Live smoke tests against the real Anthropic API (master spec s7.6).
//!
//! Gated behind `live-tests` and never run in CI. They spend real tokens, so
//! they are deliberately small.
//!
//! Run with:
//!   cargo test -p ws-providers --features live-tests -- --nocapture
//!
//! The key is read from the OS credential store under service `ws`, account
//! `anthropic` — the same place the app puts it — falling back to the
//! ANTHROPIC_API_KEY environment variable.
#![cfg(feature = "live-tests")]
// Spec s14 permits unwrap/expect in tests.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use futures_util::StreamExt;
use std::collections::BTreeMap;
use std::sync::Arc;
use tokio_util::sync::CancellationToken;
use ws_context::{
    BackendEvent, BackendId, BackendRequest, ChatBackend, DefaultProfiles, ModelRef, Part,
    RenderedMessage, RenderedView, Role, StopReason, ThinkingMode,
};
use ws_providers::AnthropicBackend;

const BACKEND: &str = "anthropic";

/// Skip rather than fail when no key is configured: a developer without one
/// should still be able to run the suite.
fn have_key() -> bool {
    if std::env::var("ANTHROPIC_API_KEY").is_ok() {
        return true;
    }
    ws_secrets::SecretStore::new()
        .has_key(BACKEND)
        .unwrap_or(false)
}

fn model() -> ModelRef {
    ModelRef::new(
        BACKEND,
        std::env::var("WS_LIVE_MODEL").unwrap_or_else(|_| "claude-sonnet-4-5".to_owned()),
    )
}

fn backend() -> AnthropicBackend {
    AnthropicBackend::new(BackendId::new(BACKEND), Arc::new(DefaultProfiles))
}

fn request(prompt: &str, thinking: ThinkingMode, max_output: u32) -> BackendRequest {
    BackendRequest {
        model: model(),
        view: RenderedView {
            system: Some("Answer in as few words as possible.".to_owned()),
            messages: vec![RenderedMessage {
                source_id: ws_context::MsgId::new(),
                role: Role::User,
                parts: vec![Part::text(prompt)],
            }],
        },
        max_output_tokens: max_output,
        thinking,
        raw_payloads: BTreeMap::new(),
        blobs: BTreeMap::new(),
        tools: Vec::new(),
    }
}

async fn collect(req: BackendRequest) -> (String, Vec<BackendEvent>) {
    let mut stream = backend()
        .stream(req, CancellationToken::new())
        .await
        .expect("the request should start");

    let mut text = String::new();
    let mut events = Vec::new();
    while let Some(event) = stream.next().await {
        if let BackendEvent::TextDelta(t) = &event {
            text.push_str(t);
        }
        let done = matches!(event, BackendEvent::Done { .. } | BackendEvent::Error(_));
        events.push(event);
        if done {
            break;
        }
    }
    (text, events)
}

#[tokio::test]
async fn a_plain_reply_streams_from_the_real_api() {
    if !have_key() {
        eprintln!("skipping: no Anthropic key configured");
        return;
    }

    let (text, events) = collect(request("Reply with exactly: pong", ThinkingMode::Off, 64)).await;

    assert!(
        !text.trim().is_empty(),
        "the model should have said something"
    );
    assert!(
        matches!(events.last(), Some(BackendEvent::Done { .. })),
        "the stream must end with Done, got {:?}",
        events.last()
    );
    assert!(
        events
            .iter()
            .any(|e| matches!(e, BackendEvent::Usage { .. })),
        "usage should be reported"
    );
    eprintln!("live reply: {text:?}");
}

#[tokio::test]
async fn the_model_list_comes_back_from_the_real_api() {
    if !have_key() {
        eprintln!("skipping: no Anthropic key configured");
        return;
    }

    let models = backend().list_models().await.expect("list_models");
    assert!(!models.is_empty(), "the provider should advertise models");
    assert!(
        models.iter().all(|m| !m.model.model_id.is_empty()),
        "every model must have an id"
    );
    eprintln!("live models: {}", models.len());
}

#[tokio::test]
async fn a_thinking_reply_carries_a_signed_block_that_can_be_replayed() {
    // The point of the whole opaque machinery: a real signature comes back,
    // survives storage, and is accepted when sent to the same model.
    if !have_key() {
        eprintln!("skipping: no Anthropic key configured");
        return;
    }

    let req = request(
        "What is 127 * 43? Think it through.",
        ThinkingMode::Enabled {
            budget_tokens: 1024,
        },
        2048,
    );
    let (text, events) = collect(req).await;

    let opaque: Vec<_> = events
        .iter()
        .filter_map(|e| match e {
            BackendEvent::OpaqueBlock(o) => Some(o),
            _ => None,
        })
        .collect();

    if opaque.is_empty() {
        // Some models answer without thinking even when it is enabled; that is
        // the API's choice, not a failure of this adapter.
        eprintln!("note: the model returned no thinking block for this prompt");
        return;
    }

    let block = opaque[0];
    assert_eq!(block.kind, "anthropic.thinking");
    assert!(
        block
            .payload
            .get("signature")
            .and_then(|v| v.as_str())
            .is_some_and(|s| !s.is_empty()),
        "a real thinking block must carry a signature"
    );

    let raw = events
        .iter()
        .find_map(|e| match e {
            BackendEvent::RawPayload(v) => Some(v.clone()),
            _ => None,
        })
        .expect("a raw payload should be emitted before Done");

    // Replay it as a prior assistant turn and check the API accepts it. This
    // is the failure the raw-payload rule exists to prevent (master spec s5.2).
    let assistant_id = ws_context::MsgId::new();
    let mut replay = request(
        "And what is that plus one?",
        ThinkingMode::Enabled {
            budget_tokens: 1024,
        },
        2048,
    );
    replay.view.messages.insert(
        0,
        RenderedMessage {
            source_id: ws_context::MsgId::new(),
            role: Role::User,
            parts: vec![Part::text("What is 127 * 43? Think it through.")],
        },
    );
    replay.view.messages.insert(
        1,
        RenderedMessage {
            source_id: assistant_id,
            role: Role::Assistant,
            parts: vec![Part::text(text)],
        },
    );
    replay.raw_payloads.insert(assistant_id, raw);

    let (replied, events) = collect(replay).await;
    let failed = events.iter().find_map(|e| match e {
        BackendEvent::Error(err) => Some(err.clone()),
        _ => None,
    });
    assert!(
        failed.is_none(),
        "replaying a stored thinking block was rejected: {failed:?}"
    );
    assert!(!replied.trim().is_empty());
    eprintln!("replay accepted; follow-up: {replied:?}");
}

#[tokio::test]
async fn an_oversized_request_fails_with_a_readable_message() {
    if !have_key() {
        eprintln!("skipping: no Anthropic key configured");
        return;
    }

    // max_tokens far beyond any model's limit: a 4xx the adapter must surface
    // rather than retry.
    let req = request("hello", ThinkingMode::Off, 100_000_000);
    let result = backend().stream(req, CancellationToken::new()).await;

    let Err(e) = result else {
        panic!("an impossible max_tokens should have been rejected");
    };
    assert!(!e.is_retryable(), "a 4xx must not be retried");
    let message = e.to_string();
    assert!(
        message.len() > 10 && !message.contains("x-api-key"),
        "the error should be readable and must not leak the key: {message}"
    );
    eprintln!("live error surfaced as: {message}");
}

#[tokio::test]
async fn cancelling_mid_stream_stops_promptly_and_keeps_what_arrived() {
    if !have_key() {
        eprintln!("skipping: no Anthropic key configured");
        return;
    }

    let req = request(
        "Count slowly from 1 to 200, one number per line.",
        ThinkingMode::Off,
        2048,
    );
    let cancel = CancellationToken::new();
    let mut stream = backend()
        .stream(req, cancel.clone())
        .await
        .expect("the request should start");

    let mut text = String::new();
    let mut deltas = 0;
    while let Some(event) = stream.next().await {
        match event {
            BackendEvent::TextDelta(t) => {
                text.push_str(&t);
                deltas += 1;
                if deltas == 3 {
                    cancel.cancel();
                }
            }
            BackendEvent::Done { stop_reason } => {
                assert_eq!(stop_reason, StopReason::Cancelled);
                break;
            }
            _ => {}
        }
    }

    assert!(
        !text.is_empty(),
        "whatever arrived before the stop must be kept"
    );
    eprintln!("cancelled after {deltas} deltas, kept {} chars", text.len());
}
