//! Subagents: helpers a skill hands work to, in Claude Code's agent format.
//!
//! An agent is a markdown file with `name` and `description` frontmatter;
//! its body is the helper's system prompt. The model starts one with the
//! `dispatch_agent` tool. The orchestrator runs it as a turn of its own, in
//! memory, with the same model and tools (minus `dispatch_agent`), and its
//! final reply becomes the tool's result. Understand-Anything's `/understand`
//! is built on this.

use std::path::Path;
use ws_context::ToolSpec;

pub const DISPATCH_AGENT: &str = "dispatch_agent";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentDef {
    pub name: String,
    pub description: String,
    /// The helper's instructions: its system prompt.
    pub body: String,
}

#[derive(Debug, Default)]
pub struct Agents {
    list: Vec<AgentDef>,
}

impl Agents {
    /// Every `*.md` with frontmatter directly in `dir`, sorted by name. A
    /// missing folder is just no agents.
    #[must_use]
    pub fn load(dir: &Path) -> Self {
        let mut list: Vec<AgentDef> = std::fs::read_dir(dir)
            .into_iter()
            .flatten()
            .flatten()
            .map(|e| e.path())
            .filter(|p| p.extension().is_some_and(|x| x == "md"))
            .filter_map(|p| {
                let (name, description, body) =
                    crate::skills::parse(&std::fs::read_to_string(&p).ok()?)?;
                let stem = p.file_stem()?.to_string_lossy().into_owned();
                Some(AgentDef {
                    name: name.unwrap_or(stem),
                    description,
                    body,
                })
            })
            .collect();
        list.sort_by(|a, b| a.name.cmp(&b.name));
        Self { list }
    }

    #[must_use]
    pub fn get(&self, name: &str) -> Option<&AgentDef> {
        self.list.iter().find(|a| a.name == name)
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.list.is_empty()
    }

    /// The `dispatch_agent` tool, listing the agents on offer.
    #[must_use]
    pub fn spec(&self) -> ToolSpec {
        let menu: String = self
            .list
            .iter()
            .map(|a| format!("\n- {}: {}", a.name, a.description.trim()))
            .collect();
        ToolSpec {
            name: DISPATCH_AGENT.to_owned(),
            description: format!(
                "Hand a task to a helper agent and get its final report back. Use it when a skill says to dispatch a subagent. The helper starts with no memory of this conversation, so put everything it needs in the prompt. Several dispatches in one reply run at the same time. Agents:{menu}"
            ),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "agent": {"type": "string", "description": "One of the agents listed."},
                    "prompt": {"type": "string", "description": "The full task, with every path and parameter it needs."}
                },
                "required": ["agent", "prompt"]
            }),
        }
    }
}

#[cfg(test)]
mod tests {
    // Spec s14 permits unwrap/expect in tests.
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;

    #[test]
    fn agents_are_read_from_markdown_with_frontmatter() {
        let dir = tempfile::tempdir().unwrap();
        let scanner = "---\nname: project-scanner\ndescription: |\n  Scans a project.\n---\n\n# Scanner\nScan it.\n";
        std::fs::write(dir.path().join("project-scanner.md"), scanner).unwrap();
        std::fs::write(dir.path().join("notes.md"), "no frontmatter").unwrap();
        std::fs::write(dir.path().join("x.txt"), scanner).unwrap();
        let agents = Agents::load(dir.path());
        let a = agents.get("project-scanner").unwrap();
        assert!(a.body.contains("Scan it."));
        assert!(
            agents
                .spec()
                .description
                .contains("project-scanner: Scans a project.")
        );
        assert!(Agents::load(&dir.path().join("missing")).is_empty());
    }
}
