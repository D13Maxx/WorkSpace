//! End-to-end turn orchestration against a scripted backend.
//!
//! Covers the C1 acceptance items that do not need a UI: a streaming reply,
//! regenerate producing a branch, history surviving a restart, cancellation and
//! failure both preserving partial text, and the summary path firing when
//! history will not fit.

// Spec s14 permits unwrap/expect in tests.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::sync::{Arc, Mutex};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;
use ws_context::{
    ContextConfig, ConvId, ModelCapabilities, ModelRef, MsgStatus, Part, StopReason, ViewMode,
};
use ws_core::testing::{FakeBackend, Scripted};
use ws_core::{Orchestrator, Profile, ProfileTable, TurnEvent};
use ws_store::{BlobStore, Store};

const BACKEND: &str = "fake";

fn model() -> ModelRef {
    ModelRef::new(BACKEND, "fake-1")
}

fn other_model() -> ModelRef {
    ModelRef::new(BACKEND, "fake-2")
}

struct Harness {
    orch: Orchestrator,
    store: Arc<Mutex<Store>>,
    backend: Arc<FakeBackend>,
    _dir: tempfile::TempDir,
}

fn harness_with(window: u32, replies: Vec<Scripted>) -> Harness {
    let dir = tempfile::tempdir().unwrap();
    let store = Arc::new(Mutex::new(Store::open(dir.path().join("ws.db")).unwrap()));
    let blobs = Arc::new(BlobStore::new(dir.path().join("blobs")).unwrap());

    let profiles = Arc::new(ProfileTable::new());
    for m in [model(), other_model()] {
        profiles.set(
            m,
            Profile {
                capabilities: ModelCapabilities {
                    context_window: window,
                    max_output_tokens: 256,
                    ..ModelCapabilities::default()
                },
                thinking: ws_context::ThinkingMode::Off,
                user_edited: true,
            },
        );
    }

    let mut backend = FakeBackend::new(BACKEND, profiles.clone() as Arc<_>);
    for r in replies {
        backend = backend.scripted(r);
    }
    let backend = Arc::new(backend);

    let orch = Orchestrator::new(store.clone(), blobs, profiles)
        .with_backend(backend.clone() as Arc<_>)
        .with_config(ContextConfig::default());

    Harness {
        orch,
        store,
        backend,
        _dir: dir,
    }
}

fn harness(replies: Vec<Scripted>) -> Harness {
    harness_with(8192, replies)
}

/// Run a turn, returning the events the UI would have seen.
async fn run(h: &Harness, conv: ConvId, cancel: CancellationToken) -> Vec<TurnEvent> {
    let (tx, mut rx) = mpsc::channel(256);
    let turn = h.orch.run_turn(conv, tx, cancel);
    let collect = async {
        let mut out = Vec::new();
        while let Some(e) = rx.recv().await {
            out.push(e);
        }
        out
    };
    let (_, events) = tokio::join!(turn, collect);
    events
}

fn text_of(events: &[TurnEvent]) -> String {
    events
        .iter()
        .filter_map(|e| match e {
            TurnEvent::TextDelta { text } => Some(text.as_str()),
            _ => None,
        })
        .collect()
}

#[tokio::test]
async fn a_turn_streams_text_and_persists_one_message() {
    let h = harness(vec![Scripted::Text(vec![
        "Hello".into(),
        ", ".into(),
        "world".into(),
    ])]);
    let conv = h.orch.create_conversation("test", model()).unwrap();
    h.orch
        .append_user_message(conv, vec![Part::text("hi")])
        .unwrap();

    let events = run(&h, conv, CancellationToken::new()).await;

    assert_eq!(text_of(&events), "Hello, world");
    assert!(matches!(
        events.last(),
        Some(TurnEvent::Completed {
            stop_reason: StopReason::EndTurn,
            ..
        })
    ));

    let s = h.store.lock().unwrap();
    let path = s.load_active_path(conv).unwrap();
    assert_eq!(path.len(), 2, "one user message, one reply");
    assert_eq!(path[1].text_content(), "Hello, world");
    assert_eq!(path[1].status, MsgStatus::Complete);
    assert_eq!(path[1].author_model.as_ref(), Some(&model()));
    // The draft must be gone: it has been superseded by a real row.
    assert!(s.list_drafts().unwrap().is_empty());
}

