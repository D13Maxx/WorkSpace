//! Every tool server's tools behind three, in Code Mode (`ws_codemode`):
//! `mcp_search` runs a script over the catalog, `mcp_execute` runs a script
//! that calls tools with `await call(server, tool, args)`, and `mcp_call`
//! makes one plain call for a model whose scripts fail. The model sees a
//! fixed, small tool list however many servers and tools there are, and a
//! script returns only what it picked out of what the tools said.
//!
//! Approval: a tool the user listed in `autoApprove` (and that runs no
//! commands) runs unasked, as before. A script asks once when its text names
//! a tool that needs approval; one that names none runs unasked, and then may
//! call only tools that need no approval, so a name built at runtime cannot
//! slip past.

use std::sync::{Arc, Mutex};

use ws_context::ToolSpec;
use ws_core::{Change, ToolHost, ToolOutput};

use crate::{McpTools, Offered};

const SEARCH: &str = "mcp_search";
const EXECUTE: &str = "mcp_execute";
const CALL: &str = "mcp_call";
/// Tool names listed in the descriptions, so a model knows what exists.
const MAX_NAMES: usize = 80;

/// The connected servers' tools, offered as Code Mode.
#[derive(Debug, Clone)]
pub struct CodeMode {
    tools: Arc<McpTools>,
}

impl CodeMode {
    #[must_use]
    pub fn new(tools: McpTools) -> Self {
        Self {
            tools: Arc::new(tools),
        }
    }

    fn server(&self, t: &Offered) -> &str {
        &self.tools.servers[t.server].id
    }

    /// A tool by its server's id and its own name.
    fn find(&self, server: &str, tool: &str) -> Option<&Offered> {
        self.tools
            .tools
            .iter()
            .find(|t| self.server(t) == server && t.tool_name == tool)
    }

    fn asks(&self, t: &Offered) -> bool {
        self.tools.needs_approval(&t.spec.name)
    }

    /// What a search script sees as `catalog`.
    fn catalog(&self) -> serde_json::Value {
        self.tools
            .tools
            .iter()
            .map(|t| {
                serde_json::json!({
                    "server": self.server(t),
                    "tool": t.tool_name,
                    "description": t.spec.description,
                    "input_schema": t.spec.input_schema,
                    "asks_first": self.asks(t),
                })
            })
            .collect()
    }

    /// Each server and its tools' names, for the descriptions.
    fn summary(&self) -> String {
        let mut out = Vec::new();
        let mut listed = 0;
        for (i, server) in self.tools.servers.iter().enumerate() {
            let names: Vec<&str> = self
                .tools
                .tools
                .iter()
                .filter(|t| t.server == i)
                .map(|t| t.tool_name.as_str())
                .collect();
            let shown: Vec<&str> = names
                .iter()
                .take(MAX_NAMES.saturating_sub(listed))
                .copied()
                .collect();
            listed += shown.len();
            let more = names.len() - shown.len();
            let rest = if more > 0 {
                format!(", and {more} more")
            } else {
                String::new()
            };
            out.push(format!("{} ({}{rest})", server.id, shown.join(", ")));
        }
        out.join("; ")
    }

    /// Whether `code` names a tool that needs approval. Generous: any
    /// mention counts, and asking more than needed is the safe mistake.
    fn names_a_tool_that_asks(&self, code: &str) -> bool {
        self.tools
            .tools
            .iter()
            .any(|t| self.asks(t) && code.contains(t.tool_name.as_str()))
    }
}

/// Where a script's `call(server, tool, args)` goes.
struct Scripted {
    mode: CodeMode,
    /// The user approved the script, so any tool may run.
    approved: bool,
    changes: Mutex<Vec<Change>>,
}

#[async_trait::async_trait]
impl ws_codemode::Host for Scripted {
    async fn call(&self, args: serde_json::Value) -> Result<serde_json::Value, String> {
        let (Some(server), Some(tool)) = (args[0].as_str(), args[1].as_str()) else {
            return Err("call(server, tool, args): server and tool are names".to_owned());
        };
        let Some(t) = self.mode.find(server, tool) else {
            return Err(format!(
                "{server} has no tool called {tool}; look it up with {SEARCH}"
            ));
        };
        if !self.approved && self.mode.asks(t) {
            return Err(format!(
                "{server}/{tool} asks the user first, and this script was not approved: \
                 name the tool as a string in the script so the user is asked, or use {CALL}"
            ));
        }
        let arguments = match &args[2] {
            a @ serde_json::Value::Object(_) => a.clone(),
            serde_json::Value::Null => serde_json::json!({}),
            _ => return Err("a tool's arguments must be an object".to_owned()),
        };
        // An editing tool names its file: snapshot it, so the edit can be undone.
        let path = self.mode.tools.file_for(&t.spec.name, &arguments);
        let before = path
            .as_deref()
            .and_then(|p| ws_core::edits::snapshot(p).ok());
        let out = self.mode.tools.call(&t.spec.name, arguments).await;
        if let (Some(path), Some(before)) = (path, before) {
            match ws_core::edits::snapshot(&path) {
                Ok(after) if after != before => {
                    if let Ok(mut changes) = self.changes.lock() {
                        changes.push(Change {
                            path,
                            before,
                            after,
                        });
                    }
                }
                _ => {}
            }
        }
        if out.is_error {
            return Err(out.text);
        }
        Ok(serde_json::from_str(&out.text).unwrap_or(serde_json::Value::String(out.text)))
    }
}

