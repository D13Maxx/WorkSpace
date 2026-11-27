//! MCP client: connects to tool servers such as Serena and offers all of their
//! tools to the orchestrator as one [`ToolHost`].
//!
//! The config file uses the same shape as Claude Code's `.mcp.json`, so a
//! server's own setup instructions can be pasted in unchanged:
//!
//! ```json
//! { "mcpServers": { "serena": { "command": "serena", "args": ["start-mcp-server"] } } }
//! ```

mod codemode;
pub use codemode::CodeMode;

use rmcp::ServiceExt as _;
use rmcp::model::{CallToolRequestParams, Tool};
use rmcp::service::{Peer, RoleClient, RunningService};
use rmcp::transport::TokioChildProcess;
use serde::Deserialize;
use std::collections::BTreeMap;
use ws_context::ToolSpec;
use ws_core::{ToolHost, ToolOutput};

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct McpConfig {
    #[serde(default)]
    pub mcp_servers: BTreeMap<String, ServerConfig>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ServerConfig {
    pub command: String,
    #[serde(default)]
    pub args: Vec<String>,
    #[serde(default)]
    pub env: BTreeMap<String, String>,
    /// Tools that run without asking. Only the user's list counts: a server
    /// calling its own tool read-only is a claim, not a promise.
    #[serde(default)]
    pub auto_approve: Vec<String>,
}

impl McpConfig {
    /// Parse a config file's contents.
    ///
    /// # Errors
    /// Fails if the text is not a valid `mcpServers` config.
    pub fn parse(text: &str) -> Result<Self, serde_json::Error> {
        serde_json::from_str(text)
    }
}

/// A server id Settings accepts: what tool names are prefixed with.
fn valid_id(id: &str) -> Result<(), String> {
    let ok = !id.is_empty()
        && id.len() <= 40
        && id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'));
    if ok {
        Ok(())
    } else {
        Err(format!(
            "\"{id}\" is not a server name: use letters, digits, - _ or ."
        ))
    }
}

/// One server's settings, checked as Workspace will start it.
fn valid_server(id: &str, server: &serde_json::Value) -> Result<(), String> {
    valid_id(id)?;
    serde_json::from_value::<ServerConfig>(server.clone())
        .map(drop)
        .map_err(|e| {
            if server.get("command").is_none() {
                format!(
                    "{id} has no command: Workspace starts each server itself, so it needs one (servers reached by URL are not supported)"
                )
            } else {
                format!("{id}: {e}")
            }
        })
}

/// The servers in text pasted from a tool's README: `{"mcpServers": {...}}`
/// or just the map of servers. Each is checked.
///
/// # Errors
/// Not JSON, not that shape, or a server Workspace cannot start.
pub fn pasted_servers(text: &str) -> Result<serde_json::Map<String, serde_json::Value>, String> {
    let value: serde_json::Value =
        serde_json::from_str(text.trim()).map_err(|e| format!("not valid JSON: {e}"))?;
    let map = match value.get("mcpServers") {
        Some(inner) => inner.clone(),
        None => value,
    };
    let serde_json::Value::Object(servers) = map else {
        return Err("expected {\"mcpServers\": {...}} or a map of servers".to_owned());
    };
    if servers.is_empty() {
        return Err("there are no servers in it".to_owned());
    }
    for (id, server) in &servers {
        valid_server(id, server)?;
    }
    Ok(servers)
}

/// `mcp.json`'s new text: `id` set to `server`, or removed when None, with
/// every other server and field kept as it was.
///
/// # Errors
/// The file is not valid JSON, or the server is not one Workspace can start.
pub fn edit_config(
    file: Option<&str>,
    id: &str,
    server: Option<serde_json::Value>,
) -> Result<String, String> {
    let mut root: serde_json::Value = match file.map(str::trim).filter(|t| !t.is_empty()) {
        Some(text) => serde_json::from_str(text)
            .map_err(|e| format!("mcp.json is not valid JSON, so it was left alone: {e}"))?,
        None => serde_json::json!({}),
    };
    let Some(obj) = root.as_object_mut() else {
        return Err("mcp.json is not a JSON object, so it was left alone".to_owned());
    };
    let servers = obj
        .entry("mcpServers")
        .or_insert_with(|| serde_json::json!({}));
    let Some(servers) = servers.as_object_mut() else {
        return Err("mcpServers in mcp.json is not an object".to_owned());
    };
    match server {
        Some(server) => {
            valid_server(id, &server)?;
            servers.insert(id.to_owned(), server);
        }
        None => {
            servers.remove(id);
        }
    }
    serde_json::to_string_pretty(&root).map_err(|e| e.to_string())
}

/// One offered tool: which server has it, and what to call it there.
#[derive(Debug, Clone)]
struct Offered {
    spec: ToolSpec,
    server: usize,
    tool_name: String,
    /// The user listed it in `autoApprove`.
    approved: bool,
}

struct Server {
    id: String,
    /// The project it works on, from Serena's `--project` flag, so an edit's
    /// `relative_path` can be resolved for a diff and undo.
    root: Option<std::path::PathBuf>,
    // Holds the child process; dropping it stops the server.
    _service: RunningService<RoleClient, ()>,
    peer: Peer<RoleClient>,
}

/// Every connected server's tools, behind one [`ToolHost`].
pub struct McpTools {
    servers: Vec<Server>,
    tools: Vec<Offered>,
}

impl std::fmt::Debug for McpTools {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("McpTools")
            .field(
                "servers",
                &self.servers.iter().map(|s| &s.id).collect::<Vec<_>>(),
            )
            .field("tools", &self.tools.len())
            .finish()
    }
}

