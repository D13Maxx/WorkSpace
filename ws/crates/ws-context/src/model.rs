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

impl ModelCapabilities {
    /// Sensible capabilities for a model with a known context window.
    ///
    /// The output reservation is proportional rather than fixed. A flat 4,096
    /// is 2% of a 200k API window but 50% of the local engine's default 8,192
    /// cap -- which, once the safety margin is taken too, would leave a local
    /// model barely a fifth of its own window for the conversation. Reserving
    /// a quarter of the window, capped at the flat default, keeps both ends of
    /// that range usable.
    #[must_use]
    pub fn for_window(context_window: u32) -> Self {
        Self {
            context_window,
            max_output_tokens: DEFAULT_MAX_OUTPUT_TOKENS.min(context_window / 4).max(256),
            ..Self::default()
        }
    }
}

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

#[cfg(test)]
mod tests {
    // Spec s14 permits unwrap/expect in tests.
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;

    #[test]
    fn a_large_window_keeps_the_flat_output_reservation() {
        let caps = ModelCapabilities::for_window(200_000);
        assert_eq!(caps.max_output_tokens, DEFAULT_MAX_OUTPUT_TOKENS);
    }

    #[test]
    fn a_small_window_is_not_swallowed_by_its_own_output_reservation() {
        // The local engine's default cap. A flat 4,096 would take half of it.
        let caps = ModelCapabilities::for_window(8_192);
        assert_eq!(caps.max_output_tokens, 2_048);
        assert!(
            caps.max_output_tokens * 4 <= caps.context_window,
            "output must never claim more than a quarter of the window"
        );
    }

    #[test]
    fn a_tiny_window_still_reserves_enough_to_answer_with() {
        let caps = ModelCapabilities::for_window(512);
        assert_eq!(caps.max_output_tokens, 256, "a floor, so a reply can exist");
    }
}
