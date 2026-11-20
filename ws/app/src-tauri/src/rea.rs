//! Reverse Engineer Anything (REA), bundled with the app and offered only to
//! agent teams (`Orchestrator::with_team_tools`).
//!
//! REA has 263 tools, about 90k tokens of schemas, and many providers refuse
//! more than 128 tools a request. So a team never sees them directly: until
//! the lead switches REA on it is one small tool, and after that three that
//! find a REA tool by what it does, call one, and read REA's workflow guide.
//! REA itself runs on the system's Node.js, started on first use.

use std::path::{Path, PathBuf};
use ws_context::ToolSpec;
use ws_core::{ToolHost, ToolOutput};
use ws_mcp::{McpConfig, McpTools, ServerConfig};

const START: &str = "start_reverse_engineering";
const FIND: &str = "rea_find_tools";
const CALL: &str = "rea_call";
const GUIDE: &str = "rea_guide";
/// How McpTools prefixes the server's tools.
const PREFIX: &str = "rea__";
const MAX_FOUND: usize = 8;

/// REA's tools for a team, started when the lead first needs them.
#[derive(Debug)]
pub struct Rea {
    /// The bundled package folder (holds `scripts/rea.mjs`); none in a build without it.
    dir: Option<PathBuf>,
    tools: tokio::sync::OnceCell<McpTools>,
}

impl Rea {
    pub fn new(dir: Option<PathBuf>) -> Self {
        Self {
            dir,
            tools: tokio::sync::OnceCell::new(),
        }
    }

    /// Start REA once; every team after that shares it.
    async fn start(&self) -> Result<&McpTools, String> {
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
                    None => Ok(tools),
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

/// The REA tools that best match `query`: every word counts, a word in the
/// name more than one in the description.
fn search<'a>(specs: &'a [ToolSpec], query: &str) -> Vec<&'a ToolSpec> {
    let words: Vec<String> = query
        .to_lowercase()
        .split(|c: char| !c.is_alphanumeric())
        .filter(|w| w.len() > 1)
        .map(str::to_owned)
        .collect();
    let mut scored: Vec<(usize, &ToolSpec)> = specs
        .iter()
        .map(|s| {
            let (name, desc) = (s.name.to_lowercase(), s.description.to_lowercase());
            let score = words
                .iter()
                .map(|w| {
                    3 * usize::from(name.contains(w.as_str()))
                        + usize::from(desc.contains(w.as_str()))
                })
                .sum();
            (score, s)
        })
        .filter(|(score, _)| *score > 0)
        .collect();
    scored.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.name.cmp(&b.1.name)));
    scored.into_iter().take(MAX_FOUND).map(|(_, s)| s).collect()
}

