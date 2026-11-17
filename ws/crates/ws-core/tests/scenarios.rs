//! Checkpoint C2 scenarios S1 and S2.
//!
//! These are the tests the whole opaque-replay design exists for. They run
//! against scripted backends rather than live APIs: what is being checked is
//! the *decision* — which provider-native blocks go back to whom — and that is
//! this app's logic, not the provider's. The live counterpart needs real keys
//! and is recorded separately in the checkpoint notes.

// Tests may unwrap and expect: a panic is the failure report.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::sync::{Arc, Mutex};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;
use ws_context::{
    ContextConfig, ConvId, ModelCapabilities, ModelRef, Part, ProviderKind, ThinkingMode, ViewMode,
};
use ws_core::testing::{FakeBackend, Scripted};
use ws_core::{Orchestrator, Profile, ProfileTable, TurnEvent};
use ws_store::{BlobStore, Store};

const ANTHROPIC: &str = "anthropic";
const OPENAI: &str = "openai";
const GEMINI: &str = "gemini";
const COMPAT: &str = "local-llama";

struct World {
    orch: Orchestrator,
    store: Arc<Mutex<Store>>,
    backends: Vec<Arc<FakeBackend>>,
    _dir: tempfile::TempDir,
}

fn thinking_reply(text: &str, signature: &str) -> Scripted {
    Scripted::TextWithOpaque {
        deltas: vec![text.to_owned()],
        opaque_payload: serde_json::json!({
            "type": "thinking",
            "thinking": "some reasoning",
            "signature": signature,
        }),
    }
}

/// Four backends, each impersonating a different provider, each scripted with
/// the replies it should give in order.
fn world(window: u32, scripts: Vec<(&str, ProviderKind, Vec<Scripted>)>) -> World {
    let dir = tempfile::tempdir().unwrap();
    let store = Arc::new(Mutex::new(Store::open(dir.path().join("ws.db")).unwrap()));
    let blobs = Arc::new(BlobStore::new(dir.path().join("blobs")).unwrap());
    let profiles = Arc::new(ProfileTable::new());

    let mut orch = Orchestrator::new(store.clone(), blobs, profiles.clone())
        .with_config(ContextConfig::default());
    let mut backends = Vec::new();

    for (id, provider, replies) in scripts {
        profiles.set(
            ModelRef::new(id, format!("{id}-model")),
            Profile {
                capabilities: ModelCapabilities {
                    context_window: window,
                    max_output_tokens: 256,
                    ..ModelCapabilities::default()
                },
                thinking: ThinkingMode::Adaptive,
                user_edited: true,
            },
        );

        let mut b = FakeBackend::new(id, profiles.clone() as Arc<_>).as_provider(provider);
        for r in replies {
            b = b.scripted(r);
        }
        let b = Arc::new(b);
        backends.push(b.clone());
        orch = orch.with_backend(b as Arc<_>);
    }

    World {
        orch,
        store,
        backends,
        _dir: dir,
    }
}

fn model_for(id: &str) -> ModelRef {
    ModelRef::new(id, format!("{id}-model"))
}

async fn turn(w: &World, conv: ConvId, text: &str) -> Vec<TurnEvent> {
    w.orch
        .append_user_message(conv, vec![Part::text(text)])
        .unwrap();
    let (tx, mut rx) = mpsc::channel(256);
    let run = w.orch.run_turn(conv, tx, CancellationToken::new());
    let collect = async {
        let mut out = Vec::new();
        while let Some(e) = rx.recv().await {
            out.push(e);
        }
        out
    };
    let (result, events) = tokio::join!(run, collect);
    result.unwrap_or_else(|e| panic!("turn failed: {e}"));
    events
}

fn view_of(events: &[TurnEvent]) -> ws_context::ContextView {
    events
        .iter()
        .find_map(|e| match e {
            TurnEvent::ContextReady { view } => Some((**view).clone()),
            _ => None,
        })
        .expect("every turn reports what it sent")
}

fn failures(events: &[TurnEvent]) -> Vec<String> {
    events
        .iter()
        .filter_map(|e| match e {
            TurnEvent::Failed { error, .. } => Some(error.clone()),
            _ => None,
        })
        .collect()
}

