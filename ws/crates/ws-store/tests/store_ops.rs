//! Conversation, branching, draft recovery and round-trip persistence.

// Spec s14 permits unwrap/expect in tests, where a panic is the failure report.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use chrono::Utc;
use ws_context::{
    ContextSummary, ContextView, ConvId, Conversation, Message, ModelRef, MsgId, MsgStatus, Part,
    ProviderKind, ProviderOpaque, Role, SummaryId, ViewMode,
};
use ws_store::{Draft, NewMessage, Store};

fn model() -> ModelRef {
    ModelRef::new("anthropic", "claude-test-1")
}

fn new_conversation(store: &Store) -> ConvId {
    let conv = Conversation {
        id: ConvId::new(),
        title: "test".to_owned(),
        system_prompt: Some("be careful".to_owned()),
        active_leaf_id: None,
        current_model: model(),
        created_at: Utc::now(),
    };
    store.create_conversation(&conv).unwrap();
    conv.id
}

fn add(store: &Store, conv: ConvId, parent: Option<MsgId>, role: Role, text: &str) -> MsgId {
    let id = MsgId::new();
    let message = Message {
        id,
        conversation_id: conv,
        parent_id: parent,
        role,
        author_model: (role == Role::Assistant).then(model),
        parts: vec![Part::text(text)],
        created_at: Utc::now(),
        status: MsgStatus::Complete,
    };
    store
        .insert_message(&NewMessage {
            message,
            raw_provider_payload: None,
        })
        .unwrap();
    store.set_active_leaf(conv, Some(id)).unwrap();
    id
}

#[test]
fn a_conversation_round_trips() {
    let store = Store::open_in_memory().unwrap();
    let id = new_conversation(&store);

    let got = store.get_conversation(id).unwrap().unwrap();
    assert_eq!(got.id, id);
    assert_eq!(got.system_prompt.as_deref(), Some("be careful"));
    assert_eq!(got.current_model, model());
}

#[test]
fn the_active_path_runs_from_root_to_leaf_in_order() {
    let store = Store::open_in_memory().unwrap();
    let conv = new_conversation(&store);

    let m1 = add(&store, conv, None, Role::User, "one");
    let m2 = add(&store, conv, Some(m1), Role::Assistant, "two");
    let m3 = add(&store, conv, Some(m2), Role::User, "three");

    let path = store.load_active_path(conv).unwrap();
    let ids: Vec<MsgId> = path.iter().map(|m| m.id).collect();
    assert_eq!(ids, vec![m1, m2, m3]);
    assert_eq!(path[0].text_content(), "one");
    assert_eq!(path[2].text_content(), "three");
}

#[test]
fn regenerating_creates_a_sibling_and_leaves_the_original_intact() {
    let store = Store::open_in_memory().unwrap();
    let conv = new_conversation(&store);

    let user = add(&store, conv, None, Role::User, "question");
    let first = add(&store, conv, Some(user), Role::Assistant, "first answer");
    // Regenerate: a new child of the same parent, not an overwrite.
    let second = add(&store, conv, Some(user), Role::Assistant, "second answer");

    let siblings = store.siblings(first).unwrap();
    assert_eq!(siblings.len(), 2, "both answers must survive");
    // Insertion order, which is what the branch navigator shows. Not id
    // order: both ids are usually minted in the same millisecond, where a
    // ULID's remaining bits are random.
    assert_eq!(siblings[0].id, first);
    assert_eq!(siblings[1].id, second);

    // The visible thread follows the active leaf.
    let path = store.load_active_path(conv).unwrap();
    assert_eq!(path.last().unwrap().id, second);

    // Switching back shows the original branch, unchanged.
    store.set_active_leaf(conv, Some(first)).unwrap();
    let path = store.load_active_path(conv).unwrap();
    assert_eq!(path.last().unwrap().id, first);
    assert_eq!(path.last().unwrap().text_content(), "first answer");
}

