//! Subagents: helpers a skill hands work to, in Claude Code's agent format.
//!
//! An agent is a markdown file with `name` and `description` frontmatter;
//! its body is the helper's system prompt. The model starts one with the
//! `dispatch_agent` tool. The orchestrator runs it as a turn of its own, in
//! memory, with the same model and tools (minus `dispatch_agent`), and its
//! final reply becomes the tool's result. Understand-Anything's `/understand`
//! is built on this.

use std::collections::BTreeMap;
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

/// The model each team role starts on, ahead of the worker model, shared like it.
pub const ROLE_MODELS_FILE: &str = "role-models.json";

/// The saved model per role; roles left out use the worker model.
#[must_use]
pub fn load_role_models(data_dir: &Path) -> BTreeMap<String, ModelRef> {
    let path = data_dir.join(ROLE_MODELS_FILE);
    let Ok(text) = std::fs::read_to_string(&path) else {
        return BTreeMap::new();
    };
    serde_json::from_str(&text).unwrap_or_else(|e| {
        tracing::warn!(error = %e, path = %path.display(), "role models unreadable; using the worker model");
        BTreeMap::new()
    })
}

/// # Errors
/// The file cannot be written.
pub fn save_role_models(
    data_dir: &Path,
    models: &BTreeMap<String, ModelRef>,
) -> std::io::Result<()> {
    let json = serde_json::to_string_pretty(models).map_err(std::io::Error::other)?;
    std::fs::write(data_dir.join(ROLE_MODELS_FILE), json)
}

/// Each team role's own fallbacks, ahead of the shared list, shared like it.
pub const ROLE_FALLBACKS_FILE: &str = "role-fallbacks.json";

/// The saved fallbacks per role; roles left out use the shared list.
#[must_use]
pub fn load_role_fallbacks(data_dir: &Path) -> BTreeMap<String, Vec<ModelRef>> {
    let path = data_dir.join(ROLE_FALLBACKS_FILE);
    let Ok(text) = std::fs::read_to_string(&path) else {
        return BTreeMap::new();
    };
    serde_json::from_str(&text).unwrap_or_else(|e| {
        tracing::warn!(error = %e, path = %path.display(), "role fallbacks unreadable; using the shared list");
        BTreeMap::new()
    })
}

