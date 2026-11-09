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
    /// When set, helpers get this as their own copy of the tools.
    copy: Mutex<Option<Arc<FakeTools>>>,
    /// The messages helpers' work was sealed with.
    sealed: Arc<Mutex<Vec<String>>>,
}

#[async_trait::async_trait]
impl ToolHost for FakeTools {
    fn specs(&self) -> Vec<ToolSpec> {
        ["read_file", "write_file", "choose_location"]
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
    fn context(&self) -> Option<String> {
        Some("OUTLINE: src/main.rs: fn main".to_owned())
    }
    async fn call(&self, name: &str, args: serde_json::Value) -> ToolOutput {
        self.calls.lock().unwrap().push((name.to_owned(), args));
        ToolOutput::ok(format!("{name} done"))
    }
    fn isolate(&self, id: &str) -> Option<Result<ws_core::Isolated, String>> {
        let copy = self.copy.lock().unwrap().clone()?;
        let sealed = self.sealed.clone();
        Some(Ok(ws_core::Isolated {
            host: copy as Arc<_>,
            root: "/repo".to_owned(),
            folder: format!("/repo/.git/ws-worktrees/{id}"),
            branch: format!("ws/{id}"),
            base: "abc123".to_owned(),
            seal: Box::new(move |message| {
                sealed.lock().unwrap().push(message.to_owned());
                Ok(true)
            }),
        }))
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
    harness_full(replies, agents, Vec::new())
}

fn harness_full(
    replies: Vec<Scripted>,
    agents: Option<Arc<ws_core::Agents>>,
    reconnect_waits: Vec<std::time::Duration>,
) -> Harness {
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
        .with_tools(tools.clone() as Arc<_>)
        .with_reconnect_waits(reconnect_waits);
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
    assert!(requests.iter().all(|r| r.tools.len() == 3));
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
    assert!(names(0).contains(&"choose_location".to_owned()));
    assert!(
        !names(1).contains(&"choose_location".to_owned()),
        "only the main model opens the folder picker"
    );
    let system = requests[1].view.system.clone().unwrap();
    assert!(system.starts_with("Read what you are told."), "{system}");
    assert!(
        system.ends_with("OUTLINE: src/main.rs: fn main"),
        "workers get the outline: {system}"
    );
    let main = requests[0].view.system.clone().unwrap_or_default();
    assert!(
        main.contains("OUTLINE: src/main.rs: fn main"),
        "so does the main model: {main}"
    );
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

#[tokio::test]
async fn workers_dispatched_together_can_message_each_other() {
    let post = |id: &str, text: &str| {
        Scripted::calls(
            id,
            "post_message",
            &serde_json::json!({ "text": text }).to_string(),
        )
    };
    // The fake backend answers in call order, and the fake streams never wait,
    // so the first worker runs to its end before the second starts.
    let h = harness_with(
        vec![
            Scripted::CallsTools(vec![
                (
                    "d1".to_owned(),
                    "dispatch_agent".to_owned(),
                    r#"{"agent":"worker","prompt":"part a"}"#.to_owned(),
                ),
                (
                    "d2".to_owned(),
                    "dispatch_agent".to_owned(),
                    r#"{"agent":"worker","prompt":"part b"}"#.to_owned(),
                ),
            ]),
            post("p1", "hello from A"),
            Scripted::Text(vec!["A done".to_owned()]),
            post("p2", "hello from B"),
            Scripted::Text(vec!["B done".to_owned()]),
            Scripted::Text(vec!["all done".to_owned()]),
        ],
        Some(Arc::new(ws_core::Agents::default().with_worker())),
    );
    let conv = h.conversation();
    let events = run(&h, conv, true).await;

    // What each worker did is streamed as it happens and saved when it ends.
    let live = events
        .iter()
        .filter(|e| matches!(e, TurnEvent::HelperStep { dispatch, .. } if dispatch == "d2"))
        .count();
    assert_eq!(live, 4, "B's post, its result, A's note and B's report");
    let logs: Vec<(String, Vec<ws_context::HelperEntry>)> = h
        .store
        .lock()
        .unwrap()
        .list_events(conv)
        .unwrap()
        .into_iter()
        .filter_map(|e| match e.kind {
            ws_context::EventKind::HelperLog {
                dispatch, entries, ..
            } => Some((dispatch, entries)),
            _ => None,
        })
        .collect();
    assert_eq!(logs.len(), 2, "one log per worker");
    let (dispatch, b) = &logs[1];
    assert_eq!(dispatch, "d2");
    use ws_context::HelperEntry as E;
    assert!(matches!(&b[0], E::Call { name, .. } if name == "post_message"));
    assert!(matches!(
        &b[1],
        E::Result {
            is_error: false,
            ..
        }
    ));
    assert!(
        matches!(&b[2], E::Received { text } if text == "- from worker (d1): hello from A"),
        "{:?}",
        b[2]
    );
    assert!(matches!(&b[3], E::Text { text } if text == "B done"));

    // The team chat is saved line by line, in order, and each line streams too.
    use ws_context::TeamWhat as W;
    let team: Vec<(String, ws_context::TeamPost)> = h
        .store
        .lock()
        .unwrap()
        .list_events(conv)
        .unwrap()
        .into_iter()
        .filter_map(|e| match e.kind {
            ws_context::EventKind::Team(p) => Some((e.id.to_string(), p)),
            _ => None,
        })
        .collect();
    let said: Vec<(W, &str, &str, &str)> = team
        .iter()
        .map(|(_, p)| {
            (
                p.what,
                p.from.as_str(),
                p.dispatch.as_str(),
                p.text.as_str(),
            )
        })
        .collect();
    assert_eq!(
        said,
        vec![
            (W::Brief, "lead", "d1", "part a"),
            (W::Note, "worker", "d1", "hello from A"),
            (W::Report, "worker", "d1", "A done"),
            (W::Brief, "lead", "d2", "part b"),
            (W::Note, "worker", "d2", "hello from B"),
            (W::Report, "worker", "d2", "B done"),
        ]
    );
    assert_eq!(team[0].1.to.as_deref(), Some("worker"));
    assert_eq!(team[1].1.to, None, "a note for everyone");
    let streamed: Vec<&str> = events
        .iter()
        .filter_map(|e| match e {
            TurnEvent::Team { id, .. } => Some(id.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(
        streamed,
        team.iter().map(|(id, _)| id.as_str()).collect::<Vec<_>>(),
        "live lines carry the saved ids"
    );

    let requests = h.backend.requests();
    assert_eq!(requests.len(), 6);
    let has_tool = |i: usize| requests[i].tools.iter().any(|t| t.name == "post_message");
    assert!(!has_tool(0), "the coordinator does not chat with workers");
    assert!(has_tool(1) && has_tool(3), "workers do");
    let text = |i: usize| -> String {
        requests[i]
            .view
            .messages
            .iter()
            .flat_map(|m| &m.parts)
            .filter_map(Part::as_text)
            .collect::<Vec<_>>()
            .join("\n")
    };
    let heard = "Messages from the other helpers:\n- from worker (d1): hello from A";
    assert!(
        text(4).contains(heard),
        "B is told what A posted: {}",
        text(4)
    );
    assert!(
        !text(2).contains("Messages from the other helpers"),
        "A hears nothing of itself"
    );
}

#[tokio::test]
async fn a_worker_on_a_backend_without_tools_says_so_instead_of_doing_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let store = Arc::new(Mutex::new(Store::open(dir.path().join("ws.db")).unwrap()));
    let blobs = Arc::new(BlobStore::new(dir.path().join("blobs")).unwrap());
    let profiles = Arc::new(ProfileTable::new());
    let backend = Arc::new(
        FakeBackend::new("fake", profiles.clone() as Arc<_>)
            .without_tools()
            .scripted(Scripted::calls(
                "d1",
                "dispatch_agent",
                r#"{"agent":"worker","prompt":"do it"}"#,
            ))
            .says("ok"),
    );
    let orch = Arc::new(
        Orchestrator::new(store, blobs, profiles)
            .with_backend(backend.clone() as Arc<_>)
            .with_tools(Arc::new(FakeTools::default()) as Arc<_>)
            .with_agents(Arc::new(ws_core::Agents::default().with_worker())),
    );
    let conv = orch
        .create_conversation("t", ModelRef::new("fake", "fake-1"))
        .unwrap();
    orch.append_user_message(conv, vec![Part::text("go")])
        .unwrap();
    let (tx, mut rx) = mpsc::channel(64);
    let run = orch.run_turn(conv, tx, CancellationToken::new());
    let (result, ()) = tokio::join!(run, async { while rx.recv().await.is_some() {} });
    result.unwrap();

    let requests = backend.requests();
    assert_eq!(requests.len(), 2, "no worker request was made");
    // Without tool support the history is flattened to text.
    let told = requests[1]
        .view
        .messages
        .iter()
        .flat_map(|m| &m.parts)
        .any(|p| {
            p.as_text()
                .is_some_and(|t| t.contains("does not support tools"))
        });
    assert!(told, "the coordinator is told why nothing happened");
}

// ---- helpers and workers: failure paths ------------------------------------

fn worker_agents() -> Option<Arc<ws_core::Agents>> {
    Some(Arc::new(ws_core::Agents::default().with_worker()))
}

fn dispatch(id: &str, agent: &str) -> Scripted {
    Scripted::calls(
        id,
        "dispatch_agent",
        &serde_json::json!({"agent": agent, "prompt": "do it"}).to_string(),
    )
}

/// Whether anything the model was sent in `request` mentions `needle`.
fn heard(request: &ws_context::BackendRequest, needle: &str) -> bool {
    request
        .view
        .messages
        .iter()
        .flat_map(|m| &m.parts)
        .any(|p| match p {
            Part::ToolResult { content, .. } => content
                .iter()
                .any(|c| c.as_text().is_some_and(|t| t.contains(needle))),
            other => other.as_text().is_some_and(|t| t.contains(needle)),
        })
}

#[tokio::test]
async fn dispatching_an_agent_that_does_not_exist_is_an_error_result() {
    let h = harness_with(
        vec![
            dispatch("d1", "nope"),
            Scripted::Text(vec!["ok".to_owned()]),
        ],
        worker_agents(),
    );
    let conv = h.conversation();
    run(&h, conv, true).await;
    let requests = h.backend.requests();
    assert_eq!(requests.len(), 2);
    assert!(heard(&requests[1], "there is no agent called nope"));
}

#[tokio::test]
async fn a_worker_cannot_dispatch_workers_of_its_own() {
    let h = harness_with(
        vec![
            dispatch("d1", "worker"),
            dispatch("w1", "worker"), // the worker tries
            Scripted::Text(vec!["gave up".to_owned()]),
            Scripted::Text(vec!["ok".to_owned()]),
        ],
        worker_agents(),
    );
    let conv = h.conversation();
    run(&h, conv, true).await;
    let requests = h.backend.requests();
    assert_eq!(requests.len(), 4, "the nested dispatch started nothing");
    assert!(heard(&requests[2], "cannot dispatch helpers of its own"));
}

#[tokio::test]
async fn a_worker_whose_backend_fails_reports_the_failure_and_the_coordinator_carries_on() {
    let h = harness_with(
        vec![
            dispatch("d1", "worker"),
            Scripted::FailsAfter {
                deltas: vec!["partial".to_owned()],
                error: "connection reset".to_owned(),
            },
            Scripted::Text(vec!["ok".to_owned()]),
        ],
        worker_agents(),
    );
    let conv = h.conversation();
    run(&h, conv, true).await;
    let requests = h.backend.requests();
    assert!(heard(&requests[2], "worker failed"));
    assert!(heard(&requests[2], "connection reset"));
}

#[tokio::test]
async fn a_worker_that_never_stops_calling_tools_is_stopped_at_the_round_limit() {
    let mut replies = vec![dispatch("d1", "worker")];
    // Exactly the limit: every round calls a tool, so the worker never gets to end.
    replies
        .extend((0..50).map(|i| Scripted::calls(&format!("r{i}"), "read_file", r#"{"path":"a"}"#)));
    replies.push(Scripted::Text(vec!["ok".to_owned()]));
    let h = harness_with(replies, worker_agents());
    let conv = h.conversation();
    run(&h, conv, true).await;
    let requests = h.backend.requests();
    assert_eq!(
        h.tools.calls.lock().unwrap().len(),
        50,
        "50 rounds, then stop"
    );
    assert!(heard(requests.last().unwrap(), "stopped after 50 rounds"));
}

#[tokio::test]
async fn cancelling_while_a_worker_waits_for_approval_ends_it_without_hanging() {
    let h = harness_with(
        vec![
            dispatch("d1", "worker"),
            Scripted::calls("w1", "write_file", r#"{"path":"a","content":"x"}"#),
            Scripted::Text(vec!["never reached".to_owned()]),
        ],
        worker_agents(),
    );
    let conv = h.conversation();
    let cancel = CancellationToken::new();
    let (tx, mut rx) = mpsc::channel(64);
    let orch = h.orch.clone();
    let token = cancel.clone();
    let turn = tokio::spawn(async move { orch.run_turn(conv, tx, token).await });
    while let Some(e) = rx.recv().await {
        if matches!(e, TurnEvent::ApprovalRequired { .. }) {
            cancel.cancel();
        }
    }
    tokio::time::timeout(std::time::Duration::from_secs(5), turn)
        .await
        .expect("the turn ended")
        .unwrap()
        .unwrap();
    assert!(
        h.tools.calls.lock().unwrap().is_empty(),
        "the write never ran"
    );
    assert_eq!(
        h.backend.requests().len(),
        2,
        "no model call after the cancel"
    );
}

// ---- post_message misuse ----------------------------------------------------

#[tokio::test]
async fn post_message_from_the_coordinator_or_without_text_is_an_error_result() {
    let h = harness_with(
        vec![
            Scripted::calls("p1", "post_message", r#"{"text":"hi"}"#), // not a worker
            dispatch("d1", "worker"),
            Scripted::calls("p2", "post_message", r#"{"to":"x"}"#), // no text
            Scripted::Text(vec!["done".to_owned()]),
            Scripted::Text(vec!["ok".to_owned()]),
        ],
        worker_agents(),
    );
    let conv = h.conversation();
    run(&h, conv, true).await;
    let requests = h.backend.requests();
    assert!(
        heard(&requests[1], "post_message is for helpers"),
        "coordinator"
    );
    assert!(
        heard(&requests[3], "post_message is for helpers"),
        "missing text"
    );
}

#[tokio::test]
async fn a_worker_whose_model_fails_moves_down_the_fallbacks_and_the_coordinator_is_told() {
    let overloaded = || ws_context::BackendError::Http {
        backend: "fake".into(),
        status: 529,
        message: "overloaded".to_owned(),
    };
    let h = harness_with(
        vec![
            dispatch("d1", "worker"),
            // The worker on the chat's model, then on each fallback in turn.
            Scripted::RefusesToStart {
                error: overloaded(),
            },
            Scripted::FailsAfter {
                deltas: vec!["half".to_owned()],
                error: "connection reset".to_owned(),
            },
            Scripted::Text(vec!["done".to_owned()]),
            // Back in the coordinator's turn.
            Scripted::Text(vec!["all done".to_owned()]),
        ],
        worker_agents(),
    );
    h.orch.set_worker_fallbacks(vec![
        // Not a backend here: skipped, not a failure.
        ModelRef::new("gone", "x"),
        ModelRef::new("fake", "fake-2"),
        ModelRef::new("fake", "fake-3"),
    ]);
    let conv = h.conversation();
    run(&h, conv, true).await;

    let requests = h.backend.requests();
    let models: Vec<&str> = requests.iter().map(|r| r.model.model_id.as_str()).collect();
    assert_eq!(
        models,
        ["fake-1", "fake-1", "fake-2", "fake-3", "fake-1"],
        "only the worker moves; the coordinator stays on the chat's model"
    );
    assert!(
        !heard(&requests[3], "half"),
        "the failed round's partial reply is dropped, not shown to the next model"
    );
    assert!(heard(
        &requests[4],
        "worker finished on fake/fake-3 because fake/fake-1 failed"
    ));

    let switches: Vec<(String, String)> = h
        .store
        .lock()
        .unwrap()
        .list_events(conv)
        .unwrap()
        .into_iter()
        .flat_map(|e| match e.kind {
            ws_context::EventKind::HelperLog { entries, .. } => entries,
            _ => Vec::new(),
        })
        .filter_map(|e| match e {
            ws_context::HelperEntry::Switch { from, to, .. } => Some((from, to)),
            _ => None,
        })
        .collect();
    assert_eq!(
        switches,
        [
            ("fake/fake-1".to_owned(), "fake/fake-2".to_owned()),
            ("fake/fake-2".to_owned(), "fake/fake-3".to_owned())
        ]
    );
}

#[tokio::test]
async fn workers_start_on_the_worker_model_and_an_agent_can_name_its_own() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join("coder.md"),
        "---\nname: coder\ndescription: writes code\nmodel: fake/fake-9\n---\nYou code.",
    )
    .unwrap();
    let call = |id: &str, agent: &str| {
        (
            id.to_owned(),
            "dispatch_agent".to_owned(),
            serde_json::json!({ "agent": agent, "prompt": "do it" }).to_string(),
        )
    };
    let h = harness_with(
        vec![
            Scripted::CallsTools(vec![call("d1", "worker"), call("d2", "coder")]),
            Scripted::Text(vec!["worker done".to_owned()]),
            Scripted::Text(vec!["coder done".to_owned()]),
            Scripted::Text(vec!["all done".to_owned()]),
        ],
        Some(Arc::new(ws_core::Agents::load(dir.path()).with_worker())),
    );
    h.orch
        .set_worker_model(Some(ModelRef::new("fake", "fake-2")));
    let conv = h.conversation();
    run(&h, conv, true).await;

    let models: Vec<String> = h
        .backend
        .requests()
        .iter()
        .map(|r| r.model.model_id.clone())
        .collect();
    assert_eq!(
        models,
        ["fake-1", "fake-2", "fake-9", "fake-1"],
        "the lead stays on the chat's model; the coder's own model beats the setting"
    );
    let shown: Vec<String> = h
        .store
        .lock()
        .unwrap()
        .list_events(conv)
        .unwrap()
        .into_iter()
        .flat_map(|e| match e.kind {
            ws_context::EventKind::HelperLog { entries, .. } => entries,
            _ => Vec::new(),
        })
        .filter_map(|e| match e {
            ws_context::HelperEntry::Model { name } => Some(name),
            _ => None,
        })
        .collect();
    assert_eq!(
        shown,
        ["fake/fake-2", "fake/fake-9"],
        "each worker says which model it is on"
    );
}

#[tokio::test]
async fn the_coordinator_itself_never_falls_back() {
    let h = harness(vec![Scripted::RefusesToStart {
        error: ws_context::BackendError::Http {
            backend: "fake".into(),
            status: 529,
            message: "overloaded".to_owned(),
        },
    }]);
    h.orch
        .set_worker_fallbacks(vec![ModelRef::new("fake", "fake-2")]);
    let conv = h.conversation();
    let (tx, _rx) = mpsc::channel(64);
    let result = h.orch.run_turn(conv, tx, CancellationToken::new()).await;
    assert!(result.is_err(), "the turn fails as before");
    assert_eq!(h.backend.requests().len(), 1, "no second model was tried");
}

#[tokio::test]
async fn a_worker_whose_connection_drops_retries_the_same_model_before_falling_back() {
    let h = harness_full(
        vec![
            dispatch("d1", "worker"),
            Scripted::FailsAfter {
                deltas: vec!["half".to_owned()],
                error: "connection reset".to_owned(),
            },
            Scripted::Text(vec!["done".to_owned()]),
            Scripted::Text(vec!["all done".to_owned()]),
        ],
        worker_agents(),
        vec![std::time::Duration::ZERO],
    );
    h.orch
        .set_worker_fallbacks(vec![ModelRef::new("fake", "fake-2")]);
    let conv = h.conversation();
    run(&h, conv, true).await;

    let requests = h.backend.requests();
    let models: Vec<&str> = requests.iter().map(|r| r.model.model_id.as_str()).collect();
    assert_eq!(
        models,
        ["fake-1", "fake-1", "fake-1", "fake-1"],
        "no fallback for a dropped line"
    );
    assert!(
        !heard(&requests[2], "half"),
        "the cut-off round starts clean"
    );

    let retries = h
        .store
        .lock()
        .unwrap()
        .list_events(conv)
        .unwrap()
        .into_iter()
        .flat_map(|e| match e.kind {
            ws_context::EventKind::HelperLog { entries, .. } => entries,
            _ => Vec::new(),
        })
        .filter(|e| matches!(e, ws_context::HelperEntry::Retry { .. }))
        .count();
    assert_eq!(retries, 1);
}

#[tokio::test]
async fn a_helper_works_in_its_own_copy_and_the_coordinator_is_told_where_its_changes_are() {
    let h = harness_with(
        vec![
            dispatch("d1", "worker"),
            Scripted::calls("w1", "write_file", r#"{"path":"a.rs"}"#),
            Scripted::Text(vec!["wrote a.rs".to_owned()]),
            Scripted::Text(vec!["all done".to_owned()]),
        ],
        worker_agents(),
    );
    let copy = Arc::new(FakeTools::default());
    h.tools.copy.lock().unwrap().replace(copy.clone());
    let conv = h.conversation();
    // Approvals are refused: the copy's host is a FakeTools too, so write_file
    // still asks; the call is recorded only if it went to the copy.
    run(&h, conv, true).await;

    assert!(
        h.tools.calls.lock().unwrap().is_empty(),
        "nothing ran in the project"
    );
    assert_eq!(
        copy.calls.lock().unwrap().len(),
        1,
        "the helper's call ran in its copy"
    );
    assert_eq!(*h.tools.sealed.lock().unwrap(), ["worker: do it"]);

    let requests = h.backend.requests();
    assert!(heard(requests.last().unwrap(), "on branch ws/d1"));
    let isolated = h
        .store
        .lock()
        .unwrap()
        .list_events(conv)
        .unwrap()
        .into_iter()
        .flat_map(|e| match e.kind {
            ws_context::EventKind::HelperLog { entries, .. } => entries,
            _ => Vec::new(),
        })
        .any(|e| matches!(e, ws_context::HelperEntry::Isolated { ref branch, .. } if branch == "ws/d1"));
    assert!(isolated, "the log says where it worked");
}