/// **Scenario S1**.
///
/// One conversation walks Anthropic (thinking on) → OpenAI → Gemini →
/// OpenAI-compatible → back to the *same* Anthropic model. Nothing may fail,
/// and at each step the inspector must show the right replay decision.
#[tokio::test]
async fn s1_a_conversation_crosses_every_provider_and_comes_back() {
    let w = world(
        32_000,
        vec![
            (
                ANTHROPIC,
                ProviderKind::Anthropic,
                vec![
                    thinking_reply("Anthropic first.", "sig-anthropic-1"),
                    thinking_reply("Anthropic again.", "sig-anthropic-2"),
                ],
            ),
            (
                OPENAI,
                ProviderKind::OpenAi,
                vec![thinking_reply("OpenAI here.", "enc-openai-1")],
            ),
            (
                GEMINI,
                ProviderKind::Gemini,
                vec![thinking_reply("Gemini here.", "sig-gemini-1")],
            ),
            (
                COMPAT,
                ProviderKind::OpenAiCompat,
                vec![Scripted::Text(vec!["Local here.".into()])],
            ),
        ],
    );

    let conv = w
        .orch
        .create_conversation("S1", model_for(ANTHROPIC))
        .unwrap();

    // 1. Anthropic, thinking on. Nothing earlier to replay.
    let e1 = turn(&w, conv, "first question").await;
    assert!(failures(&e1).is_empty(), "{:?}", failures(&e1));
    let v1 = view_of(&e1);
    assert_eq!(v1.mode, ViewMode::Full);
    assert!(
        !v1.opaque_replayed,
        "the first turn has nothing to replay yet"
    );

    // 2. Switch to OpenAI. Anthropic's signed block must NOT go with it.
    w.orch.switch_model(conv, model_for(OPENAI)).unwrap();
    let e2 = turn(&w, conv, "second question").await;
    assert!(failures(&e2).is_empty(), "{:?}", failures(&e2));
    let v2 = view_of(&e2);
    assert!(
        !v2.opaque_replayed,
        "a signed Anthropic block must never reach OpenAI"
    );
    assert!(
        w.backends[1].requests()[0].raw_payloads.is_empty(),
        "and no raw payload either"
    );

    // 3. Gemini. Neither of the previous two providers' blocks may travel.
    w.orch.switch_model(conv, model_for(GEMINI)).unwrap();
    let e3 = turn(&w, conv, "third question").await;
    assert!(failures(&e3).is_empty(), "{:?}", failures(&e3));
    assert!(!view_of(&e3).opaque_replayed);
    assert!(w.backends[2].requests()[0].raw_payloads.is_empty());

    // 4. OpenAI-compatible, which can replay nothing at all.
    w.orch.switch_model(conv, model_for(COMPAT)).unwrap();
    let e4 = turn(&w, conv, "fourth question").await;
    assert!(failures(&e4).is_empty(), "{:?}", failures(&e4));
    assert!(!view_of(&e4).opaque_replayed);
    assert!(w.backends[3].requests()[0].raw_payloads.is_empty());

    // 5. Back to the *same* Anthropic model. Now its own blocks come back.
    w.orch.switch_model(conv, model_for(ANTHROPIC)).unwrap();
    let e5 = turn(&w, conv, "fifth question").await;
    assert!(failures(&e5).is_empty(), "{:?}", failures(&e5));
    let v5 = view_of(&e5);
    assert_eq!(v5.mode, ViewMode::Full, "replay requires a full view");
    assert!(
        v5.opaque_replayed,
        "returning to the producing model must restore its own signed blocks"
    );

    let anthropic_second = &w.backends[0].requests()[1];
    assert_eq!(
        anthropic_second.raw_payloads.len(),
        1,
        "exactly the one Anthropic turn is replayed"
    );
    let replayed = anthropic_second.raw_payloads.values().next().unwrap();
    assert_eq!(replayed["content"][0]["signature"], "sig-anthropic-1");

    // No other provider's signature may appear anywhere in that request.
    let sent =
        serde_json::to_string(&anthropic_second.raw_payloads.values().collect::<Vec<_>>()).unwrap();
    for foreign in ["enc-openai-1", "sig-gemini-1"] {
        assert!(
            !sent.contains(foreign),
            "{foreign} leaked into an Anthropic request"
        );
    }

    // And the whole walk is recorded as events, not as messages.
    let s = w.store.lock().unwrap();
    assert_eq!(
        s.list_events(conv).unwrap().len(),
        4,
        "four switches, each an event"
    );
    let path = s.load_active_path(conv).unwrap();
    assert_eq!(path.len(), 10, "five questions and five replies");
}