#[tokio::test]
async fn the_context_view_is_persisted_for_the_inspector() {
    let h = harness(vec![Scripted::Text(vec!["ok".into()])]);
    let conv = h.orch.create_conversation("test", model()).unwrap();
    h.orch
        .append_user_message(conv, vec![Part::text("hi")])
        .unwrap();

    let events = run(&h, conv, CancellationToken::new()).await;
    let Some(TurnEvent::Completed { message, .. }) = events.last() else {
        panic!("expected completion");
    };

    // Emitted before generation, so the inspector works while streaming.
    assert!(
        events
            .iter()
            .any(|e| matches!(e, TurnEvent::ContextReady { .. })),
        "the UI must learn what was sent"
    );

    let s = h.store.lock().unwrap();
    let view = s.get_context_view(*message).unwrap().unwrap();
    assert_eq!(view.mode, ViewMode::Full);
    assert_eq!(view.target, model());
    assert_eq!(view.verbatim_messages.len(), 1, "just the user message");
}

#[tokio::test]
async fn the_raw_provider_payload_is_stored_and_replayed_to_the_same_model() {
    let h = harness(vec![
        Scripted::TextWithOpaque {
            deltas: vec!["first".into()],
            opaque_payload: serde_json::json!({
                "type": "thinking", "thinking": "hmm", "signature": "sig-1"
            }),
        },
        Scripted::Text(vec!["second".into()]),
    ]);
    let conv = h.orch.create_conversation("test", model()).unwrap();
    h.orch
        .append_user_message(conv, vec![Part::text("one")])
        .unwrap();
    run(&h, conv, CancellationToken::new()).await;

    // Second turn to the same model: the stored response must be replayed.
    h.orch
        .append_user_message(conv, vec![Part::text("two")])
        .unwrap();
    run(&h, conv, CancellationToken::new()).await;

    let requests = h.backend.requests();
    assert_eq!(requests.len(), 2);
    let second = &requests[1];
    assert_eq!(
        second.raw_payloads.len(),
        1,
        "the earlier assistant turn must be replayed from its stored response"
    );
    let replayed = second.raw_payloads.values().next().unwrap();
    assert_eq!(replayed["content"][0]["signature"], "sig-1");
}

#[tokio::test]
async fn a_different_model_does_not_receive_the_other_models_signed_blocks() {
    // Master spec s6.4: replay only when the target is the producing model.
    let h = harness(vec![
        Scripted::TextWithOpaque {
            deltas: vec!["first".into()],
            opaque_payload: serde_json::json!({
                "type": "thinking", "thinking": "hmm", "signature": "sig-1"
            }),
        },
        Scripted::Text(vec!["second".into()]),
    ]);
    let conv = h.orch.create_conversation("test", model()).unwrap();
    h.orch
        .append_user_message(conv, vec![Part::text("one")])
        .unwrap();
    run(&h, conv, CancellationToken::new()).await;

    h.orch.switch_model(conv, other_model()).unwrap();
    h.orch
        .append_user_message(conv, vec![Part::text("two")])
        .unwrap();
    run(&h, conv, CancellationToken::new()).await;

    let requests = h.backend.requests();
    assert!(
        requests[1].raw_payloads.is_empty(),
        "a signed block must not be sent to a model that did not produce it"
    );
    let has_opaque = requests[1]
        .view
        .messages
        .iter()
        .flat_map(|m| &m.parts)
        .any(|p| matches!(p, Part::Opaque(_)));
    assert!(!has_opaque, "and it must not appear as canonical content");
}

