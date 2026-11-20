//! Diffs and undo for files an approved tool edits, through the real turn loop.

// Tests may unwrap and expect: a panic is the failure report.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;
use ws_context::{ModelRef, Part, ToolSpec};
use ws_core::testing::{FakeBackend, Scripted};
use ws_core::{CoreError, Orchestrator, ProfileTable, ToolHost, ToolOutput, TurnEvent};
use ws_store::{BlobStore, Store};

/// `write_file {path, content}` really writes, inside `root`.
#[derive(Debug)]
struct EditingTools {
    root: PathBuf,
}

#[async_trait::async_trait]
impl ToolHost for EditingTools {
    fn specs(&self) -> Vec<ToolSpec> {
        vec![ToolSpec {
            name: "write_file".to_owned(),
            description: String::new(),
            input_schema: serde_json::json!({"type": "object"}),
        }]
    }
    fn needs_approval(&self, _: &str) -> bool {
        true
    }
    fn file_for(&self, _: &str, args: &serde_json::Value) -> Option<PathBuf> {
        Some(self.root.join(args["path"].as_str()?))
    }
    async fn call(&self, _: &str, args: serde_json::Value) -> ToolOutput {
        let path = self.root.join(args["path"].as_str().unwrap());
        std::fs::write(path, args["content"].as_str().unwrap()).unwrap();
        ToolOutput::ok("written")
    }
}

struct Harness {
    orch: Arc<Orchestrator>,
    root: tempfile::TempDir,
    _db: tempfile::TempDir,
}

fn harness(replies: Vec<Scripted>) -> Harness {
    harness_with(replies, |root| {
        Arc::new(EditingTools {
            root: root.to_path_buf(),
        }) as Arc<_>
    })
}

fn harness_with(
    replies: Vec<Scripted>,
    tools: impl FnOnce(&std::path::Path) -> Arc<dyn ToolHost>,
) -> Harness {
    let db = tempfile::tempdir().unwrap();
    let root = tempfile::tempdir().unwrap();
    let store = Arc::new(Mutex::new(Store::open(db.path().join("ws.db")).unwrap()));
    let blobs = Arc::new(BlobStore::new(db.path().join("blobs")).unwrap());
    let profiles = Arc::new(ProfileTable::new());
    let mut backend = FakeBackend::new("fake", profiles.clone() as Arc<_>);
    for r in replies {
        backend = backend.scripted(r);
    }
    let orch = Orchestrator::new(store, blobs, profiles)
        .with_backend(Arc::new(backend) as Arc<_>)
        .with_agents(Arc::new(ws_core::Agents::default().with_worker()))
        .with_tools(tools(root.path()));
    Harness {
        orch: Arc::new(orch),
        root,
        _db: db,
    }
}

impl Harness {
    fn file(&self, name: &str) -> PathBuf {
        self.root.path().join(name)
    }

    /// One user message, every approval answered with `approve`.
    async fn turn(&self, approve: bool) -> ws_context::ConvId {
        let conv = self
            .orch
            .create_conversation("t", ModelRef::new("fake", "fake-1"))
            .unwrap();
        self.orch
            .append_user_message(conv, vec![Part::text("edit it")])
            .unwrap();
        let (tx, mut rx) = mpsc::channel(64);
        let orch = self.orch.clone();
        let turn =
            tokio::spawn(async move { orch.run_turn(conv, tx, CancellationToken::new()).await });
        while let Some(e) = rx.recv().await {
            if let TurnEvent::ApprovalRequired { call_id, .. } = &e {
                self.orch.resolve_approval(call_id, approve);
            }
        }
        turn.await.unwrap().unwrap();
        conv
    }
}

fn write(path: &str, content: &str) -> Vec<Scripted> {
    vec![
        Scripted::calls(
            "c1",
            "write_file",
            &serde_json::json!({"path": path, "content": content}).to_string(),
        ),
        Scripted::Text(vec!["done".to_owned()]),
    ]
}

