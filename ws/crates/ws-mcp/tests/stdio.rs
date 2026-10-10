//! The MCP client against a real server process over stdio.

// Spec s14 permits unwrap/expect in tests.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use ws_core::ToolHost as _;
use ws_mcp::{McpConfig, McpTools};

fn config(extra: &str) -> McpConfig {
    let script = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fake_server.py").replace('\\', "/");
    McpConfig::parse(&format!(
        r#"{{"mcpServers": {{"fake": {{"command": "python", "args": ["{script}"]}}{extra}}}}}"#
    ))
    .unwrap()
}

#[tokio::test]
async fn tools_are_listed_called_and_gated_by_their_annotations() {
    let (host, status) = McpTools::connect(&config("")).await;
    assert_eq!(status.len(), 1);
    assert_eq!(status[0].error, None, "{status:?}");
    assert_eq!(status[0].tools, 2);

    let names: Vec<_> = host.specs().into_iter().map(|s| s.name).collect();
    assert_eq!(names, ["fake__find_symbol", "fake__replace_symbol_body"]);

    assert!(
        !host.needs_approval("fake__find_symbol"),
        "read-only runs freely"
    );
    assert!(
        host.needs_approval("fake__replace_symbol_body"),
        "edits ask"
    );
    assert!(host.needs_approval("fake__no_such_tool"), "unknown asks");

    let out = host
        .call(
            "fake__find_symbol",
            serde_json::json!({"name_path": "Orchestrator"}),
        )
        .await;
    assert!(!out.is_error, "{out:?}");
    assert_eq!(out.text, r#"find_symbol {"name_path": "Orchestrator"}"#);
}

#[tokio::test]
async fn a_server_that_will_not_start_is_reported_not_fatal() {
    let (host, status) =
        McpTools::connect(&config(r#", "broken": {"command": "no-such-binary-ws"}"#)).await;
    let broken = status.iter().find(|s| s.id == "broken").unwrap();
    assert!(broken.error.is_some());
    let fake = status.iter().find(|s| s.id == "fake").unwrap();
    assert_eq!(fake.tools, 2, "the healthy server still connects");
    assert_eq!(host.specs().len(), 2);
}