#[tokio::test]
async fn a_model_switch_is_recorded_as_an_event_not_a_message() {
    let h = harness(vec![Scripted::Text(vec!["ok".into()])]);
    let conv = h.orch.create_conversation("test", model()).unwrap();
    h.orch
        .append_user_message(conv, vec![Part::text("hi")])
        .unwrap();
    h.orch.switch_model(conv, other_model()).unwrap();

    let s = h.store.lock().unwrap();
    let events = s.list_events(conv).unwrap();
    assert_eq!(events.len(), 1);
    match &events[0].kind {
        ws_context::EventKind::ModelSwitch { from, to } => {
            assert_eq!(from, &model());
            assert_eq!(to, &other_model());
        }
        other => panic!("expected a model switch, got {other:?}"),
    }
    // The path is unchanged: a switch is not a message.
    assert_eq!(s.load_active_path(conv).unwrap().len(), 1);
    assert_eq!(
        s.get_conversation(conv).unwrap().unwrap().current_model,
        other_model()
    );
}

#[tokio::test]
async fn regenerating_creates_a_sibling_and_keeps_the_original() {
    let h = harness(vec![
        Scripted::Text(vec!["first answer".into()]),
        Scripted::Text(vec!["second answer".into()]),
    ]);
    let conv = h.orch.create_conversation("test", model()).unwrap();
    h.orch
        .append_user_message(conv, vec![Part::text("question")])
        .unwrap();

    let first = run(&h, conv, CancellationToken::new()).await;
    let TurnEvent::Completed {
        message: first_id, ..
    } = first.last().unwrap().clone()
    else {
        panic!("expected completion");
    };

    h.orch.prepare_regenerate(conv, first_id).unwrap();
    let second = run(&h, conv, CancellationToken::new()).await;
    let TurnEvent::Completed {
        message: second_id, ..
    } = second.last().unwrap().clone()
    else {
        panic!("expected completion");
    };

    assert_ne!(first_id, second_id);
    let s = h.store.lock().unwrap();
    let siblings = s.siblings(first_id).unwrap();
    assert_eq!(siblings.len(), 2, "both answers survive");
    assert_eq!(siblings[0].text_content(), "first answer");
    assert_eq!(siblings[1].text_content(), "second answer");
    // The visible thread follows the newer one.
    assert_eq!(
        s.load_active_path(conv).unwrap().last().unwrap().id,
        second_id
    );
}

#[tokio::test]
async fn a_failure_partway_through_keeps_the_text_the_user_already_saw() {
    let h = harness(vec![Scripted::FailsAfter {
        deltas: vec!["partial ".into(), "answer".into()],
        error: "connection reset".into(),
    }]);
    let conv = h.orch.create_conversation("test", model()).unwrap();
    h.orch
        .append_user_message(conv, vec![Part::text("hi")])
        .unwrap();

    let events = run(&h, conv, CancellationToken::new()).await;
    assert_eq!(text_of(&events), "partial answer");

    let Some(TurnEvent::Failed { message, error }) = events.last() else {
        panic!("expected a failure, got {:?}", events.last());
    };
    assert!(error.contains("connection reset"));

    let s = h.store.lock().unwrap();
    let stored = s.get_message(message.unwrap()).unwrap().unwrap();
    assert_eq!(
        stored.text_content(),
        "partial answer",
        "nothing the user saw may be discarded"
    );
    assert!(matches!(stored.status, MsgStatus::Failed { .. }));
}

#[tokio::test]
async fn a_request_that_never_starts_reports_without_writing_a_message() {
    let h = harness(vec![Scripted::RefusesToStart {
        error: ws_context::BackendError::MissingApiKey {
            backend: BACKEND.into(),
        },
    }]);
    let conv = h.orch.create_conversation("test", model()).unwrap();
    h.orch
        .append_user_message(conv, vec![Part::text("hi")])
        .unwrap();

    let events = run(&h, conv, CancellationToken::new()).await;
    let Some(TurnEvent::Failed { message, error }) = events.last() else {
        panic!("expected a failure");
    };
    assert!(
        message.is_none(),
        "nothing streamed, so nothing to preserve"
    );
    assert!(error.contains("API key"));

    let s = h.store.lock().unwrap();
    assert_eq!(
        s.load_active_path(conv).unwrap().len(),
        1,
        "only the user message should exist"
    );
}