/// Status of one configured server after connecting, for the settings panel.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct ServerStatus {
    pub id: String,
    pub tools: usize,
    /// Why it failed to start, if it did.
    pub error: Option<String>,
}

impl McpTools {
    /// Start every configured server and list its tools. A server that fails
    /// is reported in the status list and skipped, never fatal to the rest.
    pub async fn connect(config: &McpConfig) -> (Self, Vec<ServerStatus>) {
        let mut host = Self {
            servers: Vec::new(),
            tools: Vec::new(),
        };
        let mut status = Vec::new();
        for (id, cfg) in &config.mcp_servers {
            match host.add(id, cfg).await {
                Ok(n) => status.push(ServerStatus {
                    id: id.clone(),
                    tools: n,
                    error: None,
                }),
                Err(e) => {
                    tracing::warn!(server = %id, error = %e, "MCP server did not start");
                    status.push(ServerStatus {
                        id: id.clone(),
                        tools: 0,
                        error: Some(e),
                    });
                }
            }
        }
        (host, status)
    }

    async fn add(&mut self, id: &str, cfg: &ServerConfig) -> Result<usize, String> {
        let spawn = |program: &str| {
            let mut cmd = tokio::process::Command::new(program);
            // Restarting from Settings drops the old servers: they must exit.
            cmd.args(&cfg.args).envs(&cfg.env).kill_on_drop(true);
            #[cfg(windows)]
            cmd.creation_flags(0x0800_0000); // no console window
            TokioChildProcess::new(cmd)
        };
        // `npx` is `npx.cmd` on Windows, which a bare name does not find.
        let transport = match spawn(&cfg.command) {
            Err(e) if cfg!(windows) && e.kind() == std::io::ErrorKind::NotFound => {
                spawn(&format!("{}.cmd", cfg.command))
            }
            other => other,
        }
        .map_err(|e| format!("could not run `{}`: {e}", cfg.command))?;
        let service = ().serve(transport).await.map_err(|e| e.to_string())?;
        let peer = service.peer().clone();
        let listed = peer.list_all_tools().await.map_err(|e| e.to_string())?;

        let server = self.servers.len();
        let prefix = sanitise(id);
        for tool in &listed {
            let approved = cfg.auto_approve.iter().any(|n| *n == *tool.name);
            self.tools.push(offered(&prefix, server, tool, approved));
        }
        self.servers.push(Server {
            id: id.to_owned(),
            root: project_root(&cfg.args),
            _service: service,
            peer,
        });
        Ok(listed.len())
    }
}

fn offered(prefix: &str, server: usize, tool: &Tool, approved: bool) -> Offered {
    Offered {
        spec: ToolSpec {
            // Prefixed so two servers can both have a `read_file`.
            name: format!("{prefix}__{}", sanitise(&tool.name)),
            description: tool.description.as_deref().unwrap_or_default().to_owned(),
            input_schema: serde_json::Value::Object((*tool.input_schema).clone()),
        },
        server,
        tool_name: tool.name.to_string(),
        approved,
    }
}

