//! The MCP client against a real server process over stdio.

// Tests may unwrap and expect: a panic is the failure report.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use ws_core::ToolHost as _;
use ws_mcp::{McpConfig, McpTools};

fn config(fake_extra: &str, extra: &str) -> McpConfig {
    let script = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fake_server.py").replace('\\', "/");
    McpConfig::parse(&format!(
        r#"{{"mcpServers": {{"fake": {{"command": "python", "args": ["{script}"]{fake_extra}}}{extra}}}}}"#
    ))
    .unwrap()
}

#[tokio::test]
async fn tools_are_listed_called_and_ask_unless_the_user_approved_them() {
    // find_symbol says it is read-only; that alone must not skip approval.
    let (host, _) = McpTools::connect(&config("", "")).await;
    assert!(
        host.needs_approval("fake__find_symbol"),
        "a server's claim is not enough"
    );

    let (host, status) =
        McpTools::connect(&config(r#", "autoApprove": ["find_symbol"]"#, "")).await;
    assert_eq!(status.len(), 1);
    assert_eq!(status[0].error, None, "{status:?}");
    assert_eq!(status[0].tools, 2);

    let names: Vec<_> = host.specs().into_iter().map(|s| s.name).collect();
    assert_eq!(names, ["fake__find_symbol", "fake__replace_symbol_body"]);

    assert!(
        !host.needs_approval("fake__find_symbol"),
        "the user's list runs freely"
    );
    assert!(
        host.needs_approval("fake__replace_symbol_body"),
        "unlisted asks"
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
    let (host, status) = McpTools::connect(&config(
        "",
        r#", "broken": {"command": "no-such-binary-ws"}"#,
    ))
    .await;
    let broken = status.iter().find(|s| s.id == "broken").unwrap();
    assert!(broken.error.is_some());
    let fake = status.iter().find(|s| s.id == "fake").unwrap();
    assert_eq!(fake.tools, 2, "the healthy server still connects");
    assert_eq!(host.specs().len(), 2);
}

#[tokio::test]
async fn an_editing_call_names_its_file_inside_the_project() {
    let script = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fake_server.py").replace('\\', "/");
    let cfg = McpConfig::parse(&format!(
        r#"{{"mcpServers": {{"fake": {{"command": "python",
            "args": ["{script}", "--project", "C:/repo"]}}}}}}"#
    ))
    .unwrap();
    let (host, _) = McpTools::connect(&cfg).await;
    let args = serde_json::json!({"relative_path": "src/a.rs"});

    assert_eq!(
        host.file_for("fake__replace_symbol_body", &args),
        Some(std::path::Path::new("C:/repo").join("src/a.rs"))
    );
    let escape = serde_json::json!({"relative_path": "../outside.rs"});
    assert_eq!(host.file_for("fake__replace_symbol_body", &escape), None);
}