#[tokio::test]
async fn cancelling_before_the_turn_starts_still_persists_a_cancelled_message() {
    let h = harness(vec![Scripted::Text(vec!["never seen".into()])]);
    let conv = h.orch.create_conversation("test", model()).unwrap();
    h.orch
        .append_user_message(conv, vec![Part::text("hi")])
        .unwrap();

    let cancel = CancellationToken::new();
    cancel.cancel();
    let events = run(&h, conv, cancel).await;

    let Some(TurnEvent::Completed {
        message,
        stop_reason,
    }) = events.last()
    else {
        panic!("expected a completion, got {:?}", events.last());
    };
    assert_eq!(*stop_reason, StopReason::Cancelled);

    let s = h.store.lock().unwrap();
    let stored = s.get_message(*message).unwrap().unwrap();
    assert_eq!(stored.status, MsgStatus::Cancelled);
}

#[tokio::test]
async fn history_survives_closing_and_reopening_the_database() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("ws.db");
    let blob_dir = dir.path().join("blobs");

    let profiles = Arc::new(ProfileTable::new());
    profiles.set(
        model(),
        Profile {
            user_edited: true,
            ..Profile::default()
        },
    );

    let conv = {
        let store = Arc::new(Mutex::new(Store::open(&db).unwrap()));
        let blobs = Arc::new(BlobStore::new(&blob_dir).unwrap());
        let backend =
            Arc::new(FakeBackend::new(BACKEND, profiles.clone() as Arc<_>).says("remembered"));
        let orch =
            Orchestrator::new(store, blobs, profiles.clone()).with_backend(backend as Arc<_>);

        let conv = orch.create_conversation("persisted", model()).unwrap();
        orch.append_user_message(conv, vec![Part::text("hi")])
            .unwrap();
        let (tx, mut rx) = mpsc::channel(64);
        let run = orch.run_turn(conv, tx, CancellationToken::new());
        let drain = async { while rx.recv().await.is_some() {} };
        let (_, ()) = tokio::join!(run, drain);
        conv
    };

    // Fresh process: a new Store over the same file.
    let store = Store::open(&db).unwrap();
    let path = store.load_active_path(conv).unwrap();
    assert_eq!(path.len(), 2);
    assert_eq!(path[0].text_content(), "hi");
    assert_eq!(path[1].text_content(), "remembered");
}

#[tokio::test]
async fn an_overflowing_conversation_is_summarised_and_an_unchanged_range_reuses_it() {
    // A small window forces eviction after a handful of turns. The scripted
    // queue is generous: summariser calls draw from it too, and the fake falls
    // back to a well-formed reply when it runs dry.
    let mut replies = Vec::new();
    for i in 0..24 {
        replies.push(Scripted::Text(vec![format!(
            "reply {i} with enough words in it to consume a real number of tokens"
        )]));
    }

    let h = harness_with(1024, replies);
    let conv = h.orch.create_conversation("long", model()).unwrap();

    let mut summarising_turns = 0;
    let mut last_message = None;
    for i in 0..13 {
        h.orch
            .append_user_message(
                conv,
                vec![Part::text(format!(
                    "question {i} padded out so the window fills up before long"
                ))],
            )
            .unwrap();
        let events = run(&h, conv, CancellationToken::new()).await;
        if events
            .iter()
            .any(|e| matches!(e, TurnEvent::Summarising { .. }))
        {
            summarising_turns += 1;
        }
        if let Some(TurnEvent::Completed { message, .. }) = events.last() {
            last_message = Some(*message);
        }
    }

    assert!(
        summarising_turns > 0,
        "history should have overflowed and been summarised"
    );

    let (view, summaries_before) = {
        let s = h.store.lock().unwrap();
        let path = s.load_active_path(conv).unwrap();
        let last = path.last().unwrap();
        let view = s.get_context_view(last.id).unwrap().unwrap();
        assert_eq!(view.mode, ViewMode::SummaryPlusRecent);
        assert!(
            view.summary.is_some(),
            "a summarised view must name the summary it used"
        );
        // The latest user message is always sent verbatim (master spec s6.2).
        let latest_user = path
            .iter()
            .rposition(|m| m.role == ws_context::Role::User)
            .unwrap();
        assert!(
            view.verbatim_messages.contains(&path[latest_user].id),
            "the question being answered must never be summarised away"
        );
        (view, s.list_summaries(conv).unwrap().len())
    };
    assert!(summaries_before > 0, "summaries must be cached");

    // Regenerating answers the same question, so the evicted range is
    // unchanged and the cached summary must be reused rather than rewritten.
    h.orch
        .prepare_regenerate(conv, last_message.unwrap())
        .unwrap();
    let events = run(&h, conv, CancellationToken::new()).await;

    assert!(
        !events
            .iter()
            .any(|e| matches!(e, TurnEvent::Summarising { .. })),
        "an unchanged range must hit the summary cache, not pay for a rewrite"
    );

    let s = h.store.lock().unwrap();
    assert_eq!(
        s.list_summaries(conv).unwrap().len(),
        summaries_before,
        "no new summary should have been written"
    );
    let regenerated = s.load_active_path(conv).unwrap().last().unwrap().id;
    let reused = s.get_context_view(regenerated).unwrap().unwrap();
    assert_eq!(
        reused.summary, view.summary,
        "the same cached summary should back both answers"
    );
}

