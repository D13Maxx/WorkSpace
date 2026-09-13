//! What the UI is told while a turn runs.
//!
//! Deliberately narrower than [`ws_context::BackendEvent`]: the UI needs
//! progress and text, not the provider-native blocks that only storage and
//! replay care about.

use serde::{Deserialize, Serialize};
use ws_context::{ContextView, ModelRef, MsgId, StopReason};

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

    /// Local engine only: prompt tokens read so far (master spec s8.6).
    PrefillProgress {
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

    /// The reply is persisted. `message` is the row that was written.
    Completed {
        message: MsgId,
        stop_reason: StopReason,
    },

    /// Generation stopped early. The partial text is still persisted under
    /// `message`, so nothing the user saw is lost.
    Failed {
        message: Option<MsgId>,
        error: String,
    },
}