/// # Errors
/// The file cannot be written.
pub fn save_role_fallbacks(
    data_dir: &Path,
    fallbacks: &BTreeMap<String, Vec<ModelRef>>,
) -> std::io::Result<()> {
    let json = serde_json::to_string_pretty(fallbacks).map_err(std::io::Error::other)?;
    std::fs::write(data_dir.join(ROLE_FALLBACKS_FILE), json)
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

const COORDINATOR_PROMPT: &str = "You are the coordinator. Do not edit files yourself, except the plan file .ws/plan.md. Read enough of the project to plan, split the task into parts that touch different files, and write the plan to .ws/plan.md before dispatching: the task, one checklist line per part (`- [ ] part: its files`), and how you will check the result. Your team has six fixed roles, listed in dispatch_agent: explorer, implementer, tester, debugger, reviewer and docs-writer. You decide which role gets which part and when, for example: the explorer to map code you do not know yet; implementers to build parts that touch different files; the tester to write and run tests; the debugger for a failure; the reviewer to check finished changes; the docs-writer when behaviour or interfaces change. dispatch_agent starts a role in the background and returns at once, so roles work at the same time: up to two copies of a role (implementer and implementer-2), six workers in all. A role remembers its earlier tasks in this run, so send a fix or a review finding to the copy that did the work. Give each the full task, its files, the interfaces it must keep, and who else is working on what. You lead a team chat: workers message one another and you with post_message, and you answer questions, settle disagreements and correct course with post_message too; every running worker reads every message. Call wait_for_team to hear from them: it returns their messages, and their reports as they finish. In a git project, all your workers share one copy of it for this run: each sees the others' changes, but you and the project's files do not until the user merges the team's branch after the run. So when a worker reports, check its result through your team (the tester runs the build and tests, the reviewer reads the changes), tick what is done in the plan (`- [x]`) with a line on what you found, and send anything that is wrong back to the role that did it, or to the debugger. Keep the plan current: it is how a stopped run resumes. Messages from the user (from: you) also arrive through wait_for_team: they come first. Decide routine things yourself; use ask_user only at a milestone worth a check, or for a decision that is the user's (something destructive, outside the task, or a real choice between approaches). Do not finish while anyone is still working: keep calling wait_for_team until every worker has reported. When the task is to find out how existing software works from its shipped files or its behaviour (reverse engineering: an app, a binary, an APK, a website) and start_reverse_engineering is offered, call it first and follow the guide it returns; split the investigation by target or feature, and tell each worker to use rea_search and rea_execute. Finish with a short summary for the user.";

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
                body: "You are a coding worker. Do exactly the task you were given, in the files it names, and stay out of other files. Read before you edit; check your work (build or run the tests) before you report. Other workers are editing this project at the same time: post_message what you are changing if it touches shared code, and act on messages you receive. If you are stuck or a decision is not yours to make, ask the lead with post_message (to: lead) and go on with what you can meanwhile; its answer arrives on its own. Finish with a short report: what you changed, what you checked, and anything left open.".to_owned(),
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

/// A team's fixed roles: name, what the lead sees, the worker's instructions.
const ROLES: [(&str, &str, &str); 6] = [
    (
        "explorer",
        "Reads and maps the code: where things are, how they connect, what a change would touch. Changes no files.",
        "You are the team's explorer. Read the code the task points at and map it: the files, functions and interfaces involved, how they connect, and what a change there would touch or break. Do not change any file. Report the map: paths, names and the facts the others need, short enough to act on.",
    ),
    (
        "implementer",
        "Builds the feature or change in the files it is given.",
        "You are one of the team's implementers. Build exactly the part you were given, in its files, and stay out of other files. Read before you edit, keep the interfaces you were told to keep, and check that it builds before you report.",
    ),
    (
        "tester",
        "Writes and runs tests for a change, and reports what passes and what fails.",
        "You are the team's tester. Write tests for the behaviour you were given, in the project's own test style, then run them and the existing tests. Change only test files. Report what passes and, for each failure, the test, the error and what you think is wrong.",
    ),
    (
        "debugger",
        "Finds the root cause of a failure or bug and fixes it.",
        "You are the team's debugger. Reproduce the failure you were given, find its root cause (not just the symptom), fix it in the fewest lines, and run the failing check again to prove the fix. Report the cause, the fix and how you checked it.",
    ),
    (
        "reviewer",
        "Reviews changes for bugs, security and quality, and reports what to fix. Changes no files.",
        "You are the team's reviewer. Read the changes you were given and the code they touch. Look for bugs, unhandled errors, security holes, missing tests and needless complexity. Do not change any file. Report each finding with its file and line, why it matters and the fix, most serious first; say so plainly if you found nothing.",
    ),
    (
        "docs-writer",
        "Updates documentation, comments and READMEs to match a change.",
        "You are the team's docs writer. Update the documentation the change affects: READMEs, guides, doc comments and examples, in the project's own voice. Change only documentation and comments. Report what you updated.",
    ),
];

/// What every role is told about working in the team.
const ROLE_NOTE: &str = "You work in a team led by a coordinator, beside other workers. Post with post_message what you are changing if it touches shared code, and act on messages you receive. If you are stuck or a decision is not yours to make, ask the lead with post_message (to: lead) and go on with what you can meanwhile. All workers in this run share one copy of the project, so you see one another's changes as you make them. Work only in your own folder, the one your outline names: run commands there, without cd to another path, even one the lead's brief mentions. You keep your memory for this run: when the lead gives you another task, your earlier work is above it. Finish with a short report: what you did, what you checked, and anything left open.";

/// At most this many copies of one role work at once.
pub const MAX_ROLE_COPIES: usize = 2;

/// The team's roles, with what each does, in the lead's order.
pub fn roles() -> impl Iterator<Item = (&'static str, &'static str)> {
    ROLES
        .iter()
        .map(|(name, description, _)| (*name, *description))
}

/// A team role by name, as an agent.
#[must_use]
pub fn role(name: &str) -> Option<AgentDef> {
    ROLES
        .iter()
        .find(|(n, ..)| *n == name)
        .map(|(n, description, body)| AgentDef {
            name: (*n).to_owned(),
            description: (*description).to_owned(),
            body: format!("{body} {ROLE_NOTE}"),
            model: None,
        })
}

/// The role a team dispatch names, and which copy if it names one:
/// `implementer` is either copy, `implementer-2` the second.
#[must_use]
pub fn role_and_copy(name: &str) -> Option<(&'static str, Option<usize>)> {
    ROLES.iter().find_map(|(role, ..)| {
        if name == *role {
            return Some((*role, None));
        }
        let copy: usize = name.strip_prefix(role)?.strip_prefix('-')?.parse().ok()?;
        (1..=MAX_ROLE_COPIES)
            .contains(&copy)
            .then_some((*role, Some(copy)))
    })
}

/// A copy's name: the role for the first, `role-2` for the second.
#[must_use]
pub fn copy_name(role: &str, copy: usize) -> String {
    if copy == 1 {
        role.to_owned()
    } else {
        format!("{role}-{copy}")
    }
}

/// The lead's `dispatch_agent`: the team's roles.
#[must_use]
pub fn team_spec() -> ToolSpec {
    let menu: String = ROLES
        .iter()
        .map(|(name, description, _)| format!("\n- {name}: {description}"))
        .collect();
    ToolSpec {
        name: DISPATCH_AGENT.to_owned(),
        description: format!(
            "Give a task to one of your team's roles. It starts in the background and this returns at once; call wait_for_team for its report. Up to {MAX_ROLE_COPIES} copies of a role work at once: `implementer` takes whichever copy is free, `implementer-2` names the second. A role remembers its earlier tasks in this run, so send follow-ups to the copy that did the work; put everything else it needs in the prompt. If a skill names another agent, dispatch that one as it says. Roles:{menu}"
        ),
        input_schema: serde_json::json!({
            "type": "object",
            "properties": {
                "agent": {"type": "string", "description": "A role, or a role's copy such as implementer-2."},
                "prompt": {"type": "string", "description": "The task, with its files, the interfaces to keep, and who else is working on what."}
            },
            "required": ["agent", "prompt"]
        }),
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

/// The lead's tool for hearing from the workers it started in the background.
pub const WAIT_FOR_TEAM: &str = "wait_for_team";

#[must_use]
pub fn wait_spec() -> ToolSpec {
    ToolSpec {
        name: WAIT_FOR_TEAM.to_owned(),
        description: "Wait for news from the workers you started: their messages, questions and reports. Returns as soon as a worker reports or writes to you (to: lead), otherwise after about a minute with whatever they said. Keep calling it until every worker has reported.".to_owned(),
        input_schema: serde_json::json!({ "type": "object", "properties": {} }),
    }
}

/// The lead's tool for asking the user, for a checkpoint or a decision.
pub const ASK_USER: &str = "ask_user";

#[must_use]
pub fn ask_spec() -> ToolSpec {
    ToolSpec {
        name: ASK_USER.to_owned(),
        description: "Ask the user and wait for their answer. Use it only at a milestone, or for a decision that is theirs: something destructive, outside the task, or a real choice between approaches. Workers keep going while you wait. Otherwise decide yourself.".to_owned(),
        input_schema: serde_json::json!({
            "type": "object",
            "properties": { "question": {"type": "string"} },
            "required": ["question"]
        }),
    }
}

/// `post_message` as the lead sees it.
#[must_use]
pub fn lead_post_spec() -> ToolSpec {
    ToolSpec {
        description: "Message the workers that are running: answer a question, correct a course, or tell everyone something. Every running worker reads it before its next step.".to_owned(),
        ..post_spec()
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

    /// Whether a note past `cursor`, not from `me`, matches `pred`; reads nothing.
    pub fn any_since(
        &self,
        conv: ws_context::ConvId,
        me: &str,
        cursor: usize,
        pred: impl Fn(&Note) -> bool,
    ) -> bool {
        let map = self
            .0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        map.get(&conv)
            .and_then(|all| all.get(cursor..))
            .is_some_and(|new| new.iter().any(|n| n.from != me && pred(n)))
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
    // Tests may unwrap and expect: a panic is the failure report.
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;

    #[test]
    fn a_dispatch_names_a_role_and_maybe_its_copy() {
        assert_eq!(role_and_copy("implementer"), Some(("implementer", None)));
        assert_eq!(
            role_and_copy("docs-writer-2"),
            Some(("docs-writer", Some(2)))
        );
        assert_eq!(role_and_copy("tester-1"), Some(("tester", Some(1))));
        for not in ["tester-3", "tester-", "testers", "worker", ""] {
            assert_eq!(role_and_copy(not), None, "{not}");
        }
        assert_eq!(copy_name("tester", 1), "tester");
        assert_eq!(copy_name("tester", 2), "tester-2");
        assert!(
            role("reviewer")
                .unwrap()
                .body
                .contains("Do not change any file")
        );
    }

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
    fn role_models_are_saved_and_cleared() {
        let dir = tempfile::tempdir().unwrap();
        assert!(load_role_models(dir.path()).is_empty());
        let models = BTreeMap::from([("tester".to_owned(), ModelRef::new("groq", "small"))]);
        save_role_models(dir.path(), &models).unwrap();
        assert_eq!(load_role_models(dir.path()), models);
        save_role_models(dir.path(), &BTreeMap::new()).unwrap();
        assert!(load_role_models(dir.path()).is_empty());
        assert_eq!(roles().count(), 6);
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
    fn role_fallbacks_round_trip_and_a_broken_file_means_none() {
        let dir = tempfile::tempdir().unwrap();
        assert!(load_role_fallbacks(dir.path()).is_empty());
        let saved = BTreeMap::from([(
            "tester".to_owned(),
            vec![ModelRef::new("a", "b"), ModelRef::new("c", "d")],
        )]);
        save_role_fallbacks(dir.path(), &saved).unwrap();
        assert_eq!(load_role_fallbacks(dir.path()), saved);
        std::fs::write(dir.path().join(ROLE_FALLBACKS_FILE), "{").unwrap();
        assert!(load_role_fallbacks(dir.path()).is_empty());
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
