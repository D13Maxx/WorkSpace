//! Reverse Engineer Anything (REA), bundled with the app and offered only to
//! agent teams (`Orchestrator::with_team_tools`).
//!
//! REA has 263 tools, about 90k tokens of schemas, and many providers refuse
//! more than 128 tools a request. So a team never sees them directly. Until
//! the lead switches REA on it is one small tool; after that it is Code Mode
//! (`ws_codemode`): the model writes JavaScript that searches REA's catalog
//! (`rea_search`) or calls REA tools and keeps only what it needs
//! (`rea_execute`), plus a plain one-call tool for models whose scripts fail,
//! and REA's workflow guide. REA itself runs on the system's Node.js, started
//! on first use.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use ws_context::ToolSpec;
use ws_core::{ToolHost, ToolOutput};
use ws_mcp::{McpConfig, McpTools, ServerConfig};

const START: &str = "start_reverse_engineering";
const SEARCH: &str = "rea_search";
const EXECUTE: &str = "rea_execute";
const CALL: &str = "rea_call";
const GUIDE: &str = "rea_guide";
/// How McpTools prefixes the server's tools.
const PREFIX: &str = "rea__";

/// How to use REA here, ahead of REA's own guide (which assumes its tools are
/// offered directly).
const CODE_MODE: &str = "REA is on for this team, in Code Mode. Its 263 tools are not offered directly: you reach them by writing JavaScript.

- rea_search: `code` is a function over `catalog`, an array of {name, description, input_schema}. Return only what you need, e.g.
  async () => catalog.filter(t => /decompil/i.test(t.name + t.description)).map(t => ({name: t.name, description: t.description}))
  then read one tool's schema: async () => catalog.find(t => t.name === 'procedure_pseudo_code').input_schema
- rea_execute: `code` is a function that calls REA with `await rea(toolName, args)`, which resolves to the tool's result (parsed JSON when it is JSON) and throws on error. Chain calls, loop over results, use Promise.all, and return a small summary: REA's results carry long evidence records you rarely need whole. console.log output comes back too. The user approves each script once.
  async () => { const h = await rea('inspect_binary_hardening', {path: 'C:/app/app.exe'}); return h.summary ?? h; }
- rea_call: one plain call, {tool, arguments}, if a script keeps failing.
- rea_guide: REA's guides.

Workers you start from now on have these tools too; tell them to use REA in Code Mode.

REA's own guide follows. Where it names a REA tool, call it through rea_execute or rea_call.";

/// REA's tools for a team, started when the lead first needs them.
#[derive(Debug)]
pub struct Rea {
    /// The bundled package folder (holds `scripts/rea.mjs`); none in a build without it.
    dir: Option<PathBuf>,
    tools: tokio::sync::OnceCell<Arc<McpTools>>,
}

impl Rea {
    pub fn new(dir: Option<PathBuf>) -> Self {
        Self {
            dir,
            tools: tokio::sync::OnceCell::new(),
        }
    }

    /// Start REA once; every team after that shares it.
    async fn start(&self) -> Result<&Arc<McpTools>, String> {
        self.tools
            .get_or_try_init(|| async {
                let dir = self
                    .dir
                    .as_deref()
                    .ok_or("REA is not bundled with this build of Workspace.")?;
                let entry = dir.join("scripts").join("rea.mjs");
                let node = ws_tools::find(&["node"])
                    .ok_or("REA needs Node.js (22.23.4+, 24.22.1+ or 26.12.1+); none was found.")?;
                check_runs(&node, &entry).await?;
                let mut servers = std::collections::BTreeMap::new();
                servers.insert(
                    "rea".to_owned(),
                    ServerConfig {
                        command: node.to_string_lossy().into_owned(),
                        args: vec![entry.to_string_lossy().into_owned(), "mcp".to_owned()],
                        env: std::collections::BTreeMap::new(),
                        auto_approve: Vec::new(),
                    },
                );
                let (tools, status) = McpTools::connect(&McpConfig {
                    mcp_servers: servers,
                })
                .await;
                match status.into_iter().find_map(|s| s.error) {
                    Some(e) => Err(format!("REA did not start: {e}")),
                    None => Ok(Arc::new(tools)),
                }
            })
            .await
    }

