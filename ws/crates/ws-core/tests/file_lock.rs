//! Two workers editing one file at the same moment (through the real turn loop).

// Spec s14 permits unwrap/expect in tests.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;
use ws_context::{ModelRef, Part, ToolSpec};
use ws_core::testing::{FakeBackend, Scripted};
use ws_core::{Agents, Orchestrator, ProfileTable, ToolHost, ToolOutput, TurnEvent};
use ws_store::{BlobStore, Store};

/// `append {path, line}` adds a line the slow, unsafe way: read the file,
/// wait, write it back. Two overlapping calls lose a line unless something
/// keeps them apart. It asks for no approval, so nothing else makes them wait.
#[derive(Debug)]
struct SlowAppend {
    root: PathBuf,
    pause: Duration,
}

#[async_trait::async_trait]
impl ToolHost for SlowAppend {
    fn specs(&self) -> Vec<ToolSpec> {
        vec![ToolSpec {
            name: "append".to_owned(),
            description: String::new(),
            input_schema: serde_json::json!({"type": "object"}),
        }]
    }
    fn needs_approval(&self, _: &str) -> bool {
        false
    }
    fn file_for(&self, _: &str, args: &serde_json::Value) -> Option<PathBuf> {
        Some(self.root.join(args["path"].as_str()?))
    }
    async fn call(&self, _: &str, args: serde_json::Value) -> ToolOutput {
        let path = self.root.join(args["path"].as_str().unwrap());
        let mut text = std::fs::read_to_string(&path).unwrap_or_default();
        tokio::time::sleep(self.pause).await;
        text.push_str(args["line"].as_str().unwrap());
        text.push('\n');
        std::fs::write(&path, text).unwrap();
        ToolOutput::ok("appended")
    }
}

struct Harness {
    orch: Arc<Orchestrator>,
    root: tempfile::TempDir,
    _db: tempfile::TempDir,
}

fn harness(pause: Duration, replies: Vec<Scripted>) -> Harness {
    let (db, root) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    let store = Arc::new(Mutex::new(Store::open(db.path().join("ws.db")).unwrap()));
    let blobs = Arc::new(BlobStore::new(db.path().join("blobs")).unwrap());
    let profiles = Arc::new(ProfileTable::new());
    let mut backend = FakeBackend::new("fake", profiles.clone() as Arc<_>);
    for r in replies {
        backend = backend.scripted(r);
    }
    let orch = Orchestrator::new(store, blobs, profiles)
        .with_backend(Arc::new(backend) as Arc<_>)
        .with_agents(Arc::new(Agents::default().with_worker()))
        .with_tools(Arc::new(SlowAppend {
            root: root.path().to_path_buf(),
            pause,
        }) as Arc<_>);
    Harness {
        orch: Arc::new(orch),
        root,
        _db: db,
    }
}

fn dispatch_two() -> Scripted {
    let one = |id: &str| {
        (
            id.to_owned(),
            "dispatch_agent".to_owned(),
            r#"{"agent":"worker","prompt":"append"}"#.to_owned(),
        )
    };
    Scripted::CallsTools(vec![one("d1"), one("d2")])
}

fn append(id: &str, line: &str) -> Scripted {
    Scripted::calls(
        id,
        "append",
        &serde_json::json!({"path": "log.txt", "line": line}).to_string(),
    )
}

async fn start(
    h: &Harness,
    cancel: CancellationToken,
) -> (tokio::task::JoinHandle<()>, mpsc::Receiver<TurnEvent>) {
    let conv = h
        .orch
        .create_conversation("t", ModelRef::new("fake", "fake-1"))
        .unwrap();
    h.orch
        .append_user_message(conv, vec![Part::text("go")])
        .unwrap();
    let (tx, rx) = mpsc::channel(64);
    let orch = h.orch.clone();
    let turn = tokio::spawn(async move {
        orch.run_turn(conv, tx, cancel).await.unwrap();
    });
    (turn, rx)
}

#[tokio::test]
async fn two_workers_appending_to_one_file_at_once_lose_nothing() {
    // Worker one calls its tool and waits; worker two starts meanwhile and
    // calls the same tool on the same file while the first is still inside it.
    let h = harness(
        Duration::from_millis(100),
        vec![
            dispatch_two(),
            append("a", "from A"),
            append("b", "from B"),
            Scripted::Text(vec!["done".to_owned()]),
            Scripted::Text(vec!["done".to_owned()]),
            Scripted::Text(vec!["all done".to_owned()]),
        ],
    );
    let (turn, mut rx) = start(&h, CancellationToken::new()).await;
    while rx.recv().await.is_some() {}
    turn.await.unwrap();

    let text = std::fs::read_to_string(h.root.path().join("log.txt")).unwrap();
    let mut lines: Vec<_> = text.lines().collect();
    lines.sort_unstable();
    assert_eq!(lines, ["from A", "from B"], "{text:?}");
}

#[tokio::test]
async fn a_worker_waiting_for_a_busy_file_gives_up_when_cancelled() {
    // The first worker is stuck in a long call. The second waits for the lock.
    let h = harness(
        Duration::from_secs(30),
        vec![
            dispatch_two(),
            append("a", "from A"),
            append("b", "from B"),
            Scripted::Text(vec!["never".to_owned()]),
        ],
    );
    let cancel = CancellationToken::new();
    let (turn, mut rx) = start(&h, cancel.clone()).await;
    let stopper = tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(300)).await;
        cancel.cancel();
    });
    while rx.recv().await.is_some() {}
    tokio::time::timeout(Duration::from_secs(5), turn)
        .await
        .expect("both workers stopped promptly")
        .unwrap();
    stopper.await.unwrap();
    assert!(
        !h.root.path().join("log.txt").exists(),
        "a cancelled call writes nothing"
    );
}