#[tokio::test]
async fn an_edit_is_shown_as_a_diff_and_undo_restores_the_exact_bytes() {
    let h = harness(write("a.rs", "one\r\nTWO\r\n"));
    std::fs::write(h.file("a.rs"), "one\r\ntwo\r\n").unwrap();
    let conv = h.turn(true).await;

    let edits = h.orch.file_edits(conv).unwrap();
    assert_eq!(edits.len(), 1);
    assert_eq!(edits[0].call_id, "c1");
    assert!(
        edits[0].diff.contains("-two") && edits[0].diff.contains("+TWO"),
        "{}",
        edits[0].diff
    );
    assert!(!edits[0].undone);

    h.orch.undo_edit("c1").unwrap();
    assert_eq!(
        std::fs::read(h.file("a.rs")).unwrap(),
        b"one\r\ntwo\r\n",
        "byte for byte, line endings included"
    );
    assert!(h.orch.file_edits(conv).unwrap()[0].undone);
    assert!(
        matches!(h.orch.undo_edit("c1"), Err(CoreError::UndoRefused(_))),
        "only once"
    );
}

#[tokio::test]
async fn undo_is_refused_once_the_file_has_changed_again() {
    // Restoring would silently throw away whatever changed it since.
    let h = harness(write("a.rs", "agent\n"));
    std::fs::write(h.file("a.rs"), "original\n").unwrap();
    h.turn(true).await;
    std::fs::write(h.file("a.rs"), "the user kept typing\n").unwrap();

    let err = h.orch.undo_edit("c1").unwrap_err().to_string();
    assert!(err.contains("changed since"), "{err}");
    assert_eq!(
        std::fs::read_to_string(h.file("a.rs")).unwrap(),
        "the user kept typing\n",
        "nothing was overwritten"
    );
}

#[tokio::test]
async fn undoing_a_created_file_removes_it() {
    let h = harness(write("new.rs", "fn x() {}\n"));
    let conv = h.turn(true).await;
    assert!(
        h.orch.file_edits(conv).unwrap()[0]
            .diff
            .contains("/dev/null")
    );

    h.orch.undo_edit("c1").unwrap();
    assert!(!h.file("new.rs").exists());
}

#[tokio::test]
async fn a_declined_or_no_op_edit_records_nothing() {
    let h = harness(write("a.rs", "x\n"));
    std::fs::write(h.file("a.rs"), "original\n").unwrap();
    let conv = h.turn(false).await;
    assert!(h.orch.file_edits(conv).unwrap().is_empty(), "declined");

    let h = harness(write("a.rs", "same\n"));
    std::fs::write(h.file("a.rs"), "same\n").unwrap();
    let conv = h.turn(true).await;
    assert!(h.orch.file_edits(conv).unwrap().is_empty(), "unchanged");
}

