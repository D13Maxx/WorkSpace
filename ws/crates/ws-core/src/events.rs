//! What the UI is told while a turn runs.
//!
//! Deliberately narrower than [`ws_context::BackendEvent`]: the UI needs
//! progress and text, not the provider-native blocks that only storage and
//! replay care about.

use serde::{Deserialize, Serialize};
use ws_context::{ContextView, HelperEntry, ModelRef, MsgId, StopReason, TeamPost};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum TurnEvent {
    /// The context is being assembled. Shown as "Preparing context for {model}…"
    /// and cancellable (master spec s6.5).
    PreparingContext {
        target: ModelRef,
    },

    /// A summary is being written because history did not fit.
    Summarising {
        writer: ModelRef,
        /// How many messages the summary will cover.
        covering: u32,
    },

    /// What the model was actually sent. Emitted once, before generation, so
    /// the inspector can show it even while the reply streams.
    ContextReady {
        view: Box<ContextView>,
    },

    /// Local engine only: progress reading the prompt (master spec s8.6).
    ///
    /// `reused_tokens` came from the KV cache and cost nothing, so the UI
    /// should base any time estimate on `done_tokens` alone.
    PrefillProgress {
        reused_tokens: u32,
        done_tokens: u32,
        total_tokens: u32,
    },

    TextDelta {
        text: String,
    },
    ReasoningDelta {
        text: String,
    },

    Usage {
        input_tokens: u32,
        output_tokens: u32,
    },

    /// The model asked to run a tool.
    ToolCall {
        call_id: String,
        name: String,
        /// The arguments as JSON text, exactly as the model wrote them.
        args: String,
    },

    /// A tool call is waiting for the user. Answer with
    /// `Orchestrator::resolve_approval`; the turn waits until then.
    ApprovalRequired {
        call_id: String,
        name: String,
        args: String,
    },

    ToolResult {
        call_id: String,
        is_error: bool,
        text: String,
    },

    /// A helper agent, working for the `dispatch_agent` call `dispatch`, said
    /// or did something. The whole log is saved when it finishes.
    HelperStep {
        dispatch: String,
        agent: String,
        entry: HelperEntry,
    },

    /// The lead asks the user something during a coordinated turn and waits;
    /// answer with `Orchestrator::answer_question`.
    Question {
        call_id: String,
        text: String,
    },

    /// A line of the team chat, already saved as event `id`.
    Team {
        id: String,
        at: chrono::DateTime<chrono::Utc>,
        #[serde(flatten)]
        post: TeamPost,
    },

    /// The reply is persisted. `message` is the row that was written.
    Completed {
        message: MsgId,
        stop_reason: StopReason,
    },

    /// The connection dropped (as when the laptop slept); the step starts
    /// again as a new reply after `wait_secs`.
    Retrying {
        error: String,
        wait_secs: u64,
    },

    /// Generation stopped early. The partial text is still persisted under
    /// `message`, so nothing the user saw is lost.
    Failed {
        message: Option<MsgId>,
        error: String,
    },
}
