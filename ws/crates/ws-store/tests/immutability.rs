//! Proof that the database itself enforces the message immutability
//! invariants, rather than trusting callers to respect them.
//!
//! Required by checkpoint C1: "test proving the immutability trigger rejects
//! updates".

// Tests may unwrap and expect: a panic is the failure report.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use chrono::Utc;
use rusqlite::params;
use ws_context::{ConvId, Conversation, Message, ModelRef, MsgId, MsgStatus, Part, Role};
use ws_store::{NewMessage, Store};

fn model() -> ModelRef {
    ModelRef::new("anthropic", "claude-test-1")
}

fn seed() -> (Store, ConvId, MsgId) {
    let store = Store::open_in_memory().unwrap();
    let conv = Conversation {
        id: ConvId::new(),
        title: "test".to_owned(),
        system_prompt: None,
        active_leaf_id: None,
        current_model: model(),
        created_at: Utc::now(),
    };
    store.create_conversation(&conv).unwrap();

    let msg = Message {
        id: MsgId::new(),
        conversation_id: conv.id,
        parent_id: None,
        role: Role::User,
        author_model: None,
        parts: vec![Part::text("original text")],
        created_at: Utc::now(),
        status: MsgStatus::Complete,
    };
    let id = msg.id;
    store
        .insert_message(&NewMessage {
            message: msg,
            raw_provider_payload: None,
        })
        .unwrap();

    (store, conv.id, id)
}

#[test]
fn rewriting_message_content_is_refused() {
    let (store, _conv, msg) = seed();

    let err = store
        .connection()
        .execute(
            "UPDATE messages SET parts_json = ?2 WHERE id = ?1",
            params![msg.to_string(), r#"[{"type":"text","text":"tampered"}]"#],
        )
        .unwrap_err();

    assert!(
        err.to_string().contains("messages are immutable"),
        "expected the immutability trigger, got: {err}"
    );

    // And the stored text is untouched.
    let stored = store.get_message(msg).unwrap().unwrap();
    assert_eq!(stored.text_content(), "original text");
}

#[test]
fn relinking_a_message_to_a_different_parent_is_refused() {
    let (store, _conv, msg) = seed();
    let err = store
        .connection()
        .execute(
            "UPDATE messages SET parent_id = ?2 WHERE id = ?1",
            params![msg.to_string(), MsgId::new().to_string()],
        )
        .unwrap_err();
    assert!(err.to_string().contains("messages are immutable"));
}

#[test]
fn changing_a_message_role_is_refused() {
    let (store, _conv, msg) = seed();
    let err = store
        .connection()
        .execute(
            "UPDATE messages SET role = 'assistant' WHERE id = ?1",
            params![msg.to_string()],
        )
        .unwrap_err();
    assert!(err.to_string().contains("messages are immutable"));
}

#[test]
fn rewriting_the_raw_provider_payload_is_refused() {
    let (store, _conv, msg) = seed();
    let err = store
        .connection()
        .execute(
            "UPDATE messages SET raw_provider_payload = '{}' WHERE id = ?1",
            params![msg.to_string()],
        )
        .unwrap_err();
    assert!(err.to_string().contains("messages are immutable"));
}

#[test]
fn the_error_is_recognised_as_an_immutability_violation() {
    let (store, _conv, msg) = seed();
    let err: ws_store::StoreError = store
        .connection()
        .execute(
            "UPDATE messages SET parts_json = '[]' WHERE id = ?1",
            params![msg.to_string()],
        )
        .unwrap_err()
        .into();

    assert!(
        err.is_immutability_violation(),
        "callers must be able to distinguish this from an ordinary failure"
    );
}

#[test]
fn deleting_one_message_from_a_live_conversation_is_refused() {
    let (store, _conv, msg) = seed();
    let err = store
        .connection()
        .execute(
            "DELETE FROM messages WHERE id = ?1",
            params![msg.to_string()],
        )
        .unwrap_err();

    assert!(
        err.to_string().contains("cannot be deleted individually"),
        "expected the single-delete guard, got: {err}"
    );
    assert!(store.get_message(msg).unwrap().is_some());
}

#[test]
fn deleting_the_whole_conversation_cascades() {
    let (store, conv, msg) = seed();

    // The only destructive operation in the product (invariant 5).
    store.delete_conversation(conv).unwrap();

    assert!(store.get_conversation(conv).unwrap().is_none());
    assert!(
        store.get_message(msg).unwrap().is_none(),
        "messages must go with their conversation"
    );
}
