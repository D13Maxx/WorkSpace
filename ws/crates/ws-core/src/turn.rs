//! Turn orchestration (master spec s4).
//!
//! One turn is: load the path, build the view the target model should see,
//! ask the backend for a reply, stream it to the UI, and persist exactly once
//! at the end.
//!
//! Two rules drive the shape of this file:
//!
//! - **The message row is written once**, when the reply completes, is
//!   cancelled, or fails (master spec s10). While it streams, the partial text
//!   lives in the `drafts` table, checkpointed about once a second, so a crash
//!   loses at most a second of output and never leaves a half-written message.
//! - **Nothing the user saw is discarded.** Cancel and failure both persist the
//!   partial text under a status that says what happened.

use crate::error::{CoreError, Result};
use crate::events::TurnEvent;
use crate::profiles::ProfileTable;
use crate::tools::{ToolHost, ToolOutput};
use chrono::Utc;
use std::collections::{BTreeMap, HashMap};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tokio::sync::{mpsc, oneshot};
use tokio_util::sync::CancellationToken;
use ws_context::{
    BackendEvent, BackendId, BackendRequest, BuildViewParams, ByteEstimateCounter, ChatBackend,
    ContextConfig, ContextSummary, ContextView, ConvId, Conversation, EventKind, InlineBlob,
    Message, ModelProfiles, ModelRef, MsgId, MsgStatus, Part, ProviderOpaque, RenderedMessage,
    RenderedView, Role, StopReason, SummaryId, SummaryRequest, ViewMode, ViewPlan, build_view,
    summary_prompt,
};
use ws_store::{BlobStore, Draft, NewMessage, Store};

/// Result of asking one backend for its models.
pub type ModelListResult =
    std::result::Result<Vec<ws_context::ModelInfo>, ws_context::BackendError>;

/// How often an in-progress reply is checkpointed (master spec s10).
const DRAFT_CHECKPOINT_INTERVAL: Duration = Duration::from_secs(1);

/// The context builder asks for at most one summary per turn. A second request
/// means the loop is not converging, which is a bug rather than a retry.
const MAX_SUMMARY_ROUNDS: usize = 1;

/// Model-then-tools rounds per user message. A model that keeps calling tools
/// past this is looping, and every round costs money.
const MAX_TOOL_ROUNDS: usize = 25;

/// Cap on one tool result, in characters. A whole-file read can otherwise be
/// larger than the model's window, and a message is never split.
// shortcut: truncates instead of paging, upgrade if agents need whole large files.
const MAX_TOOL_OUTPUT_CHARS: usize = 60_000;

