//! Subagents: helpers a skill hands work to, in Claude Code's agent format.
//!
//! An agent is a markdown file with `name` and `description` frontmatter;
//! its body is the helper's system prompt. The model starts one with the
//! `dispatch_agent` tool. The orchestrator runs it as a turn of its own, in
//! memory, with the same model and tools (minus `dispatch_agent`), and its
//! final reply becomes the tool's result. Understand-Anything's `/understand`
//! is built on this.

use std::path::Path;
use ws_context::{ModelRef, ToolSpec};

pub const DISPATCH_AGENT: &str = "dispatch_agent";

/// The built-in helper every coordinator can dispatch.
pub const WORKER: &str = "worker";

/// The models workers fall back to, in order, kept in the app data folder so
/// the app and the `ws` CLI share them.
pub const WORKER_FALLBACKS_FILE: &str = "worker-fallbacks.json";

/// The model workers start on instead of the chat's, shared like the fallbacks.
pub const WORKER_MODEL_FILE: &str = "worker-model.json";

/// The saved worker model; none if unset or unreadable (workers then use the chat's).
#[must_use]
pub fn load_worker_model(data_dir: &Path) -> Option<ModelRef> {
    let path = data_dir.join(WORKER_MODEL_FILE);
    let text = std::fs::read_to_string(&path).ok()?;
    serde_json::from_str(&text)
        .inspect_err(|e| tracing::warn!(error = %e, path = %path.display(), "worker model unreadable; using the chat's"))
        .ok()
        .flatten()
}

/// # Errors
/// The file cannot be written.
pub fn save_worker_model(data_dir: &Path, model: Option<&ModelRef>) -> std::io::Result<()> {
    let json = serde_json::to_string_pretty(&model).map_err(std::io::Error::other)?;
    std::fs::write(data_dir.join(WORKER_MODEL_FILE), json)
}

/// The saved fallback list; empty if there is none or it cannot be read.
#[must_use]
pub fn load_worker_fallbacks(data_dir: &Path) -> Vec<ModelRef> {
    let path = data_dir.join(WORKER_FALLBACKS_FILE);
    let Ok(text) = std::fs::read_to_string(&path) else {
        return Vec::new();
    };
    serde_json::from_str(&text).unwrap_or_else(|e| {
        tracing::warn!(error = %e, path = %path.display(), "worker fallbacks unreadable; using none");
        Vec::new()
    })
}

/// # Errors
/// The file cannot be written.
pub fn save_worker_fallbacks(data_dir: &Path, models: &[ModelRef]) -> std::io::Result<()> {
    let json = serde_json::to_string_pretty(models).map_err(std::io::Error::other)?;
    std::fs::write(data_dir.join(WORKER_FALLBACKS_FILE), json)
}

/// Where the coordinator keeps its plan and progress, relative to the project
/// root, so a run that stops can be picked up again.
pub const PLAN_FILE: &str = ".ws/plan.md";

const COORDINATOR_PROMPT: &str = "You are the coordinator. Do not edit files yourself, except the plan file .ws/plan.md. Read enough of the project to plan, split the task into parts that touch different files, and write the plan to .ws/plan.md before dispatching: the task, one checklist line per part (`- [ ] part: its files`), and how you will check the result. Then dispatch a worker for each part in ONE reply so they run at the same time. Give each worker the full task, its files, and the interfaces it must keep. Workers dispatched in the same reply can message one another with post_message (you cannot), so tell them who else is working and what to settle between themselves. When they report, check the result (read the changes, run the build or tests), tick what is done in the plan (`- [x]`) with a line on what you found, and dispatch workers again for anything that is wrong. Keep the plan current after every round: it is how a stopped run resumes. Finish with a short summary for the user.";

/// What makes the model the coordinator, with the plan a previous run left.
#[must_use]
pub fn coordinator_instructions(plan: Option<&str>) -> String {
    let mut out = COORDINATOR_PROMPT.to_owned();
    if let Some(plan) = plan {
        out.push_str(&format!(
            "\n\nA previous run left this plan in {PLAN_FILE}:\n\n{plan}\n\nContinue from it: check that ticked parts really are done, then do the rest. If the task below is a different one, replace the plan."
        ));
    }
    out
}

