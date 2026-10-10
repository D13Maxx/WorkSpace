//! The interface every model source implements (master spec s4).
//!
//! Four API adapters and the local engine sit behind one trait, which is what
//! makes switching models mid-conversation a matter of picking a different
//! backend rather than a different code path.
//!
//! These are definitions, not I/O: the types are the vocabulary a turn is
//! described in, and the trait says what a backend must offer. Nothing here
//! opens a socket or touches a model.

use crate::ids::{BackendId, BlobRef, MsgId};
use crate::message::ProviderOpaque;
use crate::model::{ModelCapabilities, ModelRef};
use crate::token::TokenCounter;
use crate::view::RenderedView;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use thiserror::Error;

/// How much thinking a model should do. Per master spec s7.2 this lives in the
/// model profile and may change only between turns, because providers tie
/// thinking settings to the shape of the conversation they are replaying.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(tag = "mode", rename_all = "snake_case")]
pub enum ThinkingMode {
    #[default]
    Off,
    /// The provider decides how much to think. Some newer models accept only
    /// this and reject an explicit budget.
    Adaptive,
    Enabled {
        budget_tokens: u32,
    },
}

/// A model a backend can serve. Merged from the provider's list-models
/// endpoint and any ids the user added by hand (master spec s7.1).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModelInfo {
    pub model: ModelRef,
    pub display_name: String,
    /// Present when the provider advertises it; otherwise the profile supplies
    /// conservative defaults and the UI warns.
    pub known_capabilities: Option<ModelCapabilities>,
}

/// Everything a backend needs to produce one reply.
#[derive(Debug, Clone, PartialEq)]
pub struct BackendRequest {
    pub model: ModelRef,
    /// Built by the context builder: already transformed and budgeted.
    pub view: RenderedView,
    pub max_output_tokens: u32,
    pub thinking: ThinkingMode,
    /// Verbatim provider responses for messages being replayed, keyed by
    /// message id.
    ///
    /// Populated only for a Full view of the same model (master spec s6.4).
    /// Adapters that must resend a provider's own response shape use this
    /// rather than rebuilding it from canonical parts, because rebuilding is
    /// what produces 4xx errors on providers that sign their reasoning.
    pub raw_payloads: BTreeMap<MsgId, serde_json::Value>,
    /// Bytes for the images in `view`, keyed by blob reference.
    ///
    /// Adapters cannot read the blob store themselves, so the orchestrator
    /// resolves images before handing over a request. Only images that survived
    /// the context builder appear here, and the token budget already bounds how
    /// many that can be.
    pub blobs: BTreeMap<BlobRef, InlineBlob>,
    /// Tools offered for this turn. Empty unless the backend reports
    /// [`ChatBackend::supports_tools`].
    pub tools: Vec<ToolSpec>,
}

/// A tool the model may call, in the shape every provider accepts: a name, a
/// description, and a JSON Schema for the arguments. MCP tools already arrive
/// in exactly this form.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ToolSpec {
    pub name: String,
    pub description: String,
    pub input_schema: serde_json::Value,
}

/// An image resolved from the blob store, ready to inline in a request body.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InlineBlob {
    pub mime: String,
    pub bytes: Vec<u8>,
}

/// Why generation stopped.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StopReason {
    EndTurn,
    MaxTokens,
    StopSequence,
    /// The user pressed stop. Partial text is kept.
    Cancelled,
    /// Defined for later milestones; unused in M1.
    ToolUse,
    Other(String),
}

#[derive(Debug, Clone, PartialEq, Eq, Error, Serialize, Deserialize)]
pub enum BackendError {
    #[error("no API key is set for {backend}")]
    MissingApiKey { backend: BackendId },

    #[error("{backend} rejected the request ({status}): {message}")]
    Http {
        backend: BackendId,
        status: u16,
        message: String,
    },

    #[error("could not reach {backend}: {message}")]
    Transport { backend: BackendId, message: String },

    #[error("{backend} sent a response this build could not parse: {message}")]
    Protocol { backend: BackendId, message: String },

    #[error("{0}")]
    Engine(String),
}

impl BackendError {
    /// Whether retrying could plausibly succeed.
    ///
    /// Master spec s7.1 allows retries only *before* the first streamed byte;
    /// this says whether the failure itself is the retryable kind. Both
    /// conditions must hold before a retry is attempted, because re-sending
    /// after partial output would duplicate text.
    #[must_use]
    pub fn is_retryable(&self) -> bool {
        match self {
            Self::Transport { .. } => true,
            Self::Http { status, .. } => *status == 429 || *status >= 500,
            Self::MissingApiKey { .. } | Self::Protocol { .. } | Self::Engine(_) => false,
        }
    }
}