    /// The workflow guide, or one of its references by file name.
    fn guide(&self, topic: Option<&str>) -> Result<String, String> {
        let skill = self
            .dir
            .as_deref()
            .ok_or("REA is not bundled")?
            .join("skills")
            .join("reverse-engineer-anything");
        let file = match topic.map(str::trim).filter(|t| !t.is_empty()) {
            None => skill.join("SKILL.md"),
            Some(t) => {
                let refs = skill.join("references");
                // Only a file that is there by that name: never a path.
                let name = format!("{}.md", t.trim_end_matches(".md"));
                std::fs::read_dir(&refs)
                    .map_err(|e| e.to_string())?
                    .filter_map(Result::ok)
                    .find(|e| e.file_name().to_string_lossy() == name)
                    .map(|e| e.path())
                    .ok_or_else(|| {
                        format!("no guide called {t}; there are: {}", references(&refs))
                    })?
            }
        };
        let text = std::fs::read_to_string(&file).map_err(|e| e.to_string())?;
        Ok(match topic {
            None => format!(
                "{}\n\nReferences, read with {GUIDE}: {}",
                body(&text),
                references(&skill.join("references"))
            ),
            Some(_) => text,
        })
    }
}

/// Run `rea --version` first, so a too-old Node is reported in REA's own
/// words rather than as a server that hung up.
async fn check_runs(node: &Path, entry: &Path) -> Result<(), String> {
    let mut cmd = tokio::process::Command::new(node);
    cmd.arg(entry).arg("--version").kill_on_drop(true);
    #[cfg(windows)]
    cmd.creation_flags(0x0800_0000); // no console window
    let out = tokio::time::timeout(std::time::Duration::from_secs(60), cmd.output())
        .await
        .map_err(|_| "REA did not answer within a minute.".to_owned())?
        .map_err(|e| format!("could not run Node.js: {e}"))?;
    if out.status.success() {
        return Ok(());
    }
    let said = String::from_utf8_lossy(if out.stderr.is_empty() {
        &out.stdout
    } else {
        &out.stderr
    })
    .trim()
    .to_owned();
    Err(format!("REA could not start: {said}"))
}

/// A Markdown file without its frontmatter.
fn body(text: &str) -> &str {
    text.strip_prefix("---")
        .and_then(|rest| rest.split_once("\n---"))
        .map_or(text, |(_, after)| {
            after.trim_start_matches(['-', '\r', '\n'])
        })
}

fn references(dir: &Path) -> String {
    let mut names: Vec<String> = std::fs::read_dir(dir)
        .into_iter()
        .flatten()
        .filter_map(Result::ok)
        .filter_map(|e| {
            e.file_name()
                .to_string_lossy()
                .strip_suffix(".md")
                .map(str::to_owned)
        })
        .collect();
    names.sort();
    names.join(", ")
}

/// REA's tools as a script sees them in `catalog`: names without our prefix.
fn catalog(specs: &[ToolSpec]) -> serde_json::Value {
    specs
        .iter()
        .map(|s| {
            serde_json::json!({
                "name": s.name.trim_start_matches(PREFIX),
                "description": s.description,
                "input_schema": s.input_schema,
            })
        })
        .collect()
}

/// One REA call by its plain name, for `rea_call` and for scripts alike.
async fn one_call(tools: &McpTools, tool: &str, arguments: serde_json::Value) -> ToolOutput {
    let arguments = match arguments {
        a @ serde_json::Value::Object(_) => a,
        serde_json::Value::Null => serde_json::json!({}),
        _ => return ToolOutput::error("a REA tool's arguments must be an object"),
    };
    let full = format!("{PREFIX}{}", tool.trim_start_matches(PREFIX));
    if !tools.specs().iter().any(|s| s.name == full) {
        return ToolOutput::error(format!(
            "REA has no tool called {tool}; find one with {SEARCH}"
        ));
    }
    tools.call(&full, arguments).await
}

/// Where a script's `rea(tool, args)` goes.
struct Scripted(Arc<McpTools>);

#[async_trait::async_trait]
impl ws_codemode::Host for Scripted {
    async fn call(&self, args: serde_json::Value) -> Result<serde_json::Value, String> {
        let Some(tool) = args[0].as_str() else {
            return Err("rea(toolName, args): the tool's name comes first".to_owned());
        };
        let out = one_call(&self.0, tool, args[1].clone()).await;
        if out.is_error {
            return Err(out.text);
        }
        // REA answers in JSON; a script works with it as an object.
        Ok(serde_json::from_str(&out.text).unwrap_or(serde_json::Value::String(out.text)))
    }
}