fn spec(name: &str, description: &str, schema: serde_json::Value) -> ToolSpec {
    ToolSpec {
        name: name.to_owned(),
        description: description.to_owned(),
        input_schema: schema,
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
                 Returns REA's workflow guide; then use rea_find_tools and rea_call, and tell \
                 workers to do the same.",
                serde_json::json!({"type": "object", "properties": {}}),
            )];
        }
        vec![
            spec(
                FIND,
                "Find REA's tools by what they do (REA has 263). Returns up to 8 matches with \
                 the exact input schema to pass to rea_call. Search in plain words, e.g. \
                 \"decompile function\", \"electron ipc\", \"apk class\", \"web network\".",
                serde_json::json!({
                    "type": "object",
                    "properties": {"query": {"type": "string", "description": "What the tool should do"}},
                    "required": ["query"]
                }),
            ),
            spec(
                CALL,
                "Call one REA tool found with rea_find_tools. Static inspection only reads the \
                 target; runtime capture runs or interacts with it, so prefer static tools first.",
                serde_json::json!({
                    "type": "object",
                    "properties": {
                        "tool": {"type": "string", "description": "The REA tool's name, as rea_find_tools gave it"},
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

    /// Starting REA, finding tools and reading the guide change nothing; a
    /// REA call may run the target, so it asks.
    fn needs_approval(&self, name: &str) -> bool {
        name == CALL
    }

    async fn call(&self, name: &str, args: serde_json::Value) -> ToolOutput {
        match name {
            START => match self.start().await {
                Ok(_) => match self.guide(None) {
                    Ok(guide) => ToolOutput::ok(format!(
                        "REA is on for this team: use {FIND} to find a tool and {CALL} to run it. \
                         Workers you start from now on have them too; tell them to use REA.\n\n{guide}"
                    )),
                    Err(e) => {
                        ToolOutput::ok(format!("REA is on, but its guide could not be read: {e}"))
                    }
                },
                Err(e) => ToolOutput::error(e),
            },
            FIND => {
                let Some(tools) = self.tools.get() else {
                    return ToolOutput::error(format!("REA is not on yet: call {START}"));
                };
                let query = args["query"].as_str().unwrap_or_default();
                let specs = tools.specs();
                let found = search(&specs, query);
                if found.is_empty() {
                    return ToolOutput::ok(format!(
                        "No REA tool matches \"{query}\". Try other words, or read {GUIDE}."
                    ));
                }
                let list: Vec<serde_json::Value> = found
                    .iter()
                    .map(|s| {
                        serde_json::json!({
                            "tool": s.name.trim_start_matches(PREFIX),
                            "description": s.description,
                            "input_schema": s.input_schema,
                        })
                    })
                    .collect();
                ToolOutput::ok(serde_json::to_string_pretty(&list).unwrap_or_default())
            }
            CALL => {
                let Some(tools) = self.tools.get() else {
                    return ToolOutput::error(format!("REA is not on yet: call {START}"));
                };
                let Some(tool) = args["tool"].as_str() else {
                    return ToolOutput::error("rea_call needs \"tool\"");
                };
                let arguments = match args.get("arguments") {
                    Some(a @ serde_json::Value::Object(_)) => a.clone(),
                    None | Some(serde_json::Value::Null) => serde_json::json!({}),
                    Some(_) => return ToolOutput::error("\"arguments\" must be an object"),
                };
                let full = format!("{PREFIX}{}", tool.trim_start_matches(PREFIX));
                if !tools.specs().iter().any(|s| s.name == full) {
                    return ToolOutput::error(format!(
                        "REA has no tool called {tool}; find one with {FIND}"
                    ));
                }
                tools.call(&full, arguments).await
            }
            GUIDE => match self.guide(args["topic"].as_str()) {
                Ok(text) => ToolOutput::ok(text),
                Err(e) => ToolOutput::error(e),
            },
            _ => ToolOutput::error(format!("REA has no tool called {name}")),
        }
    }
}

#[cfg(test)]
mod tests {
    // Tests may unwrap and expect: a panic is the failure report.
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;

    fn tool(name: &str, description: &str) -> ToolSpec {
        spec(name, description, serde_json::json!({"type": "object"}))
    }

    #[test]
    fn search_ranks_name_matches_first_and_drops_non_matches() {
        let specs = [
            tool(
                "rea__procedure_pseudo_code",
                "Decompile one function to pseudocode",
            ),
            tool("rea__batch_decompile", "Decompile many functions"),
            tool("rea__inspect_android_package", "Read an APK manifest"),
        ];
        let names: Vec<&str> = search(&specs, "decompile function")
            .iter()
            .map(|s| s.name.as_str())
            .collect();
        assert_eq!(
            names,
            ["rea__batch_decompile", "rea__procedure_pseudo_code"]
        );
        assert!(search(&specs, "zzz").is_empty());
        assert!(search(&specs, "").is_empty(), "no words, no matches");
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

    /// The real bundled REA, end to end: `node scripts/build-rea.mjs`, then
    /// `cargo test -p ws-app rea_starts -- --ignored --nocapture`.
    #[tokio::test]
    #[ignore = "starts the bundled REA on the system's Node.js"]
    async fn rea_starts_finds_a_tool_and_inspects_a_binary() {
        let rea = Rea::new(Some(Path::new(env!("CARGO_MANIFEST_DIR")).join("rea")));
        let started = rea.call(START, serde_json::json!({})).await;
        assert!(!started.is_error, "{}", started.text);
        assert!(started.text.contains("REA is on"));
        let names: Vec<String> = rea.specs().into_iter().map(|s| s.name).collect();
        assert_eq!(
            names,
            [FIND, CALL, GUIDE],
            "the switch is replaced by the three"
        );

        let found = rea
            .call(FIND, serde_json::json!({"query": "binary hardening"}))
            .await;
        println!("{}", &found.text[..found.text.len().min(400)]);
        assert!(
            found
                .text
                .contains("\"tool\": \"inspect_binary_hardening\"")
        );

        let exe = std::env::current_exe().unwrap();
        let out = rea
            .call(
                CALL,
                serde_json::json!({"tool": "inspect_binary_hardening", "arguments": {"path": exe}}),
            )
            .await;
        println!("{}", &out.text[..out.text.len().min(600)]);
        assert!(!out.is_error, "{}", out.text);
        let missing = rea
            .call(
                CALL,
                serde_json::json!({"tool": "no_such_tool", "arguments": {}}),
            )
            .await;
        assert!(missing.is_error && missing.text.contains("rea_find_tools"));
    }

    #[test]
    fn before_it_starts_a_team_sees_one_tool_and_without_rea_none() {
        let rea = Rea::new(Some(PathBuf::from("/nowhere")));
        let names: Vec<String> = rea.specs().into_iter().map(|s| s.name).collect();
        assert_eq!(names, [START]);
        assert!(!rea.needs_approval(START));
        assert!(rea.needs_approval(CALL), "a REA call can run the target");
        assert!(Rea::new(None).specs().is_empty());
    }
}