/// The `--project` folder from a server's arguments, either spelling.
fn project_root(args: &[String]) -> Option<std::path::PathBuf> {
    args.iter().enumerate().find_map(|(i, a)| {
        a.strip_prefix("--project=")
            .map(str::to_owned)
            .or_else(|| {
                (a == "--project")
                    .then(|| args.get(i + 1).cloned())
                    .flatten()
            })
            .map(std::path::PathBuf::from)
    })
}

/// A path relative to the project with no way out of it: undo writes here.
fn inside(root: &std::path::Path, rel: &str) -> Option<std::path::PathBuf> {
    use std::path::Component;
    let rel = std::path::Path::new(rel);
    rel.components()
        .all(|c| matches!(c, Component::Normal(_) | Component::CurDir))
        .then(|| root.join(rel))
}

/// Providers accept only `[A-Za-z0-9_-]` in tool names.
fn sanitise(s: &str) -> String {
    s.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' {
                c
            } else {
                '_'
            }
        })
        .collect()
}

/// Anything that can run commands asks first, whatever the server claims:
/// a mislabelled shell tool is the one mistake that cannot be undone.
fn runs_commands(name: &str) -> bool {
    let n = name.to_ascii_lowercase();
    n.contains("shell") || n.contains("exec") || n.contains("command")
}

#[async_trait::async_trait]
impl ToolHost for McpTools {
    fn specs(&self) -> Vec<ToolSpec> {
        self.tools.iter().map(|t| t.spec.clone()).collect()
    }

    fn needs_approval(&self, name: &str) -> bool {
        // Unknown names ask too: approval is the safe default.
        self.tools
            .iter()
            .find(|t| t.spec.name == name)
            .is_none_or(|t| !t.approved || runs_commands(&t.tool_name))
    }

    /// Serena's editing tools name their file as `relative_path`.
    // shortcut: one file per call, so a multi-file rename only snapshots the file named; upgrade with a VCS-level snapshot.
    fn file_for(&self, name: &str, args: &serde_json::Value) -> Option<std::path::PathBuf> {
        let tool = self.tools.iter().find(|t| t.spec.name == name)?;
        let root = self.servers[tool.server].root.as_deref()?;
        inside(root, args.get("relative_path")?.as_str()?)
    }

    async fn call(&self, name: &str, args: serde_json::Value) -> ToolOutput {
        let Some(tool) = self.tools.iter().find(|t| t.spec.name == name) else {
            return ToolOutput::error(format!("there is no tool called {name}"));
        };
        let serde_json::Value::Object(args) = args else {
            return ToolOutput::error("tool arguments must be a JSON object");
        };
        let params = CallToolRequestParams::new(tool.tool_name.clone()).with_arguments(args);
        match self.servers[tool.server].peer.call_tool(params).await {
            Ok(result) => {
                let text = result
                    .content
                    .iter()
                    .filter_map(|c| c.as_text().map(|t| t.text.as_str()))
                    .collect::<Vec<_>>()
                    .join("\n");
                ToolOutput {
                    text,
                    is_error: result.is_error.unwrap_or(false),
                    changes: Vec::new(),
                }
            }
            Err(e) => ToolOutput::error(format!("{name} failed: {e}")),
        }
    }
}

#[cfg(test)]
mod tests {
    // Tests may unwrap and expect: a panic is the failure report.
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;

