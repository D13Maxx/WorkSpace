//! Per-model settings, held in memory and backed by the `model_profiles`
//! table (master spec s7.1).
//!
//! Kept in memory rather than read from SQLite on each lookup because
//! `capabilities` is called during context building, which is synchronous and
//! on the path of every turn.

use std::collections::HashMap;
use std::sync::RwLock;
use ws_context::{ModelCapabilities, ModelProfiles, ModelRef, ThinkingMode};

#[derive(Debug, Clone, PartialEq)]
pub struct Profile {
    pub capabilities: ModelCapabilities,
    pub thinking: ThinkingMode,
    /// False while the values are still the conservative defaults, which the
    /// UI pairs with a warning to set the real ones.
    pub user_edited: bool,
}

impl Default for Profile {
    fn default() -> Self {
        Self {
            capabilities: ModelCapabilities::default(),
            thinking: ThinkingMode::Off,
            user_edited: false,
        }
    }
}

/// Thread-safe profile table.
#[derive(Debug, Default)]
pub struct ProfileTable {
    entries: RwLock<HashMap<ModelRef, Profile>>,
}

impl ProfileTable {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    pub fn set(&self, model: ModelRef, profile: Profile) {
        if let Ok(mut w) = self.entries.write() {
            w.insert(model, profile);
        }
    }

    #[must_use]
    pub fn get(&self, model: &ModelRef) -> Profile {
        self.entries
            .read()
            .ok()
            .and_then(|r| r.get(model).cloned())
            .unwrap_or_default()
    }

    /// True when this model still has only conservative defaults.
    #[must_use]
    pub fn is_unprofiled(&self, model: &ModelRef) -> bool {
        !self.get(model).user_edited
    }
}

impl ModelProfiles for ProfileTable {
    fn capabilities(&self, model: &ModelRef) -> ModelCapabilities {
        self.get(model).capabilities
    }

    fn thinking(&self, model: &ModelRef) -> ThinkingMode {
        self.get(model).thinking
    }
}

#[cfg(test)]
mod tests {
    // Spec s14 permits unwrap/expect in tests.
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;

    #[test]
    fn an_unknown_model_gets_conservative_defaults_and_is_flagged() {
        let table = ProfileTable::new();
        let model = ModelRef::new("anthropic", "never-seen");

        let caps = table.capabilities(&model);
        assert_eq!(caps.context_window, ws_context::DEFAULT_CONTEXT_WINDOW);
        assert!(!caps.vision, "assume no vision until told otherwise");
        assert_eq!(table.thinking(&model), ThinkingMode::Off);
        assert!(
            table.is_unprofiled(&model),
            "the UI must be able to warn about guessed values"
        );
    }

    #[test]
    fn an_edited_profile_is_returned_and_no_longer_flagged() {
        let table = ProfileTable::new();
        let model = ModelRef::new("anthropic", "known");
        table.set(
            model.clone(),
            Profile {
                capabilities: ModelCapabilities {
                    context_window: 200_000,
                    vision: true,
                    ..ModelCapabilities::default()
                },
                thinking: ThinkingMode::Adaptive,
                user_edited: true,
            },
        );

        assert_eq!(table.capabilities(&model).context_window, 200_000);
        assert!(table.capabilities(&model).vision);
        assert_eq!(table.thinking(&model), ThinkingMode::Adaptive);
        assert!(!table.is_unprofiled(&model));
    }
}