#[test]
fn a_branch_does_not_leak_into_the_other_branchs_path() {
    let store = Store::open_in_memory().unwrap();
    let conv = new_conversation(&store);

    let user = add(&store, conv, None, Role::User, "q");
    let a = add(&store, conv, Some(user), Role::Assistant, "branch A");
    let b = add(&store, conv, Some(user), Role::Assistant, "branch B");
    let a_child = add(&store, conv, Some(a), Role::User, "follow-up on A");

    let path_a = store.load_path(a_child).unwrap();
    let ids: Vec<MsgId> = path_a.iter().map(|m| m.id).collect();
    assert_eq!(ids, vec![user, a, a_child]);
    assert!(!ids.contains(&b), "branch B must not appear on branch A");
}

#[test]
fn provider_native_blocks_survive_a_round_trip_byte_for_byte() {
    let store = Store::open_in_memory().unwrap();
    let conv = new_conversation(&store);

    let payload = serde_json::json!({
        "type": "thinking",
        "thinking": "step one",
        "signature": "sig-xyz-==",
    });
    let id = MsgId::new();
    let message = Message {
        id,
        conversation_id: conv,
        parent_id: None,
        role: Role::Assistant,
        author_model: Some(model()),
        parts: vec![
            Part::Opaque(ProviderOpaque {
                provider: ProviderKind::Anthropic,
                model_id: model().model_id,
                kind: "anthropic.thinking".to_owned(),
                position: 0,
                payload: payload.clone(),
            }),
            Part::text("the answer"),
        ],
        created_at: Utc::now(),
        status: MsgStatus::Complete,
    };
    let raw = serde_json::json!({"content": [payload.clone()]});
    store
        .insert_message(&NewMessage {
            message: message.clone(),
            raw_provider_payload: Some(raw.clone()),
        })
        .unwrap();

    let got = store.get_message(id).unwrap().unwrap();
    assert_eq!(
        got, message,
        "the canonical message must round-trip exactly"
    );

    let opaque: Vec<_> = got.opaque_parts().collect();
    assert_eq!(opaque.len(), 1);
    assert_eq!(opaque[0].payload, payload);

    // The raw payload is stored separately and verbatim, because rebuilding it
    // from parts risks 4xx errors on replay (master spec s5.2).
    assert_eq!(store.get_raw_payload(id).unwrap(), Some(raw));
}

#[test]
fn a_failed_message_keeps_its_reason_and_partial_text() {
    let store = Store::open_in_memory().unwrap();
    let conv = new_conversation(&store);

    let id = MsgId::new();
    store
        .insert_message(&NewMessage {
            message: Message {
                id,
                conversation_id: conv,
                parent_id: None,
                role: Role::Assistant,
                author_model: Some(model()),
                parts: vec![Part::text("partial ans")],
                created_at: Utc::now(),
                status: MsgStatus::Failed {
                    reason: "connection reset".to_owned(),
                },
            },
            raw_provider_payload: None,
        })
        .unwrap();

    let got = store.get_message(id).unwrap().unwrap();
    assert_eq!(
        got.status,
        MsgStatus::Failed {
            reason: "connection reset".to_owned()
        }
    );
    assert_eq!(got.text_content(), "partial ans");
}

#[test]
fn a_leftover_draft_becomes_a_failed_message_with_its_partial_text() {
    let store = Store::open_in_memory().unwrap();
    let conv = new_conversation(&store);
    let user = add(&store, conv, None, Role::User, "question");

    // Simulates a crash mid-stream: the draft was checkpointed, the message
    // row never written.
    store
        .save_draft(&Draft {
            conversation_id: conv,
            parent_id: Some(user),
            model: model(),
            partial_text: "half an ans".to_owned(),
            updated_at: Utc::now(),
        })
        .unwrap();

    let recovered = store.recover_drafts().unwrap();
    assert_eq!(recovered.len(), 1);

    let msg = store.get_message(recovered[0]).unwrap().unwrap();
    assert_eq!(msg.text_content(), "half an ans");
    assert!(matches!(msg.status, MsgStatus::Failed { .. }));
    assert_eq!(msg.parent_id, Some(user));

    // It is on the visible thread, not hidden on an unreachable branch.
    let path = store.load_active_path(conv).unwrap();
    assert_eq!(path.last().unwrap().id, recovered[0]);

    // And the draft is gone, so a second launch does not duplicate it.
    assert!(store.list_drafts().unwrap().is_empty());
    assert!(store.recover_drafts().unwrap().is_empty());
}