fn spec(name: &str, description: String, schema: serde_json::Value) -> ToolSpec {
    ToolSpec {
        name: name.to_owned(),
        description,
        input_schema: schema,
    }
}

fn code_schema(what: &str) -> serde_json::Value {
    serde_json::json!({
        "type": "object",
        "properties": {"code": {"type": "string", "description": what}},
        "required": ["code"]
    })
}

fn reported(out: &ws_codemode::Outcome, changes: Vec<Change>) -> ToolOutput {
    ToolOutput {
        text: out.report(),
        is_error: out.result.is_err(),
        changes,
    }
}

#[async_trait::async_trait]
impl ToolHost for CodeMode {
    fn specs(&self) -> Vec<ToolSpec> {
        if self.tools.tools.is_empty() {
            return Vec::new();
        }
        let servers = self.summary();
        vec![
            spec(
                SEARCH,
                format!(
                    "Search the connected tool servers' tools with JavaScript. `code` is an async \
                     arrow function over `catalog`, an array of {{server, tool, description, \
                     input_schema, asks_first}}; return only what you need, e.g. \
                     async () => catalog.filter(t => /click|type/.test(t.tool)).map(t => [t.server, t.tool]). \
                     Servers: {servers}."
                ),
                code_schema("JavaScript async arrow function over `catalog`"),
            ),
            spec(
                EXECUTE,
                format!(
                    "Run JavaScript that calls tool servers' tools: `await call(server, tool, args)` \
                     resolves to the tool's result (parsed JSON when it is JSON) and throws on error. \
                     Most tools answer in plain text (Markdown or prose, not HTML or JSON), so the \
                     result is usually a string: search it with regexes or string methods, and if \
                     unsure of its shape, first return a short slice of it to look. \
                     Chain calls, loop, use Promise.all, and return only what you need. A script \
                     asks the user once if it names a tool that asks first; write tool names as \
                     plain strings. Servers: {servers}."
                ),
                code_schema(
                    "JavaScript async arrow function using `await call(server, tool, args)`",
                ),
            ),
            spec(
                CALL,
                "Call one tool server's tool plainly, if a script keeps failing.".to_owned(),
                serde_json::json!({
                    "type": "object",
                    "properties": {
                        "server": {"type": "string"},
                        "tool": {"type": "string"},
                        "arguments": {"type": "object", "description": "Matching the tool's input schema"}
                    },
                    "required": ["server", "tool", "arguments"]
                }),
            ),
        ]
    }

    fn needs_approval(&self, name: &str) -> bool {
        name != SEARCH
    }

    fn needs_approval_for(&self, name: &str, args: &serde_json::Value) -> bool {
        match name {
            SEARCH => false,
            EXECUTE => self.names_a_tool_that_asks(args["code"].as_str().unwrap_or_default()),
            CALL => match (args["server"].as_str(), args["tool"].as_str()) {
                (Some(server), Some(tool)) => self.find(server, tool).is_none_or(|t| self.asks(t)),
                _ => true,
            },
            _ => true,
        }
    }

    /// A plain call to an editing tool names its file, as a direct call did.
    fn file_for(&self, name: &str, args: &serde_json::Value) -> Option<std::path::PathBuf> {
        if name != CALL {
            return None;
        }
        let t = self.find(args["server"].as_str()?, args["tool"].as_str()?)?;
        self.tools.file_for(&t.spec.name, &args["arguments"])
    }

    async fn call(&self, name: &str, args: serde_json::Value) -> ToolOutput {
        let code = args["code"].as_str().unwrap_or_default();
        match name {
            SEARCH => reported(
                &ws_codemode::run(
                    code,
                    vec![("catalog".to_owned(), self.catalog())],
                    None,
                    ws_codemode::Limits::default(),
                )
                .await,
                Vec::new(),
            ),
            EXECUTE => {
                // Only called once approved, when approval was needed.
                let host = Arc::new(Scripted {
                    mode: self.clone(),
                    approved: self.needs_approval_for(EXECUTE, &args),
                    changes: Mutex::new(Vec::new()),
                });
                let out = ws_codemode::run(
                    code,
                    Vec::new(),
                    Some((
                        "call".to_owned(),
                        host.clone() as Arc<dyn ws_codemode::Host>,
                    )),
                    ws_codemode::Limits::default(),
                )
                .await;
                let changes = host
                    .changes
                    .lock()
                    .map(|mut c| std::mem::take(&mut *c))
                    .unwrap_or_default();
                reported(&out, changes)
            }
            CALL => {
                let (Some(server), Some(tool)) = (args["server"].as_str(), args["tool"].as_str())
                else {
                    return ToolOutput::error("mcp_call needs \"server\" and \"tool\"");
                };
                let Some(t) = self.find(server, tool) else {
                    return ToolOutput::error(format!(
                        "{server} has no tool called {tool}; look it up with {SEARCH}"
                    ));
                };
                let arguments = match &args["arguments"] {
                    a @ serde_json::Value::Object(_) => a.clone(),
                    _ => serde_json::json!({}),
                };
                self.tools.call(&t.spec.name, arguments).await
            }
            _ => ToolOutput::error(format!("there is no tool called {name}")),
        }
    }
}