/// A tool call the model asked for, assembled from its deltas.
#[derive(Debug, Clone, PartialEq, Eq)]
struct PendingCall {
    call_id: String,
    name: String,
    args: String,
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
    /// Calls waiting on the user, by call id. Resolved by `resolve_approval`.
    approvals: Mutex<HashMap<String, oneshot::Sender<bool>>>,
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
            approvals: Mutex::new(HashMap::new()),
        }
    }

    #[must_use]
    pub fn with_tools(mut self, host: Arc<dyn ToolHost>) -> Self {
        self.tools = Some(host);
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

    /// Tools are offered only when there is a host and the target's backend
    /// can carry them; otherwise tool history is flattened to text.
    fn tools_enabled(&self, target: &ModelRef) -> bool {
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
    /// being unreachable or unconfigured must not empty the whole picker
    /// (master spec s7.1).
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
    /// and never sent to a model (master spec s5.3).
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
    /// `message`, so the next turn creates a sibling rather than an overwrite
    /// (master spec s5.1 invariant 2).
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

        // Master spec s6.4: raw payloads are replayed only for a Full view of
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
                Some(host) if self.tools_enabled(target) => host.specs(),
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
    ) -> Result<(ContextView, RenderedView)> {
        let mut caps = self.profiles.capabilities(target);
        caps.tools = self.tools_enabled(target);
        // Ask the backend how it counts. The local engine counts exactly;
        // API backends fall back to the estimate (master spec s6.1).
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

    /// Ask a model to summarise the evicted prefix (master spec s6.5).
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

        let mut stream = backend.stream(request, cancel.clone()).await.map_err(|e| {
            CoreError::SummaryFailed {
                model: writer.clone(),
                message: e.to_string(),
            }
        })?;

        let mut text = String::new();
        use futures_util::StreamExt as _;
        while let Some(event) = stream.next().await {
            match event {
                BackendEvent::TextDelta(t) => text.push_str(&t),
                BackendEvent::Error(e) => {
                    return Err(CoreError::SummaryFailed {
                        model: writer,
                        message: e.to_string(),
                    });
                }
                BackendEvent::Done { .. } => break,
                _ => {}
            }
        }

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
        for _ in 0..MAX_TOOL_ROUNDS {
            let (reply, calls) = self.run_step(conv, &events, &cancel).await?;
            if calls.is_empty() {
                return Ok(reply);
            }
            // Every call gets a result, even after a cancel: a tool call with
            // no result is a request the provider rejects on the next turn.
            let results = self.run_tools(calls, &events, &cancel).await;
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
    async fn run_tools(
        &self,
        calls: Vec<PendingCall>,
        events: &mpsc::Sender<TurnEvent>,
        cancel: &CancellationToken,
    ) -> Vec<Part> {
        let mut parts = Vec::with_capacity(calls.len());
        for call in calls {
            let out = self.run_one(&call, events, cancel).await;
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

    async fn run_one(
        &self,
        call: &PendingCall,
        events: &mpsc::Sender<TurnEvent>,
        cancel: &CancellationToken,
    ) -> ToolOutput {
        let Some(host) = &self.tools else {
            return ToolOutput::error("no tools are available");
        };
        if cancel.is_cancelled() {
            return ToolOutput::error("cancelled by the user before it ran");
        }
        let args: serde_json::Value = match serde_json::from_str(&call.args) {
            Ok(v) => v,
            Err(e) => return ToolOutput::error(format!("the arguments were not valid JSON: {e}")),
        };

        if host.needs_approval(&call.name) {
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

        let mut out = tokio::select! {
            out = host.call(&call.name, args) => out,
            () = cancel.cancelled() => ToolOutput::error("cancelled by the user while running"),
        };
        if out.text.chars().count() > MAX_TOOL_OUTPUT_CHARS {
            out.text = out.text.chars().take(MAX_TOOL_OUTPUT_CHARS).collect();
            out.text.push_str("\n[output truncated]");
        }
        out
    }

    /// One model call: build the view, stream the reply, persist it. Returns
    /// the tool calls the model made, if it stopped to make any.
    async fn run_step(
        &self,
        conv: ConvId,
        events: &mpsc::Sender<TurnEvent>,
        cancel: &CancellationToken,
    ) -> Result<(MsgId, Vec<PendingCall>)> {
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

        let (view, rendered) = self.prepare_view(conv, &target, &events, &cancel).await?;
        let caps = self.profiles.capabilities(&target);
        let request = self.assemble_request(&target, &view, rendered, caps.max_output_tokens)?;

        let _ = events
            .send(TurnEvent::ContextReady {
                view: Box::new(view.clone()),
            })
            .await;

        let backend = self.backend(&target.backend)?;
        let stream = match backend.stream(request, cancel.clone()).await {
            Ok(s) => s,
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

        self.consume(conv, parent, target, view, stream, events, cancel)
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
    ) -> Result<(MsgId, Vec<PendingCall>)> {
        use futures_util::StreamExt as _;

        let mut calls: Vec<PendingCall> = Vec::new();
        let mut text = String::new();
        let mut reasoning = String::new();
        let mut opaque: Vec<ProviderOpaque> = Vec::new();
        let mut raw: Option<serde_json::Value> = None;
        let mut stop = StopReason::EndTurn;
        let mut failure: Option<String> = None;
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
        Ok((id, calls))
    }
}