/// One event in a reply stream.
///
/// Ordering matters: `OpaqueBlock` is emitted once a provider-native block is
/// complete, never per delta, because a thinking signature arrives in its own
/// delta shortly before the block ends and the block is only valid whole
/// (master spec s7.2).
#[derive(Debug, Clone, PartialEq)]
pub enum BackendEvent {
    /// Local engine only: progress reading the prompt.
    ///
    /// `reused_tokens` is how much of the prompt the KV cache already held and
    /// did not have to read again (master spec s8.5). It is carried separately
    /// rather than folded into `done_tokens` because master spec s13 C4 asks
    /// for prefix reuse to be *demonstrated* from these events, and a single
    /// counter cannot tell "reused 2,000, read 50" apart from "read 2,050".
    /// It also lets the UI show an honest estimate: only the tokens actually
    /// being read take time.
    PrefillProgress {
        reused_tokens: u32,
        done_tokens: u32,
        total_tokens: u32,
    },
    TextDelta(String),
    /// Human-readable reasoning. Shown in the UI, never replayed to a model.
    ReasoningDelta(String),
    /// A complete provider-native block, stored verbatim.
    OpaqueBlock(ProviderOpaque),
    /// Defined now, unused in M1 (master spec s3).
    ToolCallDelta {
        call_id: String,
        name: Option<String>,
        args_delta: String,
    },
    Usage {
        input_tokens: u32,
        output_tokens: u32,
    },
    /// The provider's own reassembled response content, emitted once just
    /// before `Done`.
    ///
    /// Master spec s5.2 requires storing this verbatim rather than rebuilding
    /// it from canonical parts, so it has to reach the orchestrator somehow;
    /// carrying it as an event keeps `ChatBackend` to a single output channel.
    RawPayload(serde_json::Value),
    Done {
        stop_reason: StopReason,
    },
    Error(BackendError),
}

/// Where a backend reads per-model settings.
///
/// Master spec s7.1 forbids hardcoding model names: capabilities come from a
/// user-editable profile table, with conservative defaults for ids the user
/// has not filled in yet.
pub trait ModelProfiles: Send + Sync + std::fmt::Debug {
    fn capabilities(&self, model: &ModelRef) -> ModelCapabilities;
    fn thinking(&self, model: &ModelRef) -> ThinkingMode;
}

/// Profiles for a build with no configuration yet: conservative defaults and
/// thinking off. Real deployments back this with the `model_profiles` table.
#[derive(Debug, Clone, Copy, Default)]
pub struct DefaultProfiles;

impl ModelProfiles for DefaultProfiles {
    fn capabilities(&self, _model: &ModelRef) -> ModelCapabilities {
        ModelCapabilities::default()
    }
    fn thinking(&self, _model: &ModelRef) -> ThinkingMode {
        ThinkingMode::Off
    }
}

/// A source of model replies: one of the API adapters, or the local engine.
///
/// `stream` returns events in order. A backend must emit `Done` or `Error`
/// exactly once, last, so callers can finalise a message without a timeout.
#[async_trait::async_trait]
pub trait ChatBackend: Send + Sync + std::fmt::Debug {
    fn id(&self) -> BackendId;

    /// Models this backend can serve right now.
    ///
    /// # Errors
    /// Fails if the provider cannot be reached or rejects the request.
    async fn list_models(&self) -> Result<Vec<ModelInfo>, BackendError>;

    /// What this model accepts. Read from the user-editable model profile,
    /// falling back to conservative defaults for unknown ids.
    fn capabilities(&self, model: &ModelRef) -> ModelCapabilities;

    /// How tokens are counted for this model (master spec s6.1).
    ///
    /// API backends keep the default byte estimate: they expose no tokenizer,
    /// and being a little wrong against a 200k window is absorbed by the
    /// safety margin. The local engine overrides this with an exact counter
    /// backed by its own tokenizer and chat template, where the same error
    /// against an 8k cap would be the difference between fitting and failing.
    fn token_counter(&self, _model: &ModelRef) -> std::sync::Arc<dyn TokenCounter> {
        std::sync::Arc::new(crate::token::ByteEstimateCounter)
    }

    /// Whether this backend sends `BackendRequest::tools` and reports tool calls.
    ///
    /// Off by default so each adapter opts in once it really encodes tools; a
    /// backend that does not gets tool history flattened to text instead.
    fn supports_tools(&self) -> bool {
        false
    }

    /// Begin generating. Cancelling drops the stream; whatever text has
    /// already arrived is the caller's to keep.
    ///
    /// # Errors
    /// Fails if the request cannot be started at all. Failures *during*
    /// streaming arrive as `BackendEvent::Error` instead, so partial output is
    /// not lost.
    async fn stream(
        &self,
        request: BackendRequest,
        cancel: tokio_util::sync::CancellationToken,
    ) -> Result<futures_util::stream::BoxStream<'static, BackendEvent>, BackendError>;
}

#[cfg(test)]
mod tests {
    // Spec s14 permits unwrap/expect in tests.
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;

    fn backend() -> BackendId {
        BackendId::new("anthropic")
    }

    #[test]
    fn rate_limits_and_server_errors_are_retryable() {
        for status in [429, 500, 502, 503] {
            let e = BackendError::Http {
                backend: backend(),
                status,
                message: "busy".to_owned(),
            };
            assert!(e.is_retryable(), "{status} should be retryable");
        }
    }

    #[test]
    fn client_errors_and_missing_keys_are_not_retryable() {
        for status in [400, 401, 403, 404, 422] {
            let e = BackendError::Http {
                backend: backend(),
                status,
                message: "bad request".to_owned(),
            };
            assert!(!e.is_retryable(), "{status} must not be retried");
        }
        assert!(
            !BackendError::MissingApiKey { backend: backend() }.is_retryable(),
            "retrying cannot conjure a key"
        );
    }

    #[test]
    fn transport_failures_are_retryable() {
        assert!(
            BackendError::Transport {
                backend: backend(),
                message: "connection reset".to_owned(),
            }
            .is_retryable()
        );
    }

    #[test]
    fn thinking_defaults_to_off() {
        assert_eq!(ThinkingMode::default(), ThinkingMode::Off);
    }
}
