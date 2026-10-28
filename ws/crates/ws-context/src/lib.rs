//! Canonical conversation format and context builder.
//!
//! Pure data and pure functions: this crate performs no I/O, so every rule in
//! master spec s5 and s6 is unit- and property-testable without a database,
//! a network, or a model.

pub mod backend;
pub mod config;
pub mod error;
pub mod ids;
pub mod message;
pub mod model;
pub mod summary;
pub mod token;
pub mod view;

pub use backend::{
    BackendError, BackendEvent, BackendRequest, ChatBackend, DefaultProfiles, InlineBlob,
    ModelInfo, ModelProfiles, StopReason, ThinkingMode, ToolSpec,
};
pub use config::ContextConfig;
pub use error::ContextError;
pub use ids::{BackendId, BlobRef, ConvId, EventId, MsgId, SummaryId};
pub use message::{
    Conversation, ConversationEvent, EventKind, Message, MsgStatus, Part, ProviderOpaque, Role,
};
pub use model::{
    DEFAULT_CONTEXT_WINDOW, DEFAULT_IMAGE_TOKEN_COST, DEFAULT_MAX_OUTPUT_TOKENS, ModelCapabilities,
    ModelRef, ProviderKind, ReasoningSupport, TokenCounting,
};
pub use summary::{ContextSummary, SUMMARY_PROMPT_VERSION, summary_prompt};
pub use token::{ByteEstimateCounter, TokenCounter};
pub use view::{
    BuildViewParams, BuiltView, ContextView, OpaqueDropReason, RenderedMessage, RenderedView,
    SummaryRequest, TransformNote, ViewMode, ViewPlan, build_view,
};