#[test]
fn checkpointing_a_draft_repeatedly_keeps_only_the_latest() {
    let store = Store::open_in_memory().unwrap();
    let conv = new_conversation(&store);

    for text in ["a", "ab", "abc"] {
        store
            .save_draft(&Draft {
                conversation_id: conv,
                parent_id: None,
                model: model(),
                partial_text: text.to_owned(),
                updated_at: Utc::now(),
            })
            .unwrap();
    }

    let drafts = store.list_drafts().unwrap();
    assert_eq!(drafts.len(), 1, "one draft per conversation");
    assert_eq!(drafts[0].partial_text, "abc");
}

#[test]
fn a_context_view_round_trips_for_the_inspector() {
    let store = Store::open_in_memory().unwrap();
    let conv = new_conversation(&store);
    let msg = add(&store, conv, None, Role::Assistant, "answer");

    let view = ContextView {
        target: model(),
        mode: ViewMode::SummaryPlusRecent,
        summary: Some(SummaryId::new()),
        verbatim_messages: vec![msg],
        transformations: Vec::new(),
        opaque_replayed: false,
        estimated_input_tokens: 1234,
    };
    store.save_context_view(msg, &view).unwrap();

    assert_eq!(store.get_context_view(msg).unwrap(), Some(view));
}

#[test]
fn summaries_round_trip_and_are_scoped_to_their_conversation() {
    let store = Store::open_in_memory().unwrap();
    let conv_a = new_conversation(&store);
    let conv_b = new_conversation(&store);
    let m1 = add(&store, conv_a, None, Role::User, "one");
    let m2 = add(&store, conv_a, Some(m1), Role::Assistant, "two");

    let summary = ContextSummary {
        id: SummaryId::new(),
        conversation_id: conv_a,
        branch_leaf_at_creation: m2,
        covers_from: m1,
        covers_to: m2,
        text: "- the user asked a question".to_owned(),
        written_by: model(),
        prompt_version: ws_context::SUMMARY_PROMPT_VERSION,
        token_estimate: 42,
        created_at: Utc::now(),
    };
    store.save_summary(&summary).unwrap();

    assert_eq!(store.list_summaries(conv_a).unwrap(), vec![summary]);
    assert!(store.list_summaries(conv_b).unwrap().is_empty());
}

#[test]
fn a_reopened_database_still_has_its_history() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("ws.db");

    let (conv, m1) = {
        let store = Store::open(&path).unwrap();
        let conv = new_conversation(&store);
        let m1 = add(&store, conv, None, Role::User, "persisted");
        (conv, m1)
    };

    let store = Store::open(&path).unwrap();
    let path_msgs = store.load_active_path(conv).unwrap();
    assert_eq!(path_msgs.len(), 1);
    assert_eq!(path_msgs[0].id, m1);
    assert_eq!(path_msgs[0].text_content(), "persisted");
}

#[test]
fn siblings_keep_insertion_order_even_when_created_in_the_same_millisecond() {
    // Regression: ordering siblings by id was intermittently wrong. A ULID
    // carries a millisecond timestamp plus 80 random bits, so ids minted in
    // the same millisecond -- the normal case for rapid regeneration -- sort
    // randomly against each other.
    let store = Store::open_in_memory().unwrap();
    let conv = new_conversation(&store);
    let user = add(&store, conv, None, Role::User, "question");

    let expected: Vec<MsgId> = (0..25)
        .map(|i| {
            let id = MsgId::new();
            store
                .insert_message(&NewMessage {
                    message: Message {
                        id,
                        conversation_id: conv,
                        parent_id: Some(user),
                        role: Role::Assistant,
                        author_model: Some(model()),
                        parts: vec![Part::text(format!("answer {i}"))],
                        created_at: Utc::now(),
                        status: MsgStatus::Complete,
                    },
                    raw_provider_payload: None,
                })
                .unwrap();
            id
        })
        .collect();

    let got: Vec<MsgId> = store
        .siblings(expected[0])
        .unwrap()
        .iter()
        .map(|m| m.id)
        .collect();
    assert_eq!(got, expected, "siblings must come back in insertion order");
}