#[tokio::test]
async fn an_in_progress_reply_is_checkpointed_so_a_crash_cannot_lose_it() {
    // Master spec s10: the message row is written once, at the end. Until
    // then the partial text lives in `drafts`. Crash recovery depends on that
    // checkpoint actually happening, so this asserts it does.
    use std::time::Duration;

    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("ws.db");
    let store = Arc::new(Mutex::new(Store::open(&db).unwrap()));
    let blobs = Arc::new(BlobStore::new(dir.path().join("blobs")).unwrap());

    let profiles = Arc::new(ProfileTable::new());
    profiles.set(
        model(),
        Profile {
            user_edited: true,
            ..Profile::default()
        },
    );

    let backend = Arc::new(
        FakeBackend::new(BACKEND, profiles.clone() as Arc<_>).scripted(Scripted::SlowText {
            deltas: vec!["one ".into(), "two ".into(), "three".into()],
            delay: Duration::from_millis(40),
        }),
    );

    let orch = Orchestrator::new(store.clone(), blobs, profiles)
        .with_backend(backend as Arc<_>)
        // Checkpoint on every event, so the assertion does not wait a second.
        .with_draft_interval(Duration::ZERO);

    let conv = orch.create_conversation("crashy", model()).unwrap();
    orch.append_user_message(conv, vec![Part::text("hi")])
        .unwrap();

    let (tx, mut rx) = mpsc::channel(64);
    let turn = orch.run_turn(conv, tx, CancellationToken::new());

    // Watch for a draft while the reply is still arriving.
    let watcher = {
        let store = store.clone();
        async move {
            let mut seen: Option<String> = None;
            while let Some(event) = rx.recv().await {
                if matches!(event, TurnEvent::TextDelta { .. }) {
                    if let Ok(s) = store.lock() {
                        if let Ok(drafts) = s.list_drafts() {
                            if let Some(d) = drafts.first() {
                                seen = Some(d.partial_text.clone());
                            }
                        }
                    }
                }
            }
            seen
        }
    };

    let (result, mid_flight) = tokio::join!(turn, watcher);
    result.unwrap();

    let partial = mid_flight.expect("a draft should exist while the reply streams");
    assert!(
        !partial.is_empty() && "one two three".starts_with(&partial),
        "the draft should hold a prefix of the reply, got {partial:?}"
    );

    // Once the row is written, the draft is gone, so a later launch does not
    // resurrect a reply that actually finished.
    let s = store.lock().unwrap();
    assert!(s.list_drafts().unwrap().is_empty());
    assert_eq!(
        s.load_active_path(conv)
            .unwrap()
            .last()
            .unwrap()
            .text_content(),
        "one two three"
    );
}

