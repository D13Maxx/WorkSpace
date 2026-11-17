//! Turn orchestration.
//!
//! One turn is: load the path, build the view the target model should see,
//! ask the backend for a reply, stream it to the UI, and persist exactly once
//! at the end.
//!
//! Two rules drive the shape of this file:
//!
//! - **The message row is written once**, when the reply completes, is
//!   cancelled, or fails. While it streams, the partial text
//!   lives in the `drafts` table, checkpointed about once a second, so a crash
//!   loses at most a second of output and never leaves a half-written message.
//! - **Nothing the user saw is discarded.** Cancel and failure both persist the
//!   partial text under a status that says what happened.

use crate::agents::{
    ASK_USER, Agents, Channel, DISPATCH_AGENT, MAX_NOTE_CHARS, MAX_POSTS_PER_HELPER, Note,
    POST_MESSAGE, WAIT_FOR_TEAM, ask_spec, lead_post_spec, post_spec, wait_spec,
};
use crate::error::{CoreError, Result};
use crate::events::TurnEvent;
use crate::profiles::ProfileTable;
use crate::rules::ApprovalRules;
use crate::team::{Report, TEAM_WAIT, Team};
use crate::tools::{ToolHost, ToolOutput};
use chrono::Utc;
use std::collections::{BTreeMap, HashMap, HashSet, VecDeque};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tokio::sync::{mpsc, oneshot};
use tokio_util::sync::CancellationToken;
use ws_context::{
    BackendError, BackendEvent, BackendId, BackendRequest, BuildViewParams, ByteEstimateCounter,
    ChatBackend, ContextConfig, ContextSummary, ContextView, ConvId, Conversation, EventId,
    EventKind, HelperEntry, InlineBlob, Message, ModelProfiles, ModelRef, MsgId, MsgStatus, Part,
    ProviderOpaque, RenderedMessage, RenderedView, Role, StopReason, SummaryId, SummaryRequest,
    TeamPost, TeamWhat, ViewMode, ViewPlan, build_view, summary_prompt,
};
use ws_store::{BlobStore, Draft, FileEdit, NewMessage, Store};

/// Result of asking one backend for its models.
pub type ModelListResult =
    std::result::Result<Vec<ws_context::ModelInfo>, ws_context::BackendError>;

/// How often an in-progress reply is checkpointed.
const DRAFT_CHECKPOINT_INTERVAL: Duration = Duration::from_secs(1);

/// The waits before each retry of a dropped connection, for apps to pass to
/// [`Orchestrator::with_reconnect_waits`]: about two minutes in all, enough
/// for the network to come back after a laptop wakes.
pub const RECONNECT_WAITS: [Duration; 4] = [
    Duration::from_secs(5),
    Duration::from_secs(15),
    Duration::from_secs(30),
    Duration::from_secs(60),
];

/// The context builder asks for at most one summary per turn. A second request
/// means the loop is not converging, which is a bug rather than a retry.
const MAX_SUMMARY_ROUNDS: usize = 1;

/// Model-then-tools rounds per user message. A model that keeps calling tools
/// past this is looping, and every round costs money. Sized for a skill like
/// Understand-Anything's /understand, which runs dozens of steps in one turn.
const MAX_TOOL_ROUNDS: usize = 80;

/// Cap on one tool result, in characters. A whole-file read can otherwise be
/// larger than the model's window, and a message is never split.
// shortcut: truncates instead of paging, upgrade if agents need whole large files.
const MAX_TOOL_OUTPUT_CHARS: usize = 60_000;

/// Model-then-tools rounds for one helper agent. Helpers read and write many
/// files (a batch of a codebase), so they get more than a user turn.
const MAX_AGENT_ROUNDS: usize = 50;

/// Helpers dispatched in one reply run side by side, at most this many at a
/// time (Understand-Anything asks for five).
const MAX_PARALLEL_AGENTS: usize = 5;

/// The lead's name in the team's messages.
const LEAD: &str = "lead";

/// The user's name in the team's messages.
const USER: &str = "you";

/// Appended to every helper's instructions.
const AGENT_NOTE: &str = "You are a helper agent dispatched by Workspace. Do the task with your tools, then reply with your final report as plain text; that reply is all the dispatcher sees. Change files only with write_file and edit_file, never through shell commands such as sed, so edits stay safe and undoable. Tools in your instructions may have other names here: Read is read_file, Write is write_file, Edit is edit_file, Grep is search_files, Glob and LS are list_dir, Bash is run_command.";

/// A file edit, ready to show: the diff, and whether it has been undone.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EditView {
    pub call_id: String,
    pub path: String,
    pub diff: String,
    pub undone: bool,
}

/// A tool call the model asked for, assembled from its deltas.
#[derive(Debug, Clone, PartialEq, Eq)]
struct PendingCall {
    call_id: String,
    name: String,
    args: String,
}

const DROPPED: &str = "[output dropped to fit the context window]";

/// Tell the UI what a helper just did, and keep it for the saved log.
async fn step(
    events: &mpsc::Sender<TurnEvent>,
    dispatch: &PendingCall,
    agent: &str,
    log: &mut Vec<HelperEntry>,
    entry: HelperEntry,
) {
    let _ = events
        .send(TurnEvent::HelperStep {
            dispatch: dispatch.call_id.clone(),
            agent: agent.to_owned(),
            entry: entry.clone(),
        })
        .await;
    log.push(entry);
}

fn model_name(m: &ModelRef) -> String {
    format!("{}/{}", m.backend, m.model_id)
}

/// A tool's output as the helper log keeps it: the full text went to the
/// helper, but a whole file read would bloat every saved conversation.
fn clip(text: &str) -> String {
    const KEEP: usize = 4_000;
    match text.char_indices().nth(KEEP) {
        Some((at, _)) => format!(
            "{}\n… ({} more characters)",
            &text[..at],
            text[at..].chars().count()
        ),
        None => text.to_owned(),
    }
}

/// Cut a helper's history down to `budget` tokens (estimated) by replacing the
/// oldest tool results with a note. The newest message is never touched, since
/// the model is about to read it.
fn trim_history(messages: &mut [RenderedMessage], budget: u32) {
    use ws_context::TokenCounter as _;
    let count = |t: &str| ByteEstimateCounter.count_text(t);
    let size = |p: &Part| match p {
        Part::Text { text } => count(text),
        Part::ToolCall { args_json, .. } => count(args_json),
        Part::ToolResult { content, .. } => {
            content.iter().filter_map(Part::as_text).map(count).sum()
        }
        _ => 0,
    };
    let mut total: u32 = messages.iter().flat_map(|m| &m.parts).map(size).sum();
    let last = messages.len().saturating_sub(1);
    for part in messages.iter_mut().take(last).flat_map(|m| &mut m.parts) {
        if total <= budget {
            return;
        }
        if let Part::ToolResult { content, .. } = part {
            let was = size(&Part::ToolResult {
                call_id: String::new(),
                content: std::mem::take(content),
                is_error: false,
            });
            *content = vec![Part::text(DROPPED)];
            total = (total - was.min(total)) + count(DROPPED);
        }
    }
}

/// Send `request` and gather the reply's text; the error is the backend's message.
async fn collect_text(
    backend: &dyn ChatBackend,
    request: BackendRequest,
    cancel: &CancellationToken,
) -> std::result::Result<String, String> {
    use futures_util::StreamExt as _;
    let mut stream = backend
        .stream(request, cancel.clone())
        .await
        .map_err(|e| e.to_string())?;
    let mut text = String::new();
    while let Some(event) = stream.next().await {
        match event {
            BackendEvent::TextDelta(t) => text.push_str(&t),
            BackendEvent::Error(e) => return Err(e.to_string()),
            BackendEvent::Done { .. } => break,
            _ => {}
        }
    }
    Ok(text)
}

pub struct Orchestrator {
    /// `rusqlite::Connection` is `Send` but not `Sync`, so access is
    /// serialised. Every lock is held for one synchronous statement and never
    /// across an await.
    store: Arc<Mutex<Store>>,
    blobs: Arc<BlobStore>,
    backends: HashMap<BackendId, Arc<dyn ChatBackend>>,
    profiles: Arc<ProfileTable>,
    cfg: ContextConfig,
    draft_interval: Duration,
    tools: Option<Arc<dyn ToolHost>>,
    /// Helpers the model may dispatch, offered as `dispatch_agent`.
    agents: Option<Arc<Agents>>,
    /// Calls waiting on the user, by call id. Resolved by `resolve_approval`.
    approvals: Mutex<HashMap<String, oneshot::Sender<bool>>>,
    /// One lock per edited file, held from the before-snapshot to the
    /// after-snapshot, so agents running side by side cannot interleave edits
    /// to one file or get each other's changes in their diffs.
    // shortcut: entries are never pruned, upgrade if a session touches millions of paths.
    file_locks: Mutex<HashMap<std::path::PathBuf, Arc<tokio::sync::Mutex<()>>>>,
    /// What helpers running side by side say to one another.
    channel: Channel,
    /// Models a helper switches to, in order, when its own fails. Never used
    /// for the main turn.
    worker_fallbacks: std::sync::RwLock<Vec<ModelRef>>,
    /// What helpers start on, unless their agent names a model; none: the chat's.
    worker_model: std::sync::RwLock<Option<ModelRef>>,
    /// How long to wait before each retry of a step whose connection dropped,
    /// as when the laptop sleeps mid-reply. Empty: never retry.
    reconnect_waits: Vec<Duration>,
    /// Conversations whose turn runs as a team, so the lead is offered
    /// wait_for_team and post_message.
    team_convs: Mutex<HashSet<ConvId>>,
    /// Routine calls that run without asking, as the user set them.
    rules: std::sync::RwLock<ApprovalRules>,
    /// The lead's questions waiting on the user, by call id.
    questions: Mutex<HashMap<String, oneshot::Sender<String>>>,
}