/// The coordinator's first message (tier one; the workers it dispatches are
/// tier two): its instructions, the plan a previous run left, and the task.
/// An empty task means carry on with the plan.
#[must_use]
pub fn coordinator_message(task: &str, plan: Option<&str>) -> String {
    let mut out = coordinator_instructions(plan);
    let task = task.trim();
    let task = if task.is_empty() {
        "Continue the plan."
    } else {
        task
    };
    out.push_str(&format!("\n\nTask:\n{task}"));
    out
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentDef {
    pub name: String,
    pub description: String,
    /// The helper's instructions: its system prompt.
    pub body: String,
    /// `model: backend/model` in its frontmatter: what it starts on, ahead of
    /// the worker model setting.
    pub model: Option<ModelRef>,
}

/// A frontmatter `model`, if it names one here. Claude Code's `sonnet`,
/// `opus` or `inherit` name no backend, so they leave the choice to Settings.
fn agent_model(text: &str) -> Option<ModelRef> {
    let value = crate::skills::frontmatter_field(text, "model")?;
    let (backend, model) = value.split_once('/')?;
    (!backend.is_empty() && !model.is_empty()).then(|| ModelRef::new(backend, model))
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
                let text = std::fs::read_to_string(&p).ok()?;
                let (name, description, body) = crate::skills::parse(&text)?;
                let stem = p.file_stem()?.to_string_lossy().into_owned();
                Some(AgentDef {
                    name: name.unwrap_or(stem),
                    description,
                    body,
                    model: agent_model(&text),
                })
            })
            .collect();
        list.sort_by(|a, b| a.name.cmp(&b.name));
        Self { list }
    }

    /// Add the built-in `worker`, unless an agent of that name is already loaded.
    #[must_use]
    pub fn with_worker(mut self) -> Self {
        if self.get(WORKER).is_none() {
            self.list.push(AgentDef {
                name: WORKER.to_owned(),
                description: "General coding worker: reads, edits and checks code for one self-contained task.".to_owned(),
                body: "You are a coding worker. Do exactly the task you were given, in the files it names, and stay out of other files. Read before you edit; check your work (build or run the tests) before you report. Other workers are editing this project at the same time: post_message what you are changing if it touches shared code, and act on messages you receive. Finish with a short report: what you changed, what you checked, and anything left open.".to_owned(),
                model: None,
            });
            self.list.sort_by(|a, b| a.name.cmp(&b.name));
        }
        self
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

/// What one helper may post in a dispatch, and how long a note may be.
pub const MAX_POSTS_PER_HELPER: usize = 20;
pub const MAX_NOTE_CHARS: usize = 2_000;

/// The tool a helper uses to talk to the other helpers running beside it.
pub const POST_MESSAGE: &str = "post_message";

#[must_use]
pub fn post_spec() -> ToolSpec {
    ToolSpec {
        name: POST_MESSAGE.to_owned(),
        description: "Tell the other helpers running at the same time something they need: which file you are changing, a question, a finished handoff. Their new messages arrive on their own before their next step; you do not need to poll.".to_owned(),
        input_schema: serde_json::json!({
            "type": "object",
            "properties": {
                "text": {"type": "string"},
                "to": {"type": "string", "description": "Who it is mainly for, shown with the message; everyone running now reads it."}
            },
            "required": ["text"]
        }),
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Note {
    /// Sender's id: the agent name and the call that started it.
    pub from: String,
    pub to: Option<String>,
    pub text: String,
}

/// What the helpers of each conversation have said to one another.
// shortcut: lives in memory and is never pruned, upgrade if channels must survive a restart.
#[derive(Debug, Default)]
pub struct Channel(std::sync::Mutex<std::collections::HashMap<ws_context::ConvId, Vec<Note>>>);

impl Channel {
    /// Add `note`, cut to `MAX_NOTE_CHARS`. Refused (false) once its sender
    /// has posted `MAX_POSTS_PER_HELPER`, so two chatty helpers cannot loop.
    pub fn post(&self, conv: ws_context::ConvId, mut note: Note) -> bool {
        let mut map = self
            .0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let notes = map.entry(conv).or_default();
        if notes.iter().filter(|n| n.from == note.from).count() >= MAX_POSTS_PER_HELPER {
            return false;
        }
        note.text = note.text.chars().take(MAX_NOTE_CHARS).collect();
        notes.push(note);
        true
    }

    /// Forget a conversation's notes, once the helpers that wrote them are done.
    pub fn clear(&self, conv: ws_context::ConvId) {
        self.0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .remove(&conv);
    }

    /// Notes past `cursor` from the others. Every helper reads every note: they
    /// cannot know one another's names, and a note sent to a guessed one would
    /// be lost (seen in a live run). Moves `cursor` to the end.
    pub fn unread(&self, conv: ws_context::ConvId, me: &str, cursor: &mut usize) -> Vec<Note> {
        let map = self
            .0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let all = map.get(&conv).map_or(&[][..], Vec::as_slice);
        let new = all
            .get(*cursor..)
            .unwrap_or_default()
            .iter()
            .filter(|n| n.from != me)
            .cloned()
            .collect();
        *cursor = all.len();
        new
    }
}

#[cfg(test)]
mod tests {
    // Spec s14 permits unwrap/expect in tests.
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;

    #[test]
    fn the_built_in_worker_is_offered_unless_replaced() {
        assert!(Agents::default().with_worker().get(WORKER).is_some());
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("worker.md"),
            "---
name: worker
description: mine
---
mine",
        )
        .unwrap();
        let agents = Agents::load(dir.path()).with_worker();
        assert_eq!(agents.get(WORKER).unwrap().body, "mine");
    }

    #[test]
    fn an_agent_names_its_model_as_backend_slash_model() {
        let dir = tempfile::tempdir().unwrap();
        let agent = |file: &str, model: &str| {
            std::fs::write(
                dir.path().join(file),
                format!("---\nname: {file}\ndescription: d\nmodel: {model}\n---\nbody"),
            )
            .unwrap();
        };
        agent("coder.md", "openrouter/qwen/qwen3-coder");
        agent("claude-style.md", "sonnet");
        let agents = Agents::load(dir.path());
        assert_eq!(
            agents.get("coder.md").unwrap().model,
            Some(ModelRef::new("openrouter", "qwen/qwen3-coder"))
        );
        assert_eq!(agents.get("claude-style.md").unwrap().model, None);
        assert_eq!(agents.get("claude-style.md").unwrap().body, "body");
    }

    #[test]
    fn the_worker_model_is_saved_and_cleared() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(load_worker_model(dir.path()), None);
        let m = ModelRef::new("groq", "llama");
        save_worker_model(dir.path(), Some(&m)).unwrap();
        assert_eq!(load_worker_model(dir.path()), Some(m));
        save_worker_model(dir.path(), None).unwrap();
        assert_eq!(load_worker_model(dir.path()), None);
    }

    #[test]
    fn a_helper_reads_what_others_said_to_it_once() {
        let (ch, conv) = (Channel::default(), ws_context::ConvId::new());
        let note = |from: &str, to: Option<&str>| Note {
            from: from.to_owned(),
            to: to.map(str::to_owned),
            text: "t".to_owned(),
        };
        ch.post(conv, note("a#1", None));
        ch.post(conv, note("b#2", Some("a")));
        ch.post(conv, note("b#2", Some("cli developer")));
        ch.post(conv, note("a#1", None));
        let mut cursor = 0;
        let got = ch.unread(conv, "a#1", &mut cursor);
        assert_eq!(
            got.len(),
            2,
            "its own notes are skipped; one to a guessed name still arrives"
        );
        assert!(ch.unread(conv, "a#1", &mut cursor).is_empty(), "only once");
    }

    #[test]
    fn a_helper_cannot_post_forever_and_notes_are_cut_and_cleared() {
        let (ch, conv) = (Channel::default(), ws_context::ConvId::new());
        let note = |text: String| Note {
            from: "a#1".to_owned(),
            to: None,
            text,
        };
        for _ in 0..MAX_POSTS_PER_HELPER {
            assert!(ch.post(conv, note("x".repeat(MAX_NOTE_CHARS + 50))));
        }
        assert!(!ch.post(conv, note("one too many".to_owned())));
        let mut cursor = 0;
        let got = ch.unread(conv, "b#2", &mut cursor);
        assert_eq!(got.len(), MAX_POSTS_PER_HELPER);
        assert_eq!(got[0].text.chars().count(), MAX_NOTE_CHARS);

        ch.clear(conv);
        assert!(ch.unread(conv, "b#2", &mut 0).is_empty());
        assert!(
            ch.post(conv, note("again".to_owned())),
            "a cleared channel starts over"
        );
    }

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

    #[test]
    fn the_coordinator_is_given_a_left_plan_and_an_empty_task_continues_it() {
        let fresh = coordinator_message("add tests", None);
        assert!(
            fresh.contains(PLAN_FILE)
                && fresh.ends_with(
                    "Task:
add tests"
                )
        );
        assert!(!fresh.contains("previous run"));

        let resumed = coordinator_message(
            " ",
            Some(
                "- [x] parser
- [ ] cli",
            ),
        );
        assert!(resumed.contains(
            "- [x] parser
- [ ] cli"
        ));
        assert!(resumed.ends_with(
            "Task:
Continue the plan."
        ));
    }

    #[test]
    fn worker_fallbacks_round_trip_and_a_broken_file_means_none() {
        let dir = tempfile::tempdir().unwrap();
        assert!(load_worker_fallbacks(dir.path()).is_empty(), "none saved");
        let list = vec![ModelRef::new("anthropic", "a"), ModelRef::new("groq", "b")];
        save_worker_fallbacks(dir.path(), &list).unwrap();
        assert_eq!(load_worker_fallbacks(dir.path()), list);
        std::fs::write(dir.path().join(WORKER_FALLBACKS_FILE), "not json").unwrap();
        assert!(load_worker_fallbacks(dir.path()).is_empty());
    }
}