#[tokio::test]
async fn a_reply_interrupted_by_a_crash_comes_back_as_a_failed_message() {
    // Simulates the C1 acceptance item: kill the process mid-stream, relaunch,
    // and find the partial text preserved rather than lost.
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("ws.db");

    let conv = {
        let store = Store::open(&db).unwrap();
        let conv = ws_context::ConvId::new();
        store
            .create_conversation(&ws_context::Conversation {
                id: conv,
                title: "interrupted".to_owned(),
                system_prompt: None,
                active_leaf_id: None,
                current_model: model(),
                created_at: chrono::Utc::now(),
            })
            .unwrap();
        // A checkpoint with no message row: exactly what a crash leaves.
        store
            .save_draft(&ws_store::Draft {
                conversation_id: conv,
                parent_id: None,
                model: model(),
                partial_text: "half a thought".to_owned(),
                updated_at: chrono::Utc::now(),
            })
            .unwrap();
        conv
    };

    // Relaunch: a fresh Store over the same file, then recovery.
    let store = Store::open(&db).unwrap();
    let recovered = store.recover_drafts().unwrap();
    assert_eq!(recovered.len(), 1);

    let path = store.load_active_path(conv).unwrap();
    let last = path.last().unwrap();
    assert_eq!(last.text_content(), "half a thought");
    assert!(
        matches!(last.status, MsgStatus::Failed { .. }),
        "an interrupted reply must be marked failed, not presented as complete"
    );
    assert!(store.list_drafts().unwrap().is_empty());
}

#[tokio::test]
async fn compaction_is_occasional_rather_than_once_per_turn() {
    // Before the compaction trigger existed, a long conversation summarised on
    // every single turn: each new exchange pushed it back over the limit, so
    // the evicted range grew and a fresh summary was written each time. The
    // trigger/target gap is what turns that into an occasional cost.
    let mut replies = Vec::new();
    for i in 0..60 {
        replies.push(Scripted::Text(vec![format!(
            "reply {i} with enough words in it to consume a real number of tokens"
        )]));
    }

    let h = harness_with(1024, replies);
    let conv = h.orch.create_conversation("long", model()).unwrap();

    let turns = 24;
    let mut compactions = 0;
    let mut compacted_turns = Vec::new();

    for i in 0..turns {
        h.orch
            .append_user_message(
                conv,
                vec![Part::text(format!(
                    "question {i} padded out so the window fills up before long"
                ))],
            )
            .unwrap();
        let events = run(&h, conv, CancellationToken::new()).await;
        if events
            .iter()
            .any(|e| matches!(e, TurnEvent::Summarising { .. }))
        {
            compactions += 1;
            compacted_turns.push(i);
        }
    }

    eprintln!("compacted on {compactions} of {turns} turns: {compacted_turns:?}");
    assert!(
        compactions > 0,
        "a conversation this long must compact at least once"
    );
    // The real assertion: compaction is the exception, not the rule. Without
    // headroom this would be one per turn once the window filled.
    assert!(
        compactions * 3 <= turns,
        "compacted on {compactions} of {turns} turns ({compacted_turns:?}); \
         the trigger/target gap should make this occasional"
    );

    // And no consecutive pair, which is the specific pathology being fixed.
    for pair in compacted_turns.windows(2) {
        assert!(
            pair[1] - pair[0] > 1,
            "compacted on back-to-back turns {} and {}",
            pair[0],
            pair[1]
        );
    }
}

#[tokio::test]
async fn complete_answers_one_question_and_stores_nothing() {
    let h = harness(vec![Scripted::Text(vec![
        "{\"name\":".into(),
        "\"Cache\"}".into(),
    ])]);
    let text = h
        .orch
        .complete(
            &model(),
            "Answer in JSON.".into(),
            "A Redis cache".into(),
            256,
            &CancellationToken::new(),
        )
        .await
        .unwrap();
    assert_eq!(text, "{\"name\":\"Cache\"}");
    let sent = &h.backend.requests()[0];
    assert_eq!(sent.view.system.as_deref(), Some("Answer in JSON."));
    assert_eq!(sent.view.messages.len(), 1);
    assert!(
        h.store
            .lock()
            .unwrap()
            .list_conversations()
            .unwrap()
            .is_empty()
    );
}