impl std::fmt::Debug for Orchestrator {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Orchestrator")
            .field("backends", &self.backends.keys().collect::<Vec<_>>())
            .field("cfg", &self.cfg)
            .finish_non_exhaustive()
    }
}

impl Orchestrator {
    #[must_use]
    pub fn new(
        store: Arc<Mutex<Store>>,
        blobs: Arc<BlobStore>,
        profiles: Arc<ProfileTable>,
    ) -> Self {
        Self {
            store,
            blobs,
            backends: HashMap::new(),
            profiles,
            cfg: ContextConfig::default(),
            draft_interval: DRAFT_CHECKPOINT_INTERVAL,
            tools: None,
            agents: None,
            approvals: Mutex::new(HashMap::new()),
            file_locks: Mutex::new(HashMap::new()),
            channel: Channel::default(),
            worker_fallbacks: std::sync::RwLock::default(),
            worker_model: std::sync::RwLock::default(),
            reconnect_waits: Vec::new(),
            team_convs: Mutex::default(),
            rules: std::sync::RwLock::default(),
            questions: Mutex::default(),
        }
    }

    fn file_lock(&self, path: &std::path::Path) -> Arc<tokio::sync::Mutex<()>> {
        let mut locks = self
            .file_locks
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        // Canonical when the file exists, so `A.rs` and `a.rs` share a lock on Windows.
        let key = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
        locks.entry(key).or_default().clone()
    }

    #[must_use]
    pub fn with_tools(mut self, host: Arc<dyn ToolHost>) -> Self {
        self.tools = Some(host);
        self
    }

    /// Offer `agents` to the model through `dispatch_agent`.
    #[must_use]
    pub fn with_agents(mut self, agents: Arc<Agents>) -> Self {
        self.agents = Some(agents);
        self
    }

    /// Answer a pending approval. Returns false if no call is waiting under
    /// that id (already answered, or the turn was cancelled).
    pub fn resolve_approval(&self, call_id: &str, approved: bool) -> bool {
        let tx = self
            .approvals
            .lock()
            .ok()
            .and_then(|mut m| m.remove(call_id));
        tx.is_some_and(|tx| tx.send(approved).is_ok())
    }

    /// Answer the lead's question `call_id`. Returns false if nothing is asking.
    pub fn answer_question(&self, call_id: &str, answer: String) -> bool {
        let tx = self
            .questions
            .lock()
            .ok()
            .and_then(|mut m| m.remove(call_id));
        tx.is_some_and(|tx| tx.send(answer).is_ok())
    }

    /// Set which routine calls run without asking.
    pub fn set_rules(&self, rules: ApprovalRules) {
        if let Ok(mut r) = self.rules.write() {
            *r = rules;
        }
    }

    /// Tell the team working in `conv` something; the lead and every worker
    /// read it at their next step. Returns the saved team line's id and time.
    ///
    /// # Errors
    /// No team is working in `conv`, or the line could not be saved.
    pub fn user_note(&self, conv: ConvId, text: &str) -> Result<(String, chrono::DateTime<Utc>)> {
        let working = self.team_convs.lock().is_ok_and(|t| t.contains(&conv));
        if !working {
            return Err(CoreError::ToolLoop(
                "no team is working in this chat".to_owned(),
            ));
        }
        let text: String = text.trim().chars().take(MAX_NOTE_CHARS).collect();
        self.channel.post(
            conv,
            Note {
                from: USER.to_owned(),
                to: None,
                text: text.clone(),
            },
        );
        let kind = EventKind::Team(TeamPost {
            what: TeamWhat::Note,
            from: USER.to_owned(),
            dispatch: String::new(),
            to: None,
            text,
        });
        let id = self.with_store(|s| Ok(s.record_event(conv, None, &kind)?))?;
        Ok((id.to_string(), Utc::now()))
    }

    /// Tools are offered only when there is a host and the target's backend
    /// can carry them; otherwise tool history is flattened to text.
    /// Set the models a helper falls back to, in order, when its model fails
    /// (after the provider's own retries). The main turn never falls back.
    pub fn set_worker_fallbacks(&self, models: Vec<ModelRef>) {
        if let Ok(mut f) = self.worker_fallbacks.write() {
            *f = models;
        }
    }

    /// Set the model helpers start on, so a large model can lead and smaller
    /// ones do the parts. None: they use the chat's model.
    pub fn set_worker_model(&self, model: Option<ModelRef>) {
        if let Ok(mut m) = self.worker_model.write() {
            *m = model;
        }
    }

    /// The first model in `models` a helper can work with: one whose backend
    /// is here and takes tools. Those it skips are dropped.
    fn next_worker_model(
        &self,
        models: &mut VecDeque<ModelRef>,
    ) -> Option<(ModelRef, Arc<dyn ChatBackend>)> {
        while let Some(m) = models.pop_front() {
            if self.tools_enabled(&m)
                && let Ok(b) = self.backend(&m.backend)
            {
                return Some((m, b));
            }
        }
        None
    }

    pub fn tools_enabled(&self, target: &ModelRef) -> bool {
        self.tools.is_some()
            && self
                .backends
                .get(&target.backend)
                .is_some_and(|b| b.supports_tools())
    }

    /// Override how often an in-progress reply is checkpointed. Exists so
    /// tests can prove the checkpoint happens at all without waiting a second
    /// of wall time per assertion.
    #[must_use]
    pub fn with_draft_interval(mut self, interval: Duration) -> Self {
        self.draft_interval = interval;
        self
    }

    /// Retry a step whose connection dropped, waiting each of `waits` in turn.
    /// A sleeping laptop freezes the app mid-reply; on waking the network
    /// takes a few seconds to come back.
    #[must_use]
    pub fn with_reconnect_waits(mut self, waits: Vec<Duration>) -> Self {
        self.reconnect_waits = waits;
        self
    }

    #[must_use]
    pub fn with_backend(mut self, backend: Arc<dyn ChatBackend>) -> Self {
        self.backends.insert(backend.id(), backend);
        self
    }

    #[must_use]
    pub fn with_config(mut self, cfg: ContextConfig) -> Self {
        self.cfg = cfg;
        self
    }

    #[must_use]
    pub fn profiles(&self) -> &Arc<ProfileTable> {
        &self.profiles
    }

    /// Every configured backend, in registration order.
    #[must_use]
    pub fn backend_ids(&self) -> Vec<BackendId> {
        let mut ids: Vec<BackendId> = self.backends.keys().cloned().collect();
        ids.sort();
        ids
    }

    /// Ask every backend for its models.
    ///
    /// Failures are returned per backend rather than aborting: one provider
    /// being unreachable or unconfigured must not empty the whole picker.
    pub async fn list_all_models(&self) -> Vec<(BackendId, ModelListResult)> {
        let mut out = Vec::new();
        for id in self.backend_ids() {
            let Some(backend) = self.backends.get(&id) else {
                continue;
            };
            out.push((id, backend.list_models().await));
        }
        out
    }

    fn backend(&self, id: &BackendId) -> Result<Arc<dyn ChatBackend>> {
        self.backends
            .get(id)
            .cloned()
            .ok_or_else(|| CoreError::UnknownBackend(id.clone()))
    }

    /// Run one synchronous store operation. Never call with a future inside.
    fn with_store<T>(&self, f: impl FnOnce(&Store) -> Result<T>) -> Result<T> {
        // A poisoned lock means another thread panicked mid-statement. The
        // connection itself is still usable, so recover rather than cascade.
        let guard = self.store.lock().unwrap_or_else(|e| e.into_inner());
        f(&guard)
    }

    // ---- conversation lifecycle ----------------------------------------

    /// # Errors
    /// Fails on any database error.
    pub fn create_conversation(&self, title: &str, model: ModelRef) -> Result<ConvId> {
        let conv = Conversation {
            id: ConvId::new(),
            title: title.to_owned(),
            system_prompt: None,
            active_leaf_id: None,
            current_model: model,
            created_at: Utc::now(),
        };
        let id = conv.id;
        self.with_store(|s| Ok(s.create_conversation(&conv)?))?;
        Ok(id)
    }

    /// Append a user message and make it the visible leaf.
    ///
    /// # Errors
    /// Fails if the conversation does not exist, or on any database error.
    pub fn append_user_message(&self, conv: ConvId, parts: Vec<Part>) -> Result<MsgId> {
        let parent = self.with_store(|s| {
            let c = s
                .get_conversation(conv)?
                .ok_or_else(|| CoreError::ConversationNotFound(conv.to_string()))?;
            Ok(c.active_leaf_id)
        })?;
        self.insert(
            conv,
            parent,
            Role::User,
            None,
            parts,
            MsgStatus::Complete,
            None,
        )
    }

