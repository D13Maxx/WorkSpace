//! MCP client: connects to tool servers such as Serena and offers all of their
//! tools to the orchestrator as one [`ToolHost`].
//!
//! The config file uses the same shape as Claude Code's `.mcp.json`, so a
//! server's own setup instructions can be pasted in unchanged:
//!
//! ```json
//! { "mcpServers": { "serena": { "command": "serena", "args": ["start-mcp-server"] } } }
//! ```

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
pub struct ServerConfig {
    pub command: String,
    #[serde(default)]
    pub args: Vec<String>,
    #[serde(default)]
    pub env: BTreeMap<String, String>,
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

/// One offered tool: which server has it, and what to call it there.
#[derive(Debug, Clone)]
struct Offered {
    spec: ToolSpec,
    server: usize,
    tool_name: String,
    read_only: bool,
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
        let mut cmd = tokio::process::Command::new(&cfg.command);
        cmd.args(&cfg.args).envs(&cfg.env);
        let transport = TokioChildProcess::new(cmd)
            .map_err(|e| format!("could not run `{}`: {e}", cfg.command))?;
        let service = ().serve(transport).await.map_err(|e| e.to_string())?;
        let peer = service.peer().clone();
        let listed = peer.list_all_tools().await.map_err(|e| e.to_string())?;

        let server = self.servers.len();
        let prefix = sanitise(id);
        for tool in &listed {
            self.tools.push(offered(&prefix, server, tool));
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

fn offered(prefix: &str, server: usize, tool: &Tool) -> Offered {
    let read_only = tool
        .annotations
        .as_ref()
        .and_then(|a| a.read_only_hint)
        .unwrap_or(false);
    Offered {
        spec: ToolSpec {
            // Prefixed so two servers can both have a `read_file`.
            name: format!("{prefix}__{}", sanitise(&tool.name)),
            description: tool.description.as_deref().unwrap_or_default().to_owned(),
            input_schema: serde_json::Value::Object((*tool.input_schema).clone()),
        },
        server,
        tool_name: tool.name.to_string(),
        read_only,
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
            .is_none_or(|t| !t.read_only || runs_commands(&t.tool_name))
    }

    /// Serena's editing tools name their file as `relative_path`.
    // shortcut: one file per call, so a multi-file rename only snapshots the file named; upgrade with a VCS-level snapshot.
    fn file_for(&self, name: &str, args: &serde_json::Value) -> Option<std::path::PathBuf> {
        let tool = self
            .tools
            .iter()
            .find(|t| t.spec.name == name && !t.read_only)?;
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
                }
            }
            Err(e) => ToolOutput::error(format!("{name} failed: {e}")),
        }
    }
}

#[cfg(test)]
mod tests {
    // Spec s14 permits unwrap/expect in tests.
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;

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
