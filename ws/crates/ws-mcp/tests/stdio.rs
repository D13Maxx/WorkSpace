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

// ---- Code Mode: the same server behind mcp_search, mcp_execute, mcp_call ----

async fn code_mode(extra: &str) -> ws_mcp::CodeMode {
    let (host, status) = McpTools::connect(&config(extra, "")).await;
    assert_eq!(status[0].error, None, "{status:?}");
    ws_mcp::CodeMode::new(host)
}

fn code(c: &str) -> serde_json::Value {
    serde_json::json!({ "code": c })
}

#[tokio::test]
async fn code_mode_offers_three_tools_and_searches_the_catalog() {
    let mode = code_mode(r#", "autoApprove": ["find_symbol"]"#).await;
    let names: Vec<_> = mode.specs().into_iter().map(|s| s.name).collect();
    assert_eq!(names, ["mcp_search", "mcp_execute", "mcp_call"]);
    assert!(
        mode.specs()[0]
            .description
            .contains("fake (find_symbol(name_path), replace_symbol_body())"),
        "the descriptions name every server's tools and their required arguments"
    );
    let out = mode
        .call(
            "mcp_search",
            code(
                "async () => catalog.filter(t => !t.asks_first).map(t => t.server + '/' + t.tool)",
            ),
        )
        .await;
    assert_eq!(out.text, r#"["fake/find_symbol"]"#);
    assert!(!mode.needs_approval_for("mcp_search", &code("x")));
}

#[tokio::test]
async fn a_script_asks_once_if_it_names_a_tool_that_asks() {
    let mode = code_mode(r#", "autoApprove": ["find_symbol"]"#).await;
    let quiet = code("async () => (await call('fake', 'find_symbol', {name_path: 'A'}))");
    let loud = code("async () => call('fake', 'replace_symbol_body', {})");
    assert!(
        !mode.needs_approval_for("mcp_execute", &quiet),
        "auto-approved only"
    );
    assert!(mode.needs_approval_for("mcp_execute", &loud));

    // Two calls chained, a short answer back.
    let out = mode
        .call(
            "mcp_execute",
            code(
                "async () => { const a = await call('fake', 'find_symbol', {name_path: 'A'});\
                 const b = await call('fake', 'find_symbol', {name_path: 'B'});\
                 return [a.startsWith('find_symbol'), b.endsWith('\"B\"}')]; }",
            ),
        )
        .await;
    assert!(!out.is_error, "{out:?}");
    assert_eq!(out.text, "[true,true]");

    // A name built at runtime escapes the scan, so the unapproved script may
    // not call it.
    let sneaky = code("async () => call('fake', 'replace' + '_symbol' + '_body', {})");
    assert!(!mode.needs_approval_for("mcp_execute", &sneaky));
    let out = mode.call("mcp_execute", sneaky).await;
    assert!(
        out.is_error && out.text.contains("was not approved"),
        "{out:?}"
    );
}

#[tokio::test]
async fn a_plain_call_asks_as_its_tool_does() {
    let mode = code_mode(r#", "autoApprove": ["find_symbol"]"#).await;
    let call = |tool: &str| serde_json::json!({"server": "fake", "tool": tool, "arguments": {"name_path": "X"}});
    assert!(!mode.needs_approval_for("mcp_call", &call("find_symbol")));
    assert!(mode.needs_approval_for("mcp_call", &call("replace_symbol_body")));
    assert!(
        mode.needs_approval_for("mcp_call", &call("missing")),
        "unknown asks"
    );
    let out = mode.call("mcp_call", call("find_symbol")).await;
    assert_eq!(out.text, r#"find_symbol {"name_path": "X"}"#);
}

#[tokio::test]
async fn an_edit_inside_a_script_comes_back_as_a_change() {
    let root = tempfile::tempdir().unwrap();
    let project = root.path().to_string_lossy().replace('\\', "/");
    std::fs::write(root.path().join("a.rs"), "fn old() {}").unwrap();
    let script = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fake_server.py").replace('\\', "/");
    let cfg = McpConfig::parse(&format!(
        r#"{{"mcpServers": {{"fake": {{"command": "python",
            "args": ["{script}", "--project", "{project}"]}}}}}}"#
    ))
    .unwrap();
    let mode = ws_mcp::CodeMode::new(McpTools::connect(&cfg).await.0);
    let out = mode
        .call(
            "mcp_execute",
            code(
                "async () => { await call('fake', 'replace_symbol_body', \
                 {relative_path: 'a.rs', body: 'fn new() {}'}); return 'ok' }",
            ),
        )
        .await;
    assert!(!out.is_error, "{out:?}");
    assert_eq!(out.changes.len(), 1);
    let change = &out.changes[0];
    assert_eq!(change.path, root.path().join("a.rs"));
    assert_eq!(change.before.as_deref(), Some(&b"fn old() {}"[..]));
    assert_eq!(change.after.as_deref(), Some(&b"fn new() {}"[..]));

    let plain = serde_json::json!({"server": "fake", "tool": "replace_symbol_body",
        "arguments": {"relative_path": "a.rs"}});
    assert_eq!(
        mode.file_for("mcp_call", &plain),
        Some(root.path().join("a.rs")),
        "a plain editing call names its file for the usual snapshot"
    );
}