    /// Record a model switch and point the conversation at the new model.
    ///
    /// The switch is an event, not a message: it is rendered inline by the UI
    /// and never sent to a model.
    ///
    /// # Errors
    /// Fails if the conversation does not exist, or on any database error.
    pub fn switch_model(&self, conv: ConvId, to: ModelRef) -> Result<()> {
        self.with_store(|s| {
            let c = s
                .get_conversation(conv)?
                .ok_or_else(|| CoreError::ConversationNotFound(conv.to_string()))?;
            if c.current_model == to {
                return Ok(());
            }
            s.record_event(
                conv,
                c.active_leaf_id,
                &EventKind::ModelSwitch {
                    from: c.current_model.clone(),
                    to: to.clone(),
                },
            )?;
            s.set_current_model(conv, &to)?;
            Ok(())
        })
    }

    /// Prepare a regeneration: move the visible leaf back to the parent of
    /// `message`, so the next turn creates a sibling rather than an overwrite.
    ///
    /// # Errors
    /// Fails if the message does not exist, or on any database error.
    pub fn prepare_regenerate(&self, conv: ConvId, message: MsgId) -> Result<()> {
        self.with_store(|s| {
            let m = s
                .get_message(message)?
                .ok_or_else(|| CoreError::ConversationNotFound(message.to_string()))?;
            s.set_active_leaf(conv, m.parent_id)?;
            Ok(())
        })
    }

    #[allow(clippy::too_many_arguments)]
    fn insert(
        &self,
        conv: ConvId,
        parent: Option<MsgId>,
        role: Role,
        author: Option<ModelRef>,
        parts: Vec<Part>,
        status: MsgStatus,
        raw: Option<serde_json::Value>,
    ) -> Result<MsgId> {
        let id = MsgId::new();
        let message = Message {
            id,
            conversation_id: conv,
            parent_id: parent,
            role,
            author_model: author,
            parts,
            created_at: Utc::now(),
            status,
        };
        self.with_store(|s| {
            s.insert_message(&NewMessage {
                message,
                raw_provider_payload: raw,
            })?;
            s.set_active_leaf(conv, Some(id))?;
            Ok(())
        })?;
        Ok(id)
    }

    // ---- building the request ------------------------------------------

    /// Resolve the view into a request: images inlined, replayable payloads
    /// attached.
    fn assemble_request(
        &self,
        target: &ModelRef,
        view: &ContextView,
        rendered: RenderedView,
        max_output: u32,
        team: bool,
    ) -> Result<BackendRequest> {
        let mut blobs = BTreeMap::new();
        for part in rendered.messages.iter().flat_map(|m| &m.parts) {
            if let Part::Image { blob, mime, .. } = part {
                match self.blobs.get(blob.as_str()) {
                    Ok(bytes) => {
                        blobs.insert(
                            blob.clone(),
                            InlineBlob {
                                mime: mime.clone(),
                                bytes,
                            },
                        );
                    }
                    // The adapter turns an unresolved image into a visible
                    // note, so a corrupt blob degrades the question rather
                    // than failing the whole turn.
                    Err(e) => tracing::warn!(%blob, error = %e, "could not read image blob"),
                }
            }
        }

        // Raw payloads are replayed only for a Full view of
        // the same model. The context builder already decided that; this only
        // fetches what it approved.
        let mut raw_payloads = BTreeMap::new();
        if view.mode == ViewMode::Full {
            self.with_store(|s| {
                for msg_id in &view.verbatim_messages {
                    let Some(msg) = s.get_message(*msg_id)? else {
                        continue;
                    };
                    if msg.role != Role::Assistant || msg.author_model.as_ref() != Some(target) {
                        continue;
                    }
                    if let Some(raw) = s.get_raw_payload(*msg_id)? {
                        raw_payloads.insert(*msg_id, raw);
                    }
                }
                Ok(())
            })?;
        }

        Ok(BackendRequest {
            model: target.clone(),
            view: rendered,
            max_output_tokens: max_output,
            thinking: self.profiles.thinking(target),
            raw_payloads,
            blobs,
            tools: match &self.tools {
                Some(host) if self.tools_enabled(target) => {
                    let mut specs = host.specs();
                    if let Some(agents) = self.agents.as_ref().filter(|a| !a.is_empty()) {
                        specs.push(agents.spec());
                        if team {
                            specs.push(wait_spec());
                            specs.push(lead_post_spec());
                            specs.push(ask_spec());
                        }
                    }
                    specs
                }
                _ => Vec::new(),
            },
        })
    }

    /// Build the view, producing a summary first if history does not fit.
    async fn prepare_view(
        &self,
        conv: ConvId,
        target: &ModelRef,
        events: &mpsc::Sender<TurnEvent>,
        cancel: &CancellationToken,
        context: Option<&str>,
    ) -> Result<(ContextView, RenderedView)> {
        let mut caps = self.profiles.capabilities(target);
        caps.tools = self.tools_enabled(target);
        // Ask the backend how it counts. The local engine counts exactly;
        // API backends fall back to the estimate.
        let counter = self.backend(&target.backend)?.token_counter(target);
        let outgoing = self.with_store(|s| Ok(s.outgoing_model(conv)?))?;

        for round in 0..=MAX_SUMMARY_ROUNDS {
            let (path, summaries, system) = self.with_store(|s| {
                let c = s
                    .get_conversation(conv)?
                    .ok_or_else(|| CoreError::ConversationNotFound(conv.to_string()))?;
                Ok((
                    s.load_active_path(conv)?,
                    s.list_summaries(conv)?,
                    c.system_prompt,
                ))
            })?;
            if path.is_empty() {
                return Err(CoreError::EmptyConversation);
            }
            // Through the view builder, so the budget counts it.
            let system = match (system, context) {
                (Some(s), Some(c)) => Some(format!("{s}\n\n{c}")),
                (s, c) => s.or_else(|| c.map(str::to_owned)),
            };

            let plan = build_view(BuildViewParams {
                path: &path,
                system_prompt: system.as_deref(),
                target,
                caps: &caps,
                summaries: &summaries,
                counter: counter.as_ref(),
                cfg: &self.cfg,
                outgoing: outgoing.as_ref(),
            })?;

            match plan {
                ViewPlan::Ready(built) => return Ok((built.view, built.rendered)),
                ViewPlan::NeedsSummary(req) if round < MAX_SUMMARY_ROUNDS => {
                    let _ = events
                        .send(TurnEvent::Summarising {
                            writer: req.suggested_writer.clone(),
                            covering: u32::try_from(req.covering.len()).unwrap_or(u32::MAX),
                        })
                        .await;
                    let summary = self.write_summary(conv, &req, &path, cancel).await?;
                    self.with_store(|s| Ok(s.save_summary(&summary)?))?;
                }
                ViewPlan::NeedsSummary(_) => return Err(CoreError::SummaryLoopDidNotConverge),
            }
        }
        Err(CoreError::SummaryLoopDidNotConverge)
    }

    /// One question, one answer, outside any conversation: nothing is stored.
    /// For app features that need a model's help with a single task, such as
    /// turning a description into a blueprint part.
    ///
    /// # Errors
    /// Fails if the backend is unknown, the request fails, or the reply is empty.
    pub async fn complete(
        &self,
        model: &ModelRef,
        system: String,
        prompt: String,
        max_output_tokens: u32,
        cancel: &CancellationToken,
    ) -> std::result::Result<String, String> {
        let backend = self.backend(&model.backend).map_err(|e| e.to_string())?;
        let request = BackendRequest {
            model: model.clone(),
            view: RenderedView {
                system: Some(system),
                messages: vec![RenderedMessage {
                    source_id: MsgId::new(),
                    role: Role::User,
                    parts: vec![Part::text(prompt)],
                }],
            },
            max_output_tokens,
            thinking: ws_context::ThinkingMode::Off,
            raw_payloads: BTreeMap::new(),
            blobs: BTreeMap::new(),
            tools: Vec::new(),
        };
        let text = collect_text(backend.as_ref(), request, cancel).await?;
        if text.trim().is_empty() {
            return Err(format!("{model} returned nothing"));
        }
        Ok(text)
    }

    /// Ask a model to summarise the evicted prefix.
    async fn write_summary(
        &self,
        conv: ConvId,
        req: &SummaryRequest,
        path: &[Message],
        cancel: &CancellationToken,
    ) -> Result<ContextSummary> {
        let writer = req.suggested_writer.clone();
        let backend = self.backend(&writer.backend)?;

        // The excerpt goes in as one user message: the summariser is being
        // asked about a transcript, not continuing it.
        let transcript = path[req.covering.clone()]
            .iter()
            .map(|m| format!("{:?}: {}", m.role, m.text_content()))
            .collect::<Vec<_>>()
            .join("\n\n");

        let request = BackendRequest {
            model: writer.clone(),
            view: RenderedView {
                system: Some(summary_prompt(req.budget_tokens)),
                messages: vec![RenderedMessage {
                    source_id: path[req.covering.start].id,
                    role: Role::User,
                    parts: vec![Part::text(transcript)],
                }],
            },
            // Give the summariser headroom over its budget; the context
            // builder rejects an overrun rather than trusting this.
            max_output_tokens: req.budget_tokens.saturating_mul(2).max(512),
            thinking: ws_context::ThinkingMode::Off,
            raw_payloads: BTreeMap::new(),
            blobs: BTreeMap::new(),
            tools: Vec::new(),
        };

        let text = collect_text(backend.as_ref(), request, cancel)
            .await
            .map_err(|message| CoreError::SummaryFailed {
                model: writer.clone(),
                message,
            })?;

        if text.trim().is_empty() {
            return Err(CoreError::SummaryFailed {
                model: writer,
                message: "the summariser returned nothing".to_owned(),
            });
        }

        let leaf = path.last().map_or(req.covers_to, |m| m.id);
        Ok(ContextSummary {
            id: SummaryId::new(),
            conversation_id: conv,
            branch_leaf_at_creation: leaf,
            covers_from: req.covers_from,
            covers_to: req.covers_to,
            token_estimate: {
                use ws_context::TokenCounter as _;
                ByteEstimateCounter.count_text(&text)
            },
            text,
            written_by: writer,
            prompt_version: ws_context::SUMMARY_PROMPT_VERSION,
            created_at: Utc::now(),
        })
    }

