//! Model identity and declared capabilities (master spec s5.2).

use crate::ids::BackendId;
use serde::{Deserialize, Serialize};
use std::fmt;

/// Which wire protocol produced a piece of provider-native data.
///
/// Distinct from [`BackendId`]: many user-configured backends share the
/// `OpenAICompat` protocol, but each has its own id.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProviderKind {
    Anthropic,
    OpenAi,
    Gemini,
    OpenAiCompat,
    Local,
}

/// A specific model on a specific backend.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ModelRef {
    pub backend: BackendId,
    pub model_id: String,
}

impl ModelRef {
    #[must_use]
    pub fn new(backend: impl Into<BackendId>, model_id: impl Into<String>) -> Self {
        Self {
            backend: backend.into(),
            model_id: model_id.into(),
        }
    }
}

impl fmt::Display for ModelRef {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}/{}", self.backend, self.model_id)
    }
}

/// Whether a model exposes reasoning/thinking content.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReasoningSupport {
    None,
    Available,
}

/// How token counts for this model are obtained.
///
/// `Exact` means a real tokenizer ran over the rendered prompt. `Estimate`
/// means a heuristic was used and the number may be wrong in either
/// direction, so the budget keeps a safety margin (master spec s6.1).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TokenCounting {
    Exact,
    Estimate,
}

/// What a model can accept. Sourced from the user-editable model profile
/// table, never hardcoded per model id (master spec s7.1).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModelCapabilities {
    pub context_window: u32,
    pub max_output_tokens: u32,
    pub vision: bool,
    pub tools: bool,
    pub reasoning: ReasoningSupport,
    pub token_counting: TokenCounting,
    /// Flat token cost charged per image when budgeting (master spec s6.1).
    pub image_token_cost: u32,
}

/// Conservative fallback for a model the user has not profiled: small window,
/// no vision, no tools. The UI pairs this with a warning to set real values.
pub const DEFAULT_CONTEXT_WINDOW: u32 = 32_768;
pub const DEFAULT_MAX_OUTPUT_TOKENS: u32 = 4_096;
pub const DEFAULT_IMAGE_TOKEN_COST: u32 = 1_600;

impl Default for ModelCapabilities {
    fn default() -> Self {
        Self {
            context_window: DEFAULT_CONTEXT_WINDOW,
            max_output_tokens: DEFAULT_MAX_OUTPUT_TOKENS,
            vision: false,
            tools: false,
            reasoning: ReasoningSupport::None,
            token_counting: TokenCounting::Estimate,
            image_token_cost: DEFAULT_IMAGE_TOKEN_COST,
        }
    }
}