fn spec(name: &str, description: &str, schema: serde_json::Value) -> ToolSpec {
    ToolSpec {
        name: name.to_owned(),
        description: description.to_owned(),
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

/// A script's outcome as a tool result.
fn reported(out: &ws_codemode::Outcome) -> ToolOutput {
    ToolOutput {
        text: out.report(),
        is_error: out.result.is_err(),
        changes: Vec::new(),
    }
}

#[async_trait::async_trait]
impl ToolHost for Rea {
    fn specs(&self) -> Vec<ToolSpec> {
        if self.dir.is_none() {
            return Vec::new();
        }
        if self.tools.get().is_none() {
            return vec![spec(
                START,
                "Switch on Reverse Engineer Anything (REA) for this team. REA inspects software \
                 without its source code: native binaries (decompile, disassemble, strings, calls, \
                 cross-references), Electron and JavaScript apps, .NET assemblies, Android APKs, \
                 websites, firmware, packaged apps and more, and returns the evidence for each \
                 finding. Call this when the task is to find out how existing software works from \
                 its shipped files or its behaviour. Not for reading a project's own source. \
                 Returns how to use REA here and its workflow guide.",
                serde_json::json!({"type": "object", "properties": {}}),
            )];
        }
        vec![
            spec(
                SEARCH,
                "Search REA's 263 tools with JavaScript. `code` is an async arrow function over \
                 `catalog`, an array of {name, description, input_schema}; return only what you \
                 need, e.g. async () => catalog.filter(t => /apk/i.test(t.description)).map(t => t.name)",
                code_schema("JavaScript async arrow function over `catalog`"),
            ),
            spec(
                EXECUTE,
                "Run JavaScript that calls REA: `await rea(toolName, args)` resolves to the tool's \
                 result (parsed JSON) and throws on error. Chain and loop over calls and return a \
                 small summary, not whole evidence records. Asks the user once per script. Static \
                 inspection only reads the target; runtime capture runs it, so prefer static tools.",
                code_schema("JavaScript async arrow function using `await rea(toolName, args)`"),
            ),
            spec(
                CALL,
                "Call one REA tool plainly, if a script keeps failing. Asks the user first.",
                serde_json::json!({
                    "type": "object",
                    "properties": {
                        "tool": {"type": "string", "description": "The REA tool's name, as rea_search gave it"},
                        "arguments": {"type": "object", "description": "Its arguments, matching its input schema"}
                    },
                    "required": ["tool", "arguments"]
                }),
            ),
            spec(
                GUIDE,
                "REA's workflow guide: how to investigate, what evidence to keep, how to recover \
                 from a failed call. With no topic, the main guide; with a topic, one reference \
                 (the main guide lists them).",
                serde_json::json!({
                    "type": "object",
                    "properties": {"topic": {"type": "string", "description": "A reference's name; leave out for the main guide"}}
                }),
            ),
        ]
    }

    /// Starting REA, searching and reading the guide change nothing. A script
    /// or a call may run the target, so it asks: a script once for all its calls.
    fn needs_approval(&self, name: &str) -> bool {
        name == EXECUTE || name == CALL
    }

    async fn call(&self, name: &str, args: serde_json::Value) -> ToolOutput {
        if name == START {
            return match self.start().await {
                Ok(_) => match self.guide(None) {
                    Ok(guide) => ToolOutput::ok(format!("{CODE_MODE}\n\n{guide}")),
                    Err(e) => ToolOutput::ok(format!(
                        "{CODE_MODE}\n\n(REA's own guide could not be read: {e})"
                    )),
                },
                Err(e) => ToolOutput::error(e),
            };
        }
        if name == GUIDE {
            return match self.guide(args["topic"].as_str()) {
                Ok(text) => ToolOutput::ok(text),
                Err(e) => ToolOutput::error(e),
            };
        }
        let Some(tools) = self.tools.get() else {
            return ToolOutput::error(format!("REA is not on yet: call {START}"));
        };
        let code = args["code"].as_str().unwrap_or_default();
        match name {
            SEARCH => reported(
                &ws_codemode::run(
                    code,
                    vec![("catalog".to_owned(), catalog(&tools.specs()))],
                    None,
                    ws_codemode::Limits::default(),
                )
                .await,
            ),
            EXECUTE => {
                let host = Arc::new(Scripted(tools.clone())) as Arc<dyn ws_codemode::Host>;
                let out = ws_codemode::run(
                    code,
                    Vec::new(),
                    Some(("rea".to_owned(), host)),
                    ws_codemode::Limits::default(),
                )
                .await;
                let mut result = reported(&out);
                result.text = format!("{} REA calls.\n{}", out.calls, result.text);
                result
            }
            CALL => {
                let Some(tool) = args["tool"].as_str() else {
                    return ToolOutput::error("rea_call needs \"tool\"");
                };
                one_call(tools, tool, args["arguments"].clone()).await
            }
            _ => ToolOutput::error(format!("REA has no tool called {name}")),
        }
    }
}

#[cfg(test)]
mod tests {
    // Tests may unwrap and expect: a panic is the failure report.
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;

    #[tokio::test]
    async fn a_search_script_reads_the_catalog_by_plain_names() {
        let specs = [
            spec(
                "rea__procedure_pseudo_code",
                "Decompile one function to pseudocode",
                serde_json::json!({"type": "object", "required": ["address"]}),
            ),
            spec(
                "rea__inspect_android_package",
                "Read an APK manifest",
                serde_json::json!({"type": "object"}),
            ),
        ];
        let out = ws_codemode::run(
            "async () => catalog.filter(t => /decompile/i.test(t.description))\
               .map(t => [t.name, t.input_schema.required[0]])",
            vec![("catalog".to_owned(), catalog(&specs))],
            None,
            ws_codemode::Limits::default(),
        )
        .await;
        assert_eq!(
            out.result,
            Ok(r#"[["procedure_pseudo_code","address"]]"#.to_owned())
        );
    }

    #[test]
    fn the_guide_reads_only_files_that_are_there() {
        let dir = tempfile::tempdir().unwrap();
        let skill = dir.path().join("skills/reverse-engineer-anything");
        std::fs::create_dir_all(skill.join("references")).unwrap();
        std::fs::write(
            skill.join("SKILL.md"),
            "---\nname: x\n---\n\n# REA\nUse it.",
        )
        .unwrap();
        std::fs::write(skill.join("references/android-applications.md"), "APKs").unwrap();
        std::fs::write(dir.path().join("secret.md"), "no").unwrap();
        let rea = Rea::new(Some(dir.path().to_path_buf()));

        let main = rea.guide(None).unwrap();
        assert!(main.starts_with("# REA"), "frontmatter dropped: {main}");
        assert!(
            main.contains("android-applications"),
            "lists its references"
        );
        assert_eq!(rea.guide(Some("android-applications")).unwrap(), "APKs");
        for escape in ["../../secret", "../secret.md", "missing"] {
            assert!(rea.guide(Some(escape)).is_err(), "{escape}");
        }
    }

    #[test]
    fn before_it_starts_a_team_sees_one_tool_and_without_rea_none() {
        let rea = Rea::new(Some(PathBuf::from("/nowhere")));
        let names: Vec<String> = rea.specs().into_iter().map(|s| s.name).collect();
        assert_eq!(names, [START]);
        assert!(!rea.needs_approval(START));
        assert!(!rea.needs_approval(SEARCH) && !rea.needs_approval(GUIDE));
        assert!(rea.needs_approval(EXECUTE), "a script can run the target");
        assert!(rea.needs_approval(CALL));
        assert!(Rea::new(None).specs().is_empty());
    }

    /// The real bundled REA, end to end: `node scripts/build-rea.mjs`, then
    /// `cargo test -p ws-app rea_starts -- --ignored --nocapture`.
    #[tokio::test(flavor = "multi_thread")]
    #[ignore = "starts the bundled REA on the system's Node.js"]
    async fn rea_starts_and_a_script_inspects_a_binary_in_one_call() {
        let rea = Rea::new(Some(Path::new(env!("CARGO_MANIFEST_DIR")).join("rea")));
        let started = rea.call(START, serde_json::json!({})).await;
        assert!(!started.is_error, "{}", started.text);
        assert!(
            started
                .text
                .starts_with("REA is on for this team, in Code Mode")
        );
        let names: Vec<String> = rea.specs().into_iter().map(|s| s.name).collect();
        assert_eq!(names, [SEARCH, EXECUTE, CALL, GUIDE]);

        let found = rea
            .call(
                SEARCH,
                serde_json::json!({"code":
                    "async () => catalog.filter(t => /hardening|pe imports/i.test(t.name.replace(/_/g, ' '))).map(t => t.name)"}),
            )
            .await;
        println!("search: {}", found.text);
        assert!(
            found.text.contains("inspect_binary_hardening"),
            "{}",
            found.text
        );

        // Two inspections in one script, returning a few fields, not two evidence records.
        let exe = std::env::current_exe().unwrap();
        let code = format!(
            "async () => {{
                const path = {path};
                const [h, i] = await Promise.all([
                    rea('inspect_binary_hardening', {{path}}),
                    rea('inspect_pe_imports', {{path}}),
                ]);
                console.log('evidence', h.evidence_id);
                return {{ kind: h.subject?.identity?.format, bytes: JSON.stringify(h).length + JSON.stringify(i).length }};
            }}",
            path = serde_json::to_string(&exe).unwrap()
        );
        let out = rea.call(EXECUTE, serde_json::json!({ "code": code })).await;
        println!("execute: {}", out.text);
        assert!(!out.is_error, "{}", out.text);
        assert!(out.text.starts_with("2 REA calls."));
        assert!(out.text.contains(r#""kind":"pe""#), "{}", out.text);
        assert!(
            out.text.len() < 400,
            "the script kept only what it returned"
        );

        let plain = rea
            .call(
                CALL,
                serde_json::json!({"tool": "inspect_binary_hardening", "arguments": {"path": exe}}),
            )
            .await;
        assert!(!plain.is_error, "{}", plain.text);
        let missing = rea
            .call(
                EXECUTE,
                serde_json::json!({"code": "async () => rea('no_such_tool', {})"}),
            )
            .await;
        assert!(
            missing.is_error && missing.text.contains("rea_search"),
            "{}",
            missing.text
        );
    }
}