    // ---- running the turn ----------------------------------------------

    /// Generate one reply, streaming progress to `events` and persisting the
    /// result exactly once.
    ///
    /// # Errors
    /// Fails only if the turn could not be started at all. Anything that goes
    /// wrong once generation has begun is persisted as a `Failed` message and
    /// reported through `events`.
    pub async fn run_turn(
        &self,
        conv: ConvId,
        events: mpsc::Sender<TurnEvent>,
        cancel: CancellationToken,
    ) -> Result<MsgId> {
        // A message sent with Coordinate on, or a reply to it regenerated.
        if self.coordinated(conv) {
            return Box::pin(self.run_team_turn(conv, events, cancel)).await;
        }
        self.turn_loop(conv, &events, &cancel, None).await
    }

    /// Whether the latest user message carries the coordinator's instructions.
    fn coordinated(&self, conv: ConvId) -> bool {
        self.with_store(|s| Ok(s.load_active_path(conv)?))
            .ok()
            .and_then(|path| {
                let user = path.into_iter().rev().find(|m| m.role == Role::User)?;
                let first = user.parts.first()?.as_text()?.to_owned();
                Some(
                    crate::skills::unwrap_envelope(&first)
                        .is_some_and(|(name, _)| name == "coordinate"),
                )
            })
            .unwrap_or(false)
    }

    /// A coordinated turn: the lead's workers run in the background while it
    /// goes on, it hears from them with `wait_for_team` and writes to them
    /// with `post_message`. Returns once the lead is done and every worker has
    /// reported, so a lead that finishes early leaves no work cut off.
    ///
    /// # Errors
    /// As [`Orchestrator::run_turn`].
    pub async fn run_team_turn(
        &self,
        conv: ConvId,
        events: mpsc::Sender<TurnEvent>,
        cancel: CancellationToken,
    ) -> Result<MsgId> {
        let team = Team::new(&cancel);
        if let Ok(mut t) = self.team_convs.lock() {
            t.insert(conv);
        }
        let lead = async {
            let result = self.turn_loop(conv, &events, &cancel, Some(&team)).await;
            team.close();
            result
        };
        let (result, ()) = tokio::join!(lead, team.drive());
        if let Ok(mut t) = self.team_convs.lock() {
            t.remove(&conv);
        }
        self.channel.clear(conv);
        result
    }