/// **Scenario S2**.
///
/// A long conversation with a unique fact planted early is switched to a model
/// with a small window. The outgoing model writes the summary, and the fact
/// must survive into what the new model is sent.
#[tokio::test]
async fn s2_a_fact_planted_early_survives_the_switch_to_a_smaller_window() {
    const FACT: &str = "the launch code is HYACINTH-7741";

    // The summariser is the outgoing model, so its scripted reply is what the
    // summary will contain. It carries the fact through.
    let mut big_replies = vec![Scripted::Text(vec![format!("Noted: {FACT}")])];
    for i in 0..14 {
        big_replies.push(Scripted::Text(vec![format!(
            "reply {i} about something else entirely. {}",
            "It is padded out at length so that the conversation genuinely              outgrows a small context window rather than merely approaching it."
        )]));
    }
    big_replies.push(Scripted::Text(vec![format!(
        "- The user stated that {FACT}\n- Several unrelated questions followed"
    )]));

    let w = world(
        32_000,
        vec![
            (ANTHROPIC, ProviderKind::Anthropic, big_replies),
            (
                OPENAI,
                ProviderKind::OpenAi,
                vec![Scripted::Text(vec![
                    "Your launch code is HYACINTH-7741.".into(),
                ])],
            ),
        ],
    );

    // The small-window model: 2k, far below the conversation's size.
    w.orch.profiles().set(
        model_for(OPENAI),
        Profile {
            capabilities: ModelCapabilities {
                context_window: 2_048,
                max_output_tokens: 256,
                ..ModelCapabilities::default()
            },
            thinking: ThinkingMode::Off,
            user_edited: true,
        },
    );

    let conv = w
        .orch
        .create_conversation("S2", model_for(ANTHROPIC))
        .unwrap();

    // Plant the fact in the first message, then bury it under many turns.
    turn(&w, conv, &format!("Remember this: {FACT}")).await;
    for i in 0..14 {
        turn(
            &w,
            conv,
            &format!(
                "question {i} about an unrelated topic. {}",
                "This one is deliberately long so that fifteen exchanges add up                  to more than a two-thousand token window can hold."
            ),
        )
        .await;
    }

    // Switch to the small window. The outgoing model should write the summary.
    w.orch.switch_model(conv, model_for(OPENAI)).unwrap();
    let events = turn(&w, conv, "What was the launch code I gave you?").await;
    assert!(failures(&events).is_empty(), "{:?}", failures(&events));

    let summarising = events.iter().find_map(|e| match e {
        TurnEvent::Summarising { writer, .. } => Some(writer.clone()),
        _ => None,
    });
    assert_eq!(
        summarising,
        Some(model_for(ANTHROPIC)),
        "the outgoing model is preferred, since it still holds the context"
    );

    let view = view_of(&events);
    assert_eq!(view.mode, ViewMode::SummaryPlusRecent);
    assert!(view.summary.is_some());

    // The decisive check: the fact reached the new model, via the summary.
    let request = w.backends[1].requests().last().unwrap().clone();
    let system = request.view.system.clone().unwrap_or_default();
    assert!(
        system.contains("HYACINTH-7741"),
        "the planted fact must survive into what the new model is sent; \
         system prompt was: {system}"
    );

    // And the original message is no longer sent verbatim, so the summary is
    // genuinely the only route the fact took.
    let verbatim_text: String = request
        .view
        .messages
        .iter()
        .flat_map(|m| &m.parts)
        .filter_map(Part::as_text)
        .collect();
    assert!(
        !verbatim_text.contains("Remember this"),
        "the planting message should have been evicted, or this proves nothing"
    );

    let s = w.store.lock().unwrap();
    let summary = &s.list_summaries(conv).unwrap()[0];
    assert_eq!(summary.written_by, model_for(ANTHROPIC));
    assert!(summary.text.contains("HYACINTH-7741"));
}