#[tokio::test]
async fn two_workers_that_reuse_a_call_id_both_get_their_edit_recorded_and_undoable() {
    // Some providers number calls per response, so both workers say "w".
    let h = harness(vec![
        Scripted::CallsTools(vec![
            (
                "d1".to_owned(),
                "dispatch_agent".to_owned(),
                r#"{"agent":"worker","prompt":"a"}"#.to_owned(),
            ),
            (
                "d2".to_owned(),
                "dispatch_agent".to_owned(),
                r#"{"agent":"worker","prompt":"b"}"#.to_owned(),
            ),
        ]),
        Scripted::calls("w", "write_file", r#"{"path":"a.rs","content":"A\n"}"#),
        // Each write waits for approval, so the second worker starts meanwhile.
        Scripted::calls("w", "write_file", r#"{"path":"b.rs","content":"B\n"}"#),
        Scripted::Text(vec!["done".to_owned()]),
        Scripted::Text(vec!["done".to_owned()]),
        Scripted::Text(vec!["all done".to_owned()]),
    ]);
    let conv = h.turn(true).await;

    let edits = h.orch.file_edits(conv).unwrap();
    assert_eq!(
        edits.len(),
        2,
        "the second edit was not lost to a duplicate id"
    );
    assert_ne!(edits[0].call_id, edits[1].call_id);
    h.orch.undo_edit(&edits[1].call_id).unwrap();
    assert!(
        h.file("a.rs").exists() && !h.file("b.rs").exists(),
        "undo touched only its own edit"
    );
}

/// `script {files: [{path, content}], ask}`: writes several files and reports
/// what changed, as a Code Mode script does; asks first only when `ask` is set.
#[derive(Debug)]
struct ScriptTools {
    root: PathBuf,
}

#[async_trait::async_trait]
impl ToolHost for ScriptTools {
    fn specs(&self) -> Vec<ToolSpec> {
        vec![ToolSpec {
            name: "script".to_owned(),
            description: String::new(),
            input_schema: serde_json::json!({"type": "object"}),
        }]
    }
    fn needs_approval(&self, _: &str) -> bool {
        true
    }
    fn needs_approval_for(&self, _: &str, args: &serde_json::Value) -> bool {
        args["ask"].as_bool().unwrap_or(true)
    }
    async fn call(&self, _: &str, args: serde_json::Value) -> ToolOutput {
        let mut out = ToolOutput::ok("ran");
        for f in args["files"].as_array().unwrap() {
            let path = self.root.join(f["path"].as_str().unwrap());
            let before = std::fs::read(&path).ok();
            std::fs::write(&path, f["content"].as_str().unwrap()).unwrap();
            out.changes.push(ws_core::Change {
                after: std::fs::read(&path).ok(),
                before,
                path,
            });
        }
        out
    }
}

fn script(files: serde_json::Value, ask: bool) -> Vec<Scripted> {
    vec![
        Scripted::calls(
            "s1",
            "script",
            &serde_json::json!({"files": files, "ask": ask}).to_string(),
        ),
        Scripted::Text(vec!["done".to_owned()]),
    ]
}

#[tokio::test]
async fn each_file_a_script_changes_gets_its_own_diff_and_undo() {
    let files = serde_json::json!([
        {"path": "a.rs", "content": "A2"},
        {"path": "b.rs", "content": "B1"},
        {"path": "same.rs", "content": "same"},
    ]);
    let h = harness_with(script(files, true), |root| {
        Arc::new(ScriptTools {
            root: root.to_path_buf(),
        }) as Arc<_>
    });
    std::fs::write(h.file("a.rs"), "A1").unwrap();
    std::fs::write(h.file("same.rs"), "same").unwrap();
    let conv = h.turn(true).await;

    let edits = h.orch.file_edits(conv).unwrap();
    let ids: Vec<&str> = edits.iter().map(|e| e.call_id.as_str()).collect();
    assert_eq!(ids, ["s1#1", "s1#2"], "an unchanged file is not an edit");
    assert!(edits[0].diff.contains("-A1") && edits[0].diff.contains("+A2"));

    h.orch.undo_edit("s1#2").unwrap();
    assert!(
        !h.file("b.rs").exists(),
        "undoing a creation removes the file"
    );
    assert_eq!(
        std::fs::read_to_string(h.file("a.rs")).unwrap(),
        "A2",
        "the other edit stays"
    );
    h.orch.undo_edit("s1#1").unwrap();
    assert_eq!(std::fs::read_to_string(h.file("a.rs")).unwrap(), "A1");
}

#[tokio::test]
async fn approval_can_depend_on_what_a_call_will_do() {
    let files = serde_json::json!([{"path": "a.rs", "content": "quiet"}]);
    let h = harness_with(script(files, false), |root| {
        Arc::new(ScriptTools {
            root: root.to_path_buf(),
        }) as Arc<_>
    });
    // Declines everything: a call that needs no approval runs regardless.
    h.turn(false).await;
    assert_eq!(std::fs::read_to_string(h.file("a.rs")).unwrap(), "quiet");

    let files = serde_json::json!([{"path": "b.rs", "content": "loud"}]);
    let h = harness_with(script(files, true), |root| {
        Arc::new(ScriptTools {
            root: root.to_path_buf(),
        }) as Arc<_>
    });
    h.turn(false).await;
    assert!(
        !h.file("b.rs").exists(),
        "a call that asks and is declined never runs"
    );
}
