//! The tool-use loop: model calls a tool, the tool runs, the result goes back,
//! the model answers. Plus the safety rules around it.

// Spec s14 permits unwrap/expect in tests.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::sync::{Arc, Mutex};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;
use ws_context::{ModelCapabilities, ModelRef, Part, Role, ToolSpec};
use ws_core::testing::{FakeBackend, Scripted};
use ws_core::{Orchestrator, Profile, ProfileTable, ToolHost, ToolOutput, TurnEvent};
use ws_store::{BlobStore, Store};

/// Tools that record what they were asked to do. `write_file` needs approval.
#[derive(Debug, Default)]
struct FakeTools {
    calls: Mutex<Vec<(String, serde_json::Value)>>,
}

#[async_trait::async_trait]
impl ToolHost for FakeTools {
    fn specs(&self) -> Vec<ToolSpec> {
        ["read_file", "write_file"]
            .map(|n| ToolSpec {
                name: n.to_owned(),
                description: format!("{n} for tests"),
                input_schema: serde_json::json!({"type": "object"}),
            })
            .to_vec()
    }
    fn needs_approval(&self, name: &str) -> bool {
        name == "write_file"
    }
    async fn call(&self, name: &str, args: serde_json::Value) -> ToolOutput {
        self.calls.lock().unwrap().push((name.to_owned(), args));
        ToolOutput::ok(format!("{name} done"))
    }
}

struct Harness {
    orch: Arc<Orchestrator>,
    store: Arc<Mutex<Store>>,
    backend: Arc<FakeBackend>,
    tools: Arc<FakeTools>,
    _dir: tempfile::TempDir,
}

fn harness(replies: Vec<Scripted>) -> Harness {
    harness_with(replies, None)
}

fn harness_with(replies: Vec<Scripted>, agents: Option<Arc<ws_core::Agents>>) -> Harness {
    let dir = tempfile::tempdir().unwrap();
    let store = Arc::new(Mutex::new(Store::open(dir.path().join("ws.db")).unwrap()));
    let blobs = Arc::new(BlobStore::new(dir.path().join("blobs")).unwrap());
    let profiles = Arc::new(ProfileTable::new());
    profiles.set(
        ModelRef::new("fake", "fake-1"),
        Profile {
            capabilities: ModelCapabilities {
                context_window: 8192,
                max_output_tokens: 256,
                ..ModelCapabilities::default()
            },
            thinking: ws_context::ThinkingMode::Off,
            user_edited: true,
        },
    );
    let mut backend = FakeBackend::new("fake", profiles.clone() as Arc<_>);
    for r in replies {
        backend = backend.scripted(r);
    }
    let backend = Arc::new(backend);
    let tools = Arc::new(FakeTools::default());
    let mut orch = Orchestrator::new(store.clone(), blobs, profiles)
        .with_backend(backend.clone() as Arc<_>)
        .with_tools(tools.clone() as Arc<_>);
    if let Some(agents) = agents {
        orch = orch.with_agents(agents);
    }
    Harness {
        orch: Arc::new(orch),
        store,
        backend,
        tools,
        _dir: dir,
    }
}

impl Harness {
    fn conversation(&self) -> ws_context::ConvId {
        let conv = self
            .orch
            .create_conversation("t", ModelRef::new("fake", "fake-1"))
            .unwrap();
        self.orch
            .append_user_message(conv, vec![Part::text("look at main.rs")])
            .unwrap();
        conv
    }

    fn roles(&self, conv: ws_context::ConvId) -> Vec<Role> {
        let s = self.store.lock().unwrap();
        s.load_active_path(conv)
            .unwrap()
            .iter()
            .map(|m| m.role)
            .collect()
    }
}

/// Run a turn, answering every approval request with `approve`.
async fn run(h: &Harness, conv: ws_context::ConvId, approve: bool) -> Vec<TurnEvent> {
    let (tx, mut rx) = mpsc::channel(64);
    let orch = h.orch.clone();
    let turn = tokio::spawn(async move { orch.run_turn(conv, tx, CancellationToken::new()).await });
    let mut seen = Vec::new();
    while let Some(e) = rx.recv().await {
        if let TurnEvent::ApprovalRequired { call_id, .. } = &e {
            assert!(h.orch.resolve_approval(call_id, approve));
        }
        seen.push(e);
    }
    turn.await.unwrap().unwrap();
    seen
}

