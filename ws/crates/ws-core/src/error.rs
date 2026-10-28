//! Turn-level failures.

use thiserror::Error;
use ws_context::{BackendId, ContextError, ModelRef};

#[derive(Debug, Error)]
pub enum CoreError {
    #[error(transparent)]
    Store(#[from] ws_store::StoreError),

    #[error("cannot undo: {0}")]
    UndoRefused(String),

    #[error(transparent)]
    Context(#[from] ContextError),

    #[error("backend {0} is not configured")]
    UnknownBackend(BackendId),

    #[error("conversation {0} not found")]
    ConversationNotFound(String),

    #[error("conversation has no messages to answer")]
    EmptyConversation,

    #[error("{model} could not write the summary: {message}")]
    SummaryFailed { model: ModelRef, message: String },

    #[error("the context builder kept asking for summaries; this is a bug")]
    SummaryLoopDidNotConverge,

    #[error("{0}")]
    ToolLoop(String),

    #[error(transparent)]
    Backend(#[from] ws_context::BackendError),
}

pub type Result<T> = std::result::Result<T, CoreError>;