    async fn turn_loop<'s>(
        &'s self,
        conv: ConvId,
        events: &mpsc::Sender<TurnEvent>,
        cancel: &CancellationToken,
        team: Option<&Team<'s>>,
    ) -> Result<MsgId> {
        let (events, cancel) = (events.clone(), cancel.clone());
        // Once per turn, so it stays the same across rounds and the provider's
        // prompt cache keeps working; find_symbol covers what changes meanwhile.
        let context = self.tools.as_ref().and_then(|t| t.context());
        for _ in 0..MAX_TOOL_ROUNDS {
            let (reply, calls) = self
                .run_step(conv, &events, &cancel, context.as_deref())
                .await?;
            if calls.is_empty() {
                return Ok(reply);
            }
            // Every call gets a result, even after a cancel: a tool call with
            // no result is a request the provider rejects on the next turn.
            let results = self.run_tools(conv, calls, &events, &cancel, team).await;
            self.insert(
                conv,
                Some(reply),
                Role::Tool,
                None,
                results,
                MsgStatus::Complete,
                None,
            )?;
            if cancel.is_cancelled() {
                return Ok(reply);
            }
        }
        let error = format!("stopped after {MAX_TOOL_ROUNDS} rounds of tool calls");
        let _ = events
            .send(TurnEvent::Failed {
                message: None,
                error: error.clone(),
            })
            .await;
        Err(CoreError::ToolLoop(error))
    }

    /// Run each requested tool, asking the user first where the host says to.
    async fn run_tools<'s>(
        &'s self,
        conv: ConvId,
        calls: Vec<PendingCall>,
        events: &mpsc::Sender<TurnEvent>,
        cancel: &CancellationToken,
        team: Option<&Team<'s>>,
    ) -> Vec<Part> {
        use futures_util::StreamExt as _;
        // A batch of helpers runs side by side; anything else runs in order,
        // since two edits to one file must not race.
        let helpers = calls.len() > 1 && calls.iter().all(|c| c.name == DISPATCH_AGENT);
        let outs: Vec<ToolOutput> = if let Some(team) = team {
            let mut outs = Vec::with_capacity(calls.len());
            for call in &calls {
                outs.push(match call.name.as_str() {
                    DISPATCH_AGENT => self.start_worker(conv, call, events, team),
                    WAIT_FOR_TEAM => self.wait_for_team(conv, team).await,
                    POST_MESSAGE => self.lead_post(conv, call, events).await,
                    ASK_USER => self.ask_user(conv, call, events, cancel).await,
                    _ => self.run_call(conv, call, events, cancel, None, None).await,
                });
            }
            outs
        } else if helpers {
            // Built up front: a closure inside the stream trips the borrow
            // checker's higher-ranked lifetimes once this future must be Send.
            let runs: Vec<_> = calls
                .iter()
                .map(|c| self.run_call(conv, c, events, cancel, None, None))
                .collect();
            futures_util::stream::iter(runs)
                .buffered(MAX_PARALLEL_AGENTS)
                .collect()
                .await
        } else {
            let mut outs = Vec::with_capacity(calls.len());
            for call in &calls {
                outs.push(self.run_call(conv, call, events, cancel, None, None).await);
            }
            outs
        };
        // Whatever the helpers said to one another is of no use to the next
        // batch. A team's workers outlive a batch: run_team_turn clears it.
        if team.is_none() {
            self.channel.clear(conv);
        }
        let mut parts = Vec::with_capacity(calls.len());
        for (call, out) in calls.into_iter().zip(outs) {
            let _ = events
                .send(TurnEvent::ToolResult {
                    call_id: call.call_id.clone(),
                    is_error: out.is_error,
                    text: out.text.clone(),
                })
                .await;
            parts.push(Part::ToolResult {
                call_id: call.call_id,
                content: vec![Part::text(out.text)],
                is_error: out.is_error,
            });
        }
        parts
    }

    /// Start a worker in the background for the lead, and say so at once.
    fn start_worker<'s>(
        &'s self,
        conv: ConvId,
        call: &PendingCall,
        events: &mpsc::Sender<TurnEvent>,
        team: &Team<'s>,
    ) -> ToolOutput {
        if team.cancel.is_cancelled() {
            return ToolOutput::error("cancelled by the user before it ran");
        }
        let agent = serde_json::from_str::<serde_json::Value>(&call.args)
            .ok()
            .and_then(|a| a.get("agent")?.as_str().map(str::to_owned))
            .unwrap_or_default();
        if self.agents.as_ref().and_then(|a| a.get(&agent)).is_none() {
            return ToolOutput::error(format!("there is no agent called {agent}"));
        }
        if team.running().len() >= MAX_PARALLEL_AGENTS {
            return ToolOutput::error(format!(
                "{MAX_PARALLEL_AGENTS} workers are already running; call wait_for_team until one reports"
            ));
        }
        let id = call.call_id.clone();
        let (call, events, cancel) = (call.clone(), events.clone(), team.cancel.clone());
        let name = agent.clone();
        team.start(
            id.clone(),
            agent.clone(),
            Box::pin(async move {
                let out = self.run_agent(conv, &call, &events, &cancel).await;
                Report {
                    dispatch: call.call_id,
                    agent: name,
                    text: out.text,
                    is_error: out.is_error,
                }
            }),
        );
        ToolOutput::ok(format!(
            "Started {agent} ({id}). It works in the background while you go on: call wait_for_team for its messages and its report."
        ))
    }

    /// What the team said and reported since the lead last asked, once
    /// something needs it or a while has passed.
    async fn wait_for_team(&self, conv: ConvId, team: &Team<'_>) -> ToolOutput {
        // Whatever the user says, and what workers address to the lead.
        let for_lead = |n: &Note| {
            n.from == USER
                || n.to.as_deref().is_some_and(|t| {
                    let t = t.to_lowercase();
                    t.contains(LEAD) || t.contains("coordinator")
                })
        };
        let cursor = *team
            .cursor
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        team.settle(TEAM_WAIT, || {
            self.channel.any_since(conv, LEAD, cursor, for_lead)
        })
        .await;
        let notes = {
            let mut c = team
                .cursor
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            self.channel.unread(conv, LEAD, &mut c)
        };
        let reports = team.take_reports();
        let running = team.running();

        let mut out = String::new();
        if notes.is_empty() && reports.is_empty() && !running.is_empty() {
            out.push_str("Nothing new yet. ");
        }
        if !notes.is_empty() {
            out.push_str("Messages:");
            for n in &notes {
                let to =
                    n.to.as_deref()
                        .map(|t| format!(" (to {t})"))
                        .unwrap_or_default();
                out.push_str(&format!("\n- from {}{to}: {}", n.from, n.text));
            }
            out.push_str("\n\n");
        }
        for r in &reports {
            let how = if r.is_error { "failed" } else { "reported" };
            out.push_str(&format!(
                "{} ({}) {how}:\n{}\n\n",
                r.agent, r.dispatch, r.text
            ));
        }
        if running.is_empty() {
            out.push_str("No one is working now.");
        } else {
            let names: Vec<String> = running.iter().map(|(d, a)| format!("{a} ({d})")).collect();
            out.push_str(&format!("Still working: {}.", names.join(", ")));
        }
        ToolOutput::ok(out)
    }

    /// The lead asking the user, waiting for the answer; workers go on meanwhile.
    async fn ask_user(
        &self,
        conv: ConvId,
        call: &PendingCall,
        events: &mpsc::Sender<TurnEvent>,
        cancel: &CancellationToken,
    ) -> ToolOutput {
        let args: serde_json::Value = serde_json::from_str(&call.args).unwrap_or_default();
        let Some(question) = args["question"].as_str().map(str::to_owned) else {
            return ToolOutput::error("ask_user needs \"question\"");
        };
        let post = |what, from: &str, text: &str| TeamPost {
            what,
            from: from.to_owned(),
            dispatch: String::new(),
            to: None,
            text: text.to_owned(),
        };
        let (tx, rx) = oneshot::channel();
        if let Ok(mut m) = self.questions.lock() {
            m.insert(call.call_id.clone(), tx);
        }
        self.team(conv, events, post(TeamWhat::Question, LEAD, &question))
            .await;
        let _ = events
            .send(TurnEvent::Question {
                call_id: call.call_id.clone(),
                text: question,
            })
            .await;
        let answer = tokio::select! {
            a = rx => a.ok(),
            () = cancel.cancelled() => None,
        };
        if let Ok(mut m) = self.questions.lock() {
            m.remove(&call.call_id);
        }
        match answer {
            Some(answer) => {
                self.team(conv, events, post(TeamWhat::Answer, USER, &answer))
                    .await;
                ToolOutput::ok(answer)
            }
            None => ToolOutput::error("cancelled by the user before answering"),
        }
    }

    /// The lead writing to its running workers.
    async fn lead_post(
        &self,
        conv: ConvId,
        call: &PendingCall,
        events: &mpsc::Sender<TurnEvent>,
    ) -> ToolOutput {
        let args: serde_json::Value = serde_json::from_str(&call.args).unwrap_or_default();
        let Some(text) = args["text"].as_str() else {
            return ToolOutput::error("post_message needs \"text\"");
        };
        let to = args["to"].as_str().map(str::to_owned);
        let note = Note {
            from: LEAD.to_owned(),
            to: to.clone(),
            text: text.to_owned(),
        };
        if !self.channel.post(conv, note) {
            return ToolOutput::error(format!(
                "you have already posted {MAX_POSTS_PER_HELPER} messages this turn"
            ));
        }
        self.team(
            conv,
            events,
            TeamPost {
                what: TeamWhat::Note,
                from: LEAD.to_owned(),
                dispatch: String::new(),
                to,
                text: text.chars().take(MAX_NOTE_CHARS).collect(),
            },
        )
        .await;
        ToolOutput::ok("sent")
    }

    /// Run one call. `helper` is the id of the dispatched agent making it, which
    /// may not dispatch agents of its own.
    async fn run_call(
        &self,
        conv: ConvId,
        call: &PendingCall,
        events: &mpsc::Sender<TurnEvent>,
        cancel: &CancellationToken,
        helper: Option<&str>,
        // A helper's own copy of the tools, working in its copy of the project.
        scope: Option<&Arc<dyn ToolHost>>,
    ) -> ToolOutput {
        let Some(host) = scope.or(self.tools.as_ref()) else {
            return ToolOutput::error("no tools are available");
        };
        if cancel.is_cancelled() {
            return ToolOutput::error("cancelled by the user before it ran");
        }
        if call.name == DISPATCH_AGENT {
            if helper.is_some() {
                return ToolOutput::error("a helper agent cannot dispatch helpers of its own");
            }
            // Boxed: a helper's own calls come back through run_call.
            return Box::pin(self.run_agent(conv, call, events, cancel)).await;
        }
        let args: serde_json::Value = match serde_json::from_str(&call.args) {
            Ok(v) => v,
            Err(e) => return ToolOutput::error(format!("the arguments were not valid JSON: {e}")),
        };
        if call.name == POST_MESSAGE {
            let (Some(from), Some(text)) = (helper, args["text"].as_str()) else {
                return ToolOutput::error("post_message is for helpers and needs \"text\"");
            };
            let to = args["to"].as_str().map(str::to_owned);
            let note = Note {
                from: from.to_owned(),
                to: to.clone(),
                text: text.to_owned(),
            };
            return if self.channel.post(conv, note) {
                // `from` is "agent (dispatch id)", as run_agent_steps names it.
                let (agent, dispatch) = from
                    .rsplit_once(" (")
                    .map_or((from, ""), |(a, d)| (a, d.trim_end_matches(')')));
                self.team(
                    conv,
                    events,
                    TeamPost {
                        what: TeamWhat::Note,
                        from: agent.to_owned(),
                        dispatch: dispatch.to_owned(),
                        to,
                        text: text.chars().take(MAX_NOTE_CHARS).collect(),
                    },
                )
                .await;
                ToolOutput::ok("sent")
            } else {
                ToolOutput::error(format!(
                    "you have already posted {MAX_POSTS_PER_HELPER} messages; finish your task and report"
                ))
            };
        }

        let allowed = self.rules.read().is_ok_and(|r| r.allows(&call.name, &args));
        if host.needs_approval(&call.name) && !allowed {
            let (tx, rx) = oneshot::channel();
            if let Ok(mut m) = self.approvals.lock() {
                m.insert(call.call_id.clone(), tx);
            }
            let _ = events
                .send(TurnEvent::ApprovalRequired {
                    call_id: call.call_id.clone(),
                    name: call.name.clone(),
                    args: call.args.clone(),
                })
                .await;
            let approved = tokio::select! {
                answer = rx => answer.unwrap_or(false),
                () = cancel.cancelled() => false,
            };
            if let Ok(mut m) = self.approvals.lock() {
                m.remove(&call.call_id);
            }
            if !approved {
                return ToolOutput::error("the user declined to run this tool");
            }
        }

        // Snapshot the file it will edit, so the change can be shown and undone.
        // An unreadable file is skipped rather than recorded as "created".
        let target = host.file_for(&call.name, &args);
        let lock = target.as_deref().map(|p| self.file_lock(p));
        let _guard = match &lock {
            Some(l) => tokio::select! {
                g = l.lock() => Some(g),
                () = cancel.cancelled() => {
                    return ToolOutput::error("cancelled by the user before it ran");
                }
            },
            None => None,
        };
        let before = target
            .as_deref()
            .and_then(|p| crate::edits::snapshot(p).ok());

        let mut out = tokio::select! {
            out = host.call(&call.name, args) => out,
            () = cancel.cancelled() => ToolOutput::error("cancelled by the user while running"),
        };

        // Recorded even when the tool reports an error: what matters is
        // whether the file changed.
        if let (Some(path), Some(before)) = (&target, before) {
            match crate::edits::snapshot(path) {
                Ok(after) if after != before => {
                    if let Err(e) = self.record_edit(conv, &call.call_id, path, &before, &after) {
                        tracing::warn!(error = %e, "could not record a file edit; it cannot be undone");
                    }
                }
                Ok(_) => {}
                Err(e) => tracing::warn!(error = %e, "could not read a file after editing it"),
            }
        }
        if out.text.chars().count() > MAX_TOOL_OUTPUT_CHARS {
            out.text = out.text.chars().take(MAX_TOOL_OUTPUT_CHARS).collect();
            out.text.push_str("\n[output truncated]");
        }
        out
    }

    /// Save a line of the team chat and show it live. Saving can fail without
    /// losing the run: the line then shows only until the app reloads.
    async fn team(&self, conv: ConvId, events: &mpsc::Sender<TurnEvent>, post: TeamPost) {
        let kind = EventKind::Team(post.clone());
        let id = self
            .with_store(|s| Ok(s.record_event(conv, None, &kind)?))
            .inspect_err(|e| tracing::warn!(error = %e, "could not save a team message"))
            .unwrap_or_else(|_| EventId::new());
        let _ = events
            .send(TurnEvent::Team {
                id: id.to_string(),
                at: Utc::now(),
                post,
            })
            .await;
    }

    /// Run one helper agent to the end, in memory, and return its report.
    ///
    /// Its own tool calls go through `run_call`, so they ask for approval and
    /// record their file edits exactly as the main turn's do.
    /// Run a helper, then save what it did, however it ended, so the user can
    /// read its work after the turn.
    async fn run_agent(
        &self,
        conv: ConvId,
        call: &PendingCall,
        events: &mpsc::Sender<TurnEvent>,
        cancel: &CancellationToken,
    ) -> ToolOutput {
        let mut log = Vec::new();
        let args = serde_json::from_str::<serde_json::Value>(&call.args).unwrap_or_default();
        let field = |k: &str| {
            args.get(k)
                .and_then(serde_json::Value::as_str)
                .unwrap_or_default()
        };
        let agent = field("agent").to_owned();
        self.team(
            conv,
            events,
            TeamPost {
                what: TeamWhat::Brief,
                from: "lead".to_owned(),
                dispatch: call.call_id.clone(),
                to: Some(agent.clone()),
                text: field("prompt").to_owned(),
            },
        )
        .await;

        let isolated = match self.tools.as_ref().and_then(|t| t.isolate(&call.call_id)) {
            Some(Ok(iso)) => {
                step(
                    events,
                    call,
                    &agent,
                    &mut log,
                    HelperEntry::Isolated {
                        root: iso.root.clone(),
                        folder: iso.folder.clone(),
                        branch: iso.branch.clone(),
                        base: iso.base.clone(),
                    },
                )
                .await;
                Some(iso)
            }
            Some(Err(e)) => {
                // Not fatal: it works in the project, as outside git.
                tracing::warn!(error = %e, "could not give a helper its own copy");
                None
            }
            None => None,
        };

        let mut out = self
            .run_agent_steps(
                conv,
                call,
                events,
                cancel,
                &mut log,
                isolated.as_ref().map(|i| &i.host),
            )
            .await;

        if let Some(iso) = &isolated {
            let summary = field("prompt").lines().next().unwrap_or_default();
            let summary: String = summary.chars().take(72).collect();
            match (iso.seal)(&format!("{agent}: {summary}")) {
                Ok(true) => out.text.push_str(&format!(
                    "

[{agent} worked in its own copy of the project. Its changes are on branch {}, not in the project's files until the user merges them.]",
                    iso.branch
                )),
                Ok(false) => {}
                Err(e) => {
                    tracing::warn!(error = %e, "could not save a helper's work");
                    out.text.push_str(&format!(
                        "

[{agent}'s work could not be saved on its branch {}: {e}. It is still in {}.]",
                        iso.branch, iso.folder
                    ));
                }
            }
        }

        self.team(
            conv,
            events,
            TeamPost {
                what: if out.is_error {
                    TeamWhat::Failed
                } else {
                    TeamWhat::Report
                },
                from: agent.clone(),
                dispatch: call.call_id.clone(),
                to: Some("lead".to_owned()),
                text: out.text.clone(),
            },
        )
        .await;

        if !log.is_empty() {
            let kind = EventKind::HelperLog {
                dispatch: call.call_id.clone(),
                agent,
                entries: log,
            };
            if let Err(e) = self.with_store(|s| Ok(s.record_event(conv, None, &kind)?)) {
                tracing::warn!(error = %e, "could not save a helper's log");
            }
        }
        out
    }

    async fn run_agent_steps(
        &self,
        conv: ConvId,
        call: &PendingCall,
        events: &mpsc::Sender<TurnEvent>,
        cancel: &CancellationToken,
        log: &mut Vec<HelperEntry>,
        scope: Option<&Arc<dyn ToolHost>>,
    ) -> ToolOutput {
        use futures_util::StreamExt as _;

        let args: serde_json::Value = serde_json::from_str(&call.args).unwrap_or_default();
        let field = |k: &str| args.get(k).and_then(serde_json::Value::as_str);
        let (Some(agent), Some(prompt)) = (field("agent"), field("prompt")) else {
            return ToolOutput::error("dispatch_agent needs \"agent\" and \"prompt\"");
        };
        let Some(def) = self.agents.as_ref().and_then(|a| a.get(agent)).cloned() else {
            return ToolOutput::error(format!("there is no agent called {agent}"));
        };
        let chat = match self.with_store(|s| Ok(s.get_conversation(conv)?)) {
            Ok(Some(c)) => c.current_model,
            _ => return ToolOutput::error("the conversation is gone"),
        };
        // Its agent's own model, else the worker model setting, else the chat's.
        let first = def
            .model
            .clone()
            .or_else(|| self.worker_model.read().ok().and_then(|m| m.clone()))
            .unwrap_or(chat.clone());
        // That model, then the fallbacks: a helper whose model fails in a
        // way its provider's retries could not fix moves down this list.
        let mut models: VecDeque<ModelRef> = self
            .worker_fallbacks
            .read()
            .map(|f| f.clone())
            .unwrap_or_default()
            .into_iter()
            .filter(|m| *m != first)
            .collect();
        models.push_front(first.clone());
        let Some((mut target, mut backend)) = self.next_worker_model(&mut models) else {
            return ToolOutput::error(format!(
                "{} does not support tools, so {agent} cannot work, and no fallback model that does is set; use a model from Anthropic or an OpenAI-compatible endpoint",
                first.backend
            ));
        };
        if target == first && first != chat {
            // So the user sees which model each helper is on.
            step(
                events,
                call,
                agent,
                log,
                HelperEntry::Model {
                    name: model_name(&first),
                },
            )
            .await;
        }
        if target != first {
            step(
                events,
                call,
                agent,
                log,
                HelperEntry::Switch {
                    from: model_name(&first),
                    to: model_name(&target),
                    error: "it cannot use tools here".to_owned(),
                },
            )
            .await;
        }
        let mut tools = self.tools.as_ref().map(|h| h.specs()).unwrap_or_default();
        // Workers run side by side: each opening its own folder picker would
        // stack them up. The coordinator asks, and names the folder in the brief.
        tools.retain(|t| t.name != "choose_location");
        tools.push(post_spec());
        let me = format!("{agent} ({})", call.call_id);
        let mut seen = 0;
        let mut system = format!("{}\n\n{AGENT_NOTE}", def.body.trim());
        // A worker starts with only its brief: the project outline saves it
        // exploring folder by folder.
        if let Some(context) = self.tools.as_ref().and_then(|t| t.context()) {
            system.push_str("\n\n");
            system.push_str(&context);
        }
        let limits = |target: &ModelRef| {
            let caps = self.profiles.capabilities(target);
            let max_output = caps.max_output_tokens;
            // 80% of the window, less the reply: the rest is margin for the system
            // prompt, tool definitions and the estimate being only an estimate.
            let budget = (caps.context_window / 10 * 8).saturating_sub(max_output);
            (max_output, budget)
        };
        let (mut max_output, mut budget) = limits(&target);
        let mut messages = vec![RenderedMessage {
            source_id: MsgId::new(),
            role: Role::User,
            parts: vec![Part::text(prompt)],
        }];
        let mut reconnects = 0;

        for _ in 0..MAX_AGENT_ROUNDS {
            if cancel.is_cancelled() {
                return ToolOutput::error("cancelled by the user");
            }
            trim_history(&mut messages, budget);
            // Between steps only, so it follows a tool result and never splits the prompt.
            if messages.last().is_some_and(|m| m.role == Role::Tool) {
                let notes = self.channel.unread(conv, &me, &mut seen);
                if !notes.is_empty() {
                    let text: String = notes
                        .iter()
                        .map(|n| {
                            let to =
                                n.to.as_deref()
                                    .map(|t| format!(" (to {t})"))
                                    .unwrap_or_default();
                            format!(
                                "
- from {}{to}: {}",
                                n.from, n.text
                            )
                        })
                        .collect();
                    step(
                        events,
                        call,
                        agent,
                        log,
                        HelperEntry::Received {
                            text: text.trim_start().to_owned(),
                        },
                    )
                    .await;
                    messages.push(RenderedMessage {
                        source_id: MsgId::new(),
                        role: Role::User,
                        parts: vec![Part::text(format!(
                            "Messages from the other helpers:{text}"
                        ))],
                    });
                }
            }
            let request = BackendRequest {
                model: target.clone(),
                view: RenderedView {
                    system: Some(system.clone()),
                    messages: messages.clone(),
                },
                max_output_tokens: max_output,
                thinking: self.profiles.thinking(&target),
                raw_payloads: BTreeMap::new(),
                blobs: BTreeMap::new(),
                tools: tools.clone(),
            };
            let mut text = String::new();
            let mut opaque = Vec::new();
            let mut calls: Vec<PendingCall> = Vec::new();
            let mut stop = StopReason::EndTurn;
            let mut failed = None;
            let mut dropped = false;
            let mut limited = false;
            let mut stream = match backend.stream(request, cancel.clone()).await {
                Ok(s) => Some(s),
                Err(e) => {
                    dropped = matches!(e, BackendError::Transport { .. });
                    limited = e.is_rate_limit();
                    failed = Some(format!("could not start: {e}"));
                    None
                }
            };
            while let Some(event) = match stream.as_mut() {
                Some(s) => s.next().await,
                None => None,
            } {
                match event {
                    BackendEvent::TextDelta(t) => text.push_str(&t),
                    BackendEvent::OpaqueBlock(o) => opaque.push(o),
                    BackendEvent::ToolCallDelta {
                        call_id,
                        name,
                        args_delta,
                    } => match calls.iter_mut().find(|c| c.call_id == call_id) {
                        Some(c) => {
                            c.args.push_str(&args_delta);
                            if let Some(n) = name {
                                c.name = n;
                            }
                        }
                        None => calls.push(PendingCall {
                            call_id,
                            name: name.unwrap_or_default(),
                            args: args_delta,
                        }),
                    },
                    BackendEvent::Done { stop_reason } => {
                        stop = stop_reason;
                        break;
                    }
                    BackendEvent::Error(e) => {
                        dropped = matches!(e, BackendError::Transport { .. });
                        limited = e.is_rate_limit();
                        failed = Some(format!("failed: {e}"));
                        break;
                    }
                    _ => {}
                }
            }
            if let Some(error) = failed {
                if cancel.is_cancelled() {
                    return ToolOutput::error("cancelled by the user");
                }
                // A dropped connection is not the model's fault: wait and try
                // the round again on the same one before falling back. A rate
                // limit is waited out too, unless a fallback can go on now.
                let wait_out = dropped || (limited && models.is_empty());
                if let Some(&wait) = self.reconnect_waits.get(reconnects).filter(|_| wait_out) {
                    reconnects += 1;
                    step(
                        events,
                        call,
                        agent,
                        log,
                        HelperEntry::Retry {
                            error: error.clone(),
                            wait_secs: wait.as_secs(),
                        },
                    )
                    .await;
                    tokio::select! {
                        () = tokio::time::sleep(wait) => {}
                        () = cancel.cancelled() => return ToolOutput::error("cancelled by the user"),
                    }
                    continue;
                }
                let Some((next, next_backend)) = self.next_worker_model(&mut models) else {
                    return ToolOutput::error(format!("{agent} {error}"));
                };
                step(
                    events,
                    call,
                    agent,
                    log,
                    HelperEntry::Switch {
                        from: model_name(&target),
                        to: model_name(&next),
                        error: error.clone(),
                    },
                )
                .await;
                (target, backend) = (next, next_backend);
                (max_output, budget) = limits(&target);
                // Signed reasoning and the like only make sense to the provider
                // that wrote them.
                for m in &mut messages {
                    m.parts.retain(|p| !matches!(p, Part::Opaque(_)));
                }
                // The failed round is tried again on the new model.
                continue;
            }

            if !text.trim().is_empty() {
                step(
                    events,
                    call,
                    agent,
                    log,
                    HelperEntry::Text { text: text.clone() },
                )
                .await;
            }
            let mut parts: Vec<Part> = opaque.into_iter().map(Part::Opaque).collect();
            if !text.is_empty() {
                parts.push(Part::text(text.clone()));
            }
            parts.extend(calls.iter().map(|c| Part::ToolCall {
                call_id: c.call_id.clone(),
                name: c.name.clone(),
                args_json: c.args.clone(),
            }));
            messages.push(RenderedMessage {
                source_id: MsgId::new(),
                role: Role::Assistant,
                parts,
            });
            if stop != StopReason::ToolUse || calls.is_empty() {
                let report = if text.trim().is_empty() {
                    format!("{agent} finished without a report")
                } else {
                    text
                };
                // The coordinator should know another model finished the work.
                return ToolOutput::ok(if target == first {
                    report
                } else {
                    format!(
                        "{report}\n\n({agent} finished on {} because {} failed.)",
                        model_name(&target),
                        model_name(&first)
                    )
                });
            }

            let mut results = Vec::with_capacity(calls.len());
            for c in calls {
                // Providers number calls their own way, and two helpers can both
                // say "call_0": approvals and saved edits are keyed by this id.
                let scoped = PendingCall {
                    call_id: format!("{me}:{}", c.call_id),
                    name: c.name.clone(),
                    args: c.args.clone(),
                };
                let _ = events
                    .send(TurnEvent::ToolCall {
                        call_id: scoped.call_id.clone(),
                        name: format!("{agent}: {}", c.name),
                        args: c.args.clone(),
                    })
                    .await;
                step(
                    events,
                    call,
                    agent,
                    log,
                    HelperEntry::Call {
                        call_id: scoped.call_id.clone(),
                        name: c.name.clone(),
                        args: c.args.clone(),
                    },
                )
                .await;
                let out = self
                    .run_call(conv, &scoped, events, cancel, Some(&me), scope)
                    .await;
                let _ = events
                    .send(TurnEvent::ToolResult {
                        call_id: scoped.call_id.clone(),
                        is_error: out.is_error,
                        text: out.text.clone(),
                    })
                    .await;
                step(
                    events,
                    call,
                    agent,
                    log,
                    HelperEntry::Result {
                        call_id: scoped.call_id.clone(),
                        is_error: out.is_error,
                        text: clip(&out.text),
                    },
                )
                .await;
                results.push(Part::ToolResult {
                    call_id: c.call_id,
                    content: vec![Part::text(out.text)],
                    is_error: out.is_error,
                });
            }
            messages.push(RenderedMessage {
                source_id: MsgId::new(),
                role: Role::Tool,
                parts: results,
            });
        }
        ToolOutput::error(format!(
            "{agent} stopped after {MAX_AGENT_ROUNDS} rounds of tool calls"
        ))
    }

    fn record_edit(
        &self,
        conv: ConvId,
        call_id: &str,
        path: &std::path::Path,
        before: &Option<Vec<u8>>,
        after: &Option<Vec<u8>>,
    ) -> Result<()> {
        let put = |b: &Option<Vec<u8>>| b.as_deref().map(|b| self.blobs.put(b)).transpose();
        let edit = FileEdit {
            call_id: call_id.to_owned(),
            conversation_id: conv,
            path: path.display().to_string(),
            before_blob: put(before)?,
            after_blob: put(after)?,
            undone: false,
            created_at: Utc::now(),
        };
        self.with_store(|s| Ok(s.save_file_edit(&edit)?))
    }

    /// Every file edit in a conversation, with its diff.
    ///
    /// # Errors
    /// Fails on a database or blob store error.
    pub fn file_edits(&self, conv: ConvId) -> Result<Vec<EditView>> {
        let edits = self.with_store(|s| Ok(s.file_edits(conv)?))?;
        edits
            .into_iter()
            .map(|e| {
                let before = self.blob(e.before_blob.as_deref())?;
                let after = self.blob(e.after_blob.as_deref())?;
                Ok(EditView {
                    diff: crate::edits::unified_diff(&e.path, before.as_deref(), after.as_deref()),
                    call_id: e.call_id,
                    path: e.path,
                    undone: e.undone,
                })
            })
            .collect()
    }

    /// Put a file back exactly as it was before an edit.
    ///
    /// Refused if the file has changed since the edit: restoring then would
    /// throw away whatever changed it, and that is not what Undo means.
    ///
    /// # Errors
    /// Fails if there is no such edit, it was already undone, the file has
    /// changed since, or the file cannot be written.
    pub fn undo_edit(&self, call_id: &str) -> Result<()> {
        let edit = self
            .with_store(|s| Ok(s.get_file_edit(call_id)?))?
            .ok_or_else(|| {
                CoreError::UndoRefused("there is no edit for that tool call".to_owned())
            })?;
        if edit.undone {
            return Err(CoreError::UndoRefused("it was already undone".to_owned()));
        }
        let path = std::path::Path::new(&edit.path);
        let refuse = |why: String| CoreError::UndoRefused(format!("{}: {why}", edit.path));
        let now = crate::edits::snapshot(path).map_err(|e| refuse(e.to_string()))?;
        if now != self.blob(edit.after_blob.as_deref())? {
            return Err(refuse(
                "the file has changed since this edit, and undoing it would discard those changes"
                    .to_owned(),
            ));
        }
        let before = self.blob(edit.before_blob.as_deref())?;
        crate::edits::restore(path, before.as_deref()).map_err(|e| refuse(e.to_string()))?;
        self.with_store(|s| Ok(s.mark_edit_undone(call_id)?))
    }

    fn blob(&self, hash: Option<&str>) -> Result<Option<Vec<u8>>> {
        Ok(hash.map(|h| self.blobs.get(h)).transpose()?)
    }

    /// One model call: build the view, stream the reply, persist it. Returns
    /// the tool calls the model made, if it stopped to make any.
    async fn run_step(
        &self,
        conv: ConvId,
        events: &mpsc::Sender<TurnEvent>,
        cancel: &CancellationToken,
        context: Option<&str>,
    ) -> Result<(MsgId, Vec<PendingCall>)> {
        let (events, cancel) = (events.clone(), cancel.clone());
        let mut attempt = 0;
        loop {
            let may_retry = attempt < self.reconnect_waits.len();
            // The dropped reply, or the refusal when nothing started.
            let (error, dropped) = match self
                .try_step(conv, &events, &cancel, context, may_retry)
                .await
            {
                Ok(Step::Done(reply, calls)) => return Ok((reply, calls)),
                Ok(Step::Dropped { reply, error }) => (error, Ok(reply)),
                Err(CoreError::Backend(e)) if may_retry && e.is_temporary() => {
                    (e.to_string(), Err(e))
                }
                Err(e) => return Err(e),
            };
            let wait = self.reconnect_waits[attempt];
            attempt += 1;
            let _ = events
                .send(TurnEvent::Retrying {
                    error: error.clone(),
                    wait_secs: wait.as_secs(),
                })
                .await;
            tokio::select! {
                () = tokio::time::sleep(wait) => {}
                () = cancel.cancelled() => {
                    let message = dropped.as_ref().ok().copied();
                    let _ = events.send(TurnEvent::Failed { message, error }).await;
                    return dropped.map(|id| (id, Vec::new())).map_err(Into::into);
                }
            }
            // The dropped reply stays as a failed sibling; the retry is a new one.
            if let Ok(id) = dropped {
                self.prepare_regenerate(conv, id)?;
            }
        }
    }

    /// One attempt at a step. With `may_retry`, a dropped connection is
    /// returned as [`Step::Dropped`] rather than reported as a failure.
    async fn try_step(
        &self,
        conv: ConvId,
        events: &mpsc::Sender<TurnEvent>,
        cancel: &CancellationToken,
        context: Option<&str>,
        may_retry: bool,
    ) -> Result<Step> {
        let (events, cancel) = (events.clone(), cancel.clone());
        let (target, parent) = self.with_store(|s| {
            let c = s
                .get_conversation(conv)?
                .ok_or_else(|| CoreError::ConversationNotFound(conv.to_string()))?;
            Ok((c.current_model, c.active_leaf_id))
        })?;

        let _ = events
            .send(TurnEvent::PreparingContext {
                target: target.clone(),
            })
            .await;

        let (view, rendered) = self
            .prepare_view(conv, &target, &events, &cancel, context)
            .await?;
        let caps = self.profiles.capabilities(&target);
        let team = self.team_convs.lock().is_ok_and(|t| t.contains(&conv));
        let request =
            self.assemble_request(&target, &view, rendered, caps.max_output_tokens, team)?;

        let _ = events
            .send(TurnEvent::ContextReady {
                view: Box::new(view.clone()),
            })
            .await;

        let backend = self.backend(&target.backend)?;
        let stream = match backend.stream(request, cancel.clone()).await {
            Ok(s) => s,
            Err(e) if may_retry && e.is_temporary() => return Err(e.into()),
            Err(e) => {
                // Nothing streamed, so nothing to preserve: report and stop.
                let _ = events
                    .send(TurnEvent::Failed {
                        message: None,
                        error: e.to_string(),
                    })
                    .await;
                return Err(e.into());
            }
        };

        self.consume(
            conv, parent, target, view, stream, events, cancel, may_retry,
        )
        .await
    }

    #[allow(clippy::too_many_arguments, clippy::too_many_lines)]
    async fn consume(
        &self,
        conv: ConvId,
        parent: Option<MsgId>,
        target: ModelRef,
        view: ContextView,
        mut stream: futures_util::stream::BoxStream<'static, BackendEvent>,
        events: mpsc::Sender<TurnEvent>,
        cancel: CancellationToken,
        may_retry: bool,
    ) -> Result<Step> {
        use futures_util::StreamExt as _;

        let mut calls: Vec<PendingCall> = Vec::new();
        let mut text = String::new();
        let mut reasoning = String::new();
        let mut opaque: Vec<ProviderOpaque> = Vec::new();
        let mut raw: Option<serde_json::Value> = None;
        let mut stop = StopReason::EndTurn;
        let mut failure: Option<String> = None;
        let mut dropped = false;
        let mut last_checkpoint = Instant::now();

        while let Some(event) = stream.next().await {
            match event {
                BackendEvent::TextDelta(t) => {
                    text.push_str(&t);
                    let _ = events.send(TurnEvent::TextDelta { text: t }).await;
                }
                BackendEvent::ReasoningDelta(t) => {
                    reasoning.push_str(&t);
                    let _ = events.send(TurnEvent::ReasoningDelta { text: t }).await;
                }
                BackendEvent::OpaqueBlock(o) => opaque.push(o),
                BackendEvent::RawPayload(v) => raw = Some(v),
                BackendEvent::PrefillProgress {
                    reused_tokens,
                    done_tokens,
                    total_tokens,
                } => {
                    let _ = events
                        .send(TurnEvent::PrefillProgress {
                            reused_tokens,
                            done_tokens,
                            total_tokens,
                        })
                        .await;
                }
                BackendEvent::Usage {
                    input_tokens,
                    output_tokens,
                } => {
                    let _ = events
                        .send(TurnEvent::Usage {
                            input_tokens,
                            output_tokens,
                        })
                        .await;
                }
                BackendEvent::Done { stop_reason } => {
                    stop = stop_reason;
                    break;
                }
                BackendEvent::Error(e) => {
                    dropped = e.is_temporary();
                    failure = Some(e.to_string());
                    break;
                }
                BackendEvent::ToolCallDelta {
                    call_id,
                    name,
                    args_delta,
                } => match calls.iter_mut().find(|c| c.call_id == call_id) {
                    Some(c) => {
                        c.args.push_str(&args_delta);
                        if let Some(n) = name {
                            c.name = n;
                        }
                    }
                    None => calls.push(PendingCall {
                        call_id,
                        name: name.unwrap_or_default(),
                        args: args_delta,
                    }),
                },
            }

            // Checkpoint the partial reply so a crash costs at most a second.
            if last_checkpoint.elapsed() >= self.draft_interval {
                last_checkpoint = Instant::now();
                let draft = Draft {
                    conversation_id: conv,
                    parent_id: parent,
                    model: target.clone(),
                    partial_text: text.clone(),
                    updated_at: Utc::now(),
                };
                if let Err(e) = self.with_store(|s| Ok(s.save_draft(&draft)?)) {
                    tracing::warn!(error = %e, "could not checkpoint draft");
                }
            }
        }

        // Cancellation is reported by the backend as Done{Cancelled}, but the
        // token may also have been tripped without the stream noticing yet.
        if cancel.is_cancelled() && failure.is_none() {
            stop = StopReason::Cancelled;
        }

        let status = match (&failure, &stop) {
            (Some(reason), _) => MsgStatus::Failed {
                reason: reason.clone(),
            },
            (None, StopReason::Cancelled) => MsgStatus::Cancelled,
            _ => MsgStatus::Complete,
        };

        // Assemble the canonical message: neutral content plus the
        // provider-native blocks, in the order they arrived.
        let mut parts: Vec<Part> = Vec::new();
        if !reasoning.is_empty() {
            parts.push(Part::Reasoning {
                visible_text: Some(reasoning),
            });
        }
        parts.extend(opaque.into_iter().map(Part::Opaque));
        if !text.is_empty() {
            parts.push(Part::text(text));
        }
        parts.extend(calls.iter().map(|c| Part::ToolCall {
            call_id: c.call_id.clone(),
            name: c.name.clone(),
            args_json: c.args.clone(),
        }));
        if parts.is_empty() {
            // Persist the row anyway: an empty failed reply is still evidence
            // that the turn happened.
            parts.push(Part::text(String::new()));
        }

        let id = self.insert(
            conv,
            parent,
            Role::Assistant,
            Some(target),
            parts,
            status,
            raw,
        )?;

        self.with_store(|s| {
            s.save_context_view(id, &view)?;
            // The draft has been superseded by a real row.
            s.clear_draft(conv)?;
            Ok(())
        })?;

        // Run tools only for a reply that finished asking for them.
        let calls = if failure.is_none() && stop == StopReason::ToolUse {
            for c in &calls {
                let _ = events
                    .send(TurnEvent::ToolCall {
                        call_id: c.call_id.clone(),
                        name: c.name.clone(),
                        args: c.args.clone(),
                    })
                    .await;
            }
            calls
        } else {
            Vec::new()
        };

        match failure {
            Some(error) if dropped && may_retry && !cancel.is_cancelled() => {
                return Ok(Step::Dropped { reply: id, error });
            }
            Some(error) => {
                let _ = events
                    .send(TurnEvent::Failed {
                        message: Some(id),
                        error,
                    })
                    .await;
            }
            None => {
                let _ = events
                    .send(TurnEvent::Completed {
                        message: id,
                        stop_reason: stop,
                    })
                    .await;
            }
        }
        Ok(Step::Done(id, calls))
    }
}

/// How one attempt at a step ended.
enum Step {
    /// The reply and the tools it asks for (none if it failed).
    Done(MsgId, Vec<PendingCall>),
    /// The connection dropped partway; the partial reply is saved as failed.
    Dropped { reply: MsgId, error: String },
}

#[cfg(test)]
mod tests {
    // Tests may unwrap and expect: a panic is the failure report.
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;

    fn result(text: &str) -> RenderedMessage {
        RenderedMessage {
            source_id: MsgId::new(),
            role: Role::Tool,
            parts: vec![Part::ToolResult {
                call_id: "c".to_owned(),
                content: vec![Part::text(text)],
                is_error: false,
            }],
        }
    }

    fn text_of(m: &RenderedMessage) -> String {
        match &m.parts[0] {
            Part::ToolResult { content, .. } => content[0].as_text().unwrap().to_owned(),
            _ => unreachable!(),
        }
    }

    #[test]
    fn the_oldest_tool_results_go_first_and_the_newest_message_stays() {
        let big = "x".repeat(30_000); // about 11k tokens each
        let mut messages = vec![result(&big), result(&big), result(&big)];
        trim_history(&mut messages, 25_000);
        assert_eq!(text_of(&messages[0]), DROPPED, "oldest is dropped");
        assert_eq!(text_of(&messages[1]).len(), 30_000, "stops once it fits");
        assert_eq!(text_of(&messages[2]).len(), 30_000, "newest is kept");

        trim_history(&mut messages, 1);
        assert_eq!(text_of(&messages[1]), DROPPED);
        assert_eq!(text_of(&messages[2]).len(), 30_000, "never the newest");
    }

    #[test]
    fn a_history_that_fits_is_left_alone() {
        let mut messages = vec![result("small"), result("small")];
        trim_history(&mut messages, 10_000);
        assert_eq!(text_of(&messages[0]), "small");
    }
}