#[tokio::test]
async fn a_tool_call_runs_and_its_result_goes_back_to_the_model() {
    let h = harness(vec![
        Scripted::calls("c1", "read_file", r#"{"path":"main.rs"}"#),
        Scripted::Text(vec!["main.rs prints hello".to_owned()]),
    ]);
    let conv = h.conversation();
    let events = run(&h, conv, true).await;

    assert_eq!(
        *h.tools.calls.lock().unwrap(),
        vec![(
            "read_file".to_owned(),
            serde_json::json!({"path": "main.rs"})
        )]
    );
    assert_eq!(
        h.roles(conv),
        vec![Role::User, Role::Assistant, Role::Tool, Role::Assistant]
    );
    assert!(
        !events
            .iter()
            .any(|e| matches!(e, TurnEvent::ApprovalRequired { .. })),
        "a read-only tool must not ask"
    );

    // The second request carries the result, and both carry the tool list.
    let requests = h.backend.requests();
    assert_eq!(requests.len(), 2);
    assert!(requests.iter().all(|r| r.tools.len() == 2));
    let last = requests[1].view.messages.last().unwrap();
    assert!(matches!(
        &last.parts[0],
        Part::ToolResult { call_id, is_error: false, .. } if call_id == "c1"
    ));
}

#[tokio::test]
async fn a_writing_tool_waits_for_approval_and_runs_when_approved() {
    let h = harness(vec![
        Scripted::calls("c1", "write_file", r#"{"path":"a.rs"}"#),
        Scripted::Text(vec!["written".to_owned()]),
    ]);
    let conv = h.conversation();
    let events = run(&h, conv, true).await;

    assert!(
        events
            .iter()
            .any(|e| matches!(e, TurnEvent::ApprovalRequired { name, .. } if name == "write_file"))
    );
    assert_eq!(h.tools.calls.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn a_declined_tool_never_runs_and_the_model_is_told() {
    let h = harness(vec![
        Scripted::calls("c1", "write_file", r#"{"path":"a.rs"}"#),
        Scripted::Text(vec!["ok, I won't".to_owned()]),
    ]);
    let conv = h.conversation();
    run(&h, conv, false).await;

    assert!(
        h.tools.calls.lock().unwrap().is_empty(),
        "declined means not run"
    );
    let requests = h.backend.requests();
    let result = &requests[1].view.messages.last().unwrap().parts[0];
    assert!(
        matches!(result, Part::ToolResult { is_error: true, content, .. }
            if content[0].as_text().unwrap().contains("declined")),
        "{result:?}"
    );
}

#[tokio::test]
async fn bad_json_arguments_become_an_error_result_not_a_failed_turn() {
    let h = harness(vec![
        Scripted::calls("c1", "read_file", "{not json"),
        Scripted::Text(vec!["let me retry".to_owned()]),
    ]);
    let conv = h.conversation();
    run(&h, conv, true).await;

    assert!(h.tools.calls.lock().unwrap().is_empty());
    assert_eq!(
        h.roles(conv).last(),
        Some(&Role::Assistant),
        "the turn went on"
    );
}

#[tokio::test]
async fn a_model_that_never_stops_calling_tools_is_cut_off() {
    let h = harness(
        (0..90)
            .map(|i| Scripted::calls(&format!("c{i}"), "read_file", "{}"))
            .collect(),
    );
    let conv = h.conversation();
    let (tx, mut rx) = mpsc::channel(1024);
    let orch = h.orch.clone();
    let turn = tokio::spawn(async move { orch.run_turn(conv, tx, CancellationToken::new()).await });
    let mut failed = false;
    while let Some(e) = rx.recv().await {
        failed |= matches!(e, TurnEvent::Failed { .. });
    }
    assert!(turn.await.unwrap().is_err());
    assert!(failed, "the UI must be told why it stopped");
    assert_eq!(h.tools.calls.lock().unwrap().len(), 80);
}

#[tokio::test]
async fn cancelling_while_waiting_for_approval_still_records_a_result() {
    // A tool call left without a result is rejected by providers on the next
    // turn, so the conversation would be stuck.
    let h = harness(vec![Scripted::calls("c1", "write_file", "{}")]);
    let conv = h.conversation();
    let cancel = CancellationToken::new();
    let (tx, mut rx) = mpsc::channel(64);
    let orch = h.orch.clone();
    let c = cancel.clone();
    let turn = tokio::spawn(async move { orch.run_turn(conv, tx, c).await });
    while let Some(e) = rx.recv().await {
        if matches!(e, TurnEvent::ApprovalRequired { .. }) {
            cancel.cancel();
        }
    }
    turn.await.unwrap().unwrap();

    assert!(h.tools.calls.lock().unwrap().is_empty());
    assert_eq!(h.roles(conv).last(), Some(&Role::Tool));
}

#[tokio::test]
async fn without_a_tool_host_no_tools_are_offered() {
    let dir = tempfile::tempdir().unwrap();
    let store = Arc::new(Mutex::new(Store::open(dir.path().join("ws.db")).unwrap()));
    let blobs = Arc::new(BlobStore::new(dir.path().join("blobs")).unwrap());
    let profiles = Arc::new(ProfileTable::new());
    let backend = Arc::new(FakeBackend::new("fake", profiles.clone() as Arc<_>).says("hi"));
    let orch = Orchestrator::new(store, blobs, profiles).with_backend(backend.clone() as Arc<_>);
    let conv = orch
        .create_conversation("t", ModelRef::new("fake", "fake-1"))
        .unwrap();
    orch.append_user_message(conv, vec![Part::text("hi")])
        .unwrap();
    let (tx, mut rx) = mpsc::channel(64);
    let turn = orch.run_turn(conv, tx, CancellationToken::new());
    let drain = async { while rx.recv().await.is_some() {} };
    let (result, ()) = tokio::join!(turn, drain);
    result.unwrap();
    assert!(backend.requests()[0].tools.is_empty());
}

#[tokio::test]
async fn a_dispatched_helper_runs_its_own_tools_and_reports_back() {
    let defs = tempfile::tempdir().unwrap();
    std::fs::write(
        defs.path().join("helper.md"),
        "---
name: helper
description: reads files
---
Read what you are told.
",
    )
    .unwrap();
    let h = harness_with(
        vec![
            Scripted::calls(
                "c1",
                "dispatch_agent",
                r#"{"agent":"helper","prompt":"read main.rs"}"#,
            ),
            // The helper's own turn.
            Scripted::calls("h1", "read_file", r#"{"path":"main.rs"}"#),
            Scripted::Text(vec!["helper: main.rs prints hello".to_owned()]),
            // Back in the main turn.
            Scripted::Text(vec!["it prints hello".to_owned()]),
        ],
        Some(Arc::new(ws_core::Agents::load(defs.path()))),
    );
    let conv = h.conversation();
    run(&h, conv, true).await;

    assert_eq!(h.tools.calls.lock().unwrap()[0].0, "read_file");
    let requests = h.backend.requests();
    assert_eq!(requests.len(), 4);
    let names =
        |i: usize| -> Vec<String> { requests[i].tools.iter().map(|t| t.name.clone()).collect() };
    assert!(names(0).contains(&"dispatch_agent".to_owned()));
    assert!(
        !names(1).contains(&"dispatch_agent".to_owned()),
        "a helper cannot dispatch helpers"
    );
    let system = requests[1].view.system.clone().unwrap();
    assert!(system.starts_with("Read what you are told."), "{system}");
    assert_eq!(requests[1].view.messages.len(), 1, "a helper starts fresh");

    // The helper's report is the dispatch call's result in the main turn.
    let report = requests[3]
        .view
        .messages
        .iter()
        .flat_map(|m| &m.parts)
        .any(|p| match p {
            Part::ToolResult { content, .. } => content.iter().any(|c| {
                c.as_text()
                    .is_some_and(|t| t.contains("helper: main.rs prints hello"))
            }),
            _ => false,
        });
    assert!(report, "the main model sees the helper's report");
    assert_eq!(
        h.roles(conv),
        vec![Role::User, Role::Assistant, Role::Tool, Role::Assistant],
        "the helper's turn is not stored in the conversation"
    );
}