    #[test]
    fn pasted_servers_take_either_shape_and_are_checked() {
        let both = [
            r#"{"mcpServers": {"serena": {"command": "uvx", "args": ["serena"]}}}"#,
            r#"{"serena": {"command": "uvx", "args": ["serena"]}}"#,
        ];
        for text in both {
            let got = pasted_servers(text).unwrap();
            assert_eq!(got.keys().collect::<Vec<_>>(), ["serena"], "{text}");
        }
        assert!(pasted_servers("nope").is_err());
        assert!(pasted_servers("{}").is_err(), "no servers");
        let remote = pasted_servers(r#"{"x": {"type": "http", "url": "https://a"}}"#).unwrap_err();
        assert!(remote.contains("no command"), "{remote}");
        assert!(
            pasted_servers(r#"{"a b": {"command": "x"}}"#).is_err(),
            "bad name"
        );
    }

    #[test]
    fn editing_one_server_keeps_the_rest_of_the_file() {
        let file = r#"{"mcpServers": {"keep": {"command": "k", "extra": 1}}, "other": true}"#;
        let added =
            edit_config(Some(file), "new", Some(serde_json::json!({"command": "n"}))).unwrap();
        let v: serde_json::Value = serde_json::from_str(&added).unwrap();
        assert_eq!(v["other"], true);
        assert_eq!(v["mcpServers"]["keep"]["extra"], 1);
        assert_eq!(v["mcpServers"]["new"]["command"], "n");
        assert!(McpConfig::parse(&added).is_ok(), "still a valid config");

        let removed = edit_config(Some(&added), "keep", None).unwrap();
        let v: serde_json::Value = serde_json::from_str(&removed).unwrap();
        assert!(v["mcpServers"].get("keep").is_none());

        assert!(edit_config(None, "first", Some(serde_json::json!({"command": "f"}))).is_ok());
        assert!(
            edit_config(Some("{broken"), "x", None).is_err(),
            "a broken file is not overwritten"
        );
        assert!(
            edit_config(None, "x", Some(serde_json::json!({"args": []}))).is_err(),
            "needs a command"
        );
    }

    #[test]
    fn the_config_reads_claude_codes_mcp_json_shape() {
        let cfg = McpConfig::parse(
            r#"{"mcpServers": {"serena": {"command": "serena",
                "args": ["start-mcp-server"], "env": {"X": "1"}}}}"#,
        )
        .unwrap();
        let serena = &cfg.mcp_servers["serena"];
        assert_eq!(serena.command, "serena");
        assert_eq!(serena.args, ["start-mcp-server"]);
        assert_eq!(serena.env["X"], "1");
        assert!(
            serena.auto_approve.is_empty(),
            "nothing runs unasked by default"
        );

        let cfg = McpConfig::parse(
            r#"{"mcpServers": {"c7": {"command": "npx", "autoApprove": ["query-docs"]}}}"#,
        )
        .unwrap();
        assert_eq!(cfg.mcp_servers["c7"].auto_approve, ["query-docs"]);
    }

    /// Starts the servers in the file named by `WS_MCP_CONFIG` and lists what
    /// asks first: `WS_MCP_CONFIG=mcp.json cargo test -p ws-mcp -- --ignored --nocapture`.
    #[tokio::test]
    #[ignore = "starts real tool servers"]
    async fn real_servers_start() {
        let path = std::env::var("WS_MCP_CONFIG").unwrap();
        let cfg = McpConfig::parse(&std::fs::read_to_string(path).unwrap()).unwrap();
        let (host, status) = McpTools::connect(&cfg).await;
        for s in &status {
            println!("{}: {} tools {:?}", s.id, s.tools, s.error);
            assert!(s.error.is_none(), "{} failed", s.id);
        }
        for t in &host.tools {
            println!(
                "  {} asks={}",
                t.spec.name,
                host.needs_approval(&t.spec.name)
            );
        }
    }

    #[test]
    fn tool_names_are_made_safe_for_providers() {
        assert_eq!(sanitise("my server.v2"), "my_server_v2");
        assert_eq!(sanitise("find-symbol"), "find-symbol");
    }

    #[test]
    fn the_project_root_is_read_from_either_flag_spelling() {
        let args = |a: &[&str]| a.iter().map(|s| (*s).to_owned()).collect::<Vec<_>>();
        assert_eq!(
            project_root(&args(&["start-mcp-server", "--project", "C:/repo"])),
            Some("C:/repo".into())
        );
        assert_eq!(
            project_root(&args(&["--project=C:/repo"])),
            Some("C:/repo".into())
        );
        assert_eq!(project_root(&args(&["--project-from-cwd"])), None);
        assert_eq!(
            project_root(&args(&["--project"])),
            None,
            "flag with no value"
        );
    }

    #[test]
    fn an_edit_path_cannot_leave_the_project() {
        let root = std::path::Path::new("/repo");
        assert_eq!(inside(root, "src/a.rs"), Some(root.join("src/a.rs")));
        for escape in ["../x", "src/../../x", "/etc/passwd"] {
            assert_eq!(inside(root, escape), None, "{escape}");
        }
    }

    #[test]
    fn shell_tools_ask_even_when_marked_read_only() {
        assert!(runs_commands("execute_shell_command"));
        assert!(!runs_commands("find_symbol"));
    }
}
