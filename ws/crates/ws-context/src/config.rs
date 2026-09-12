//! Tunables for the context builder (master spec s6.1, s6.2).

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ContextConfig {
    /// Hard cap on summary size regardless of window (master spec s6.2).
    pub summary_max_tokens: u32,
    /// Fraction of the available budget a summary may occupy, as a percentage.
    pub summary_budget_pct: u32,
    /// Floor for the safety margin; the effective margin is
    /// `max(this, safety_margin_pct% of context_window)`.
    pub safety_margin_floor: u32,
    pub safety_margin_pct: u32,
    /// How far over its budget a returned summary may run before the
    /// orchestrator re-requests it. Spec s6.5 sets no bound; without one a
    /// verbose summariser can crowd out the recent window entirely.
    pub summary_overflow_pct: u32,
}

impl Default for ContextConfig {
    fn default() -> Self {
        Self {
            summary_max_tokens: 2_000,
            summary_budget_pct: 25,
            safety_margin_floor: 256,
            safety_margin_pct: 5,
            summary_overflow_pct: 20,
        }
    }
}

impl ContextConfig {
    /// `max(floor, pct% of context_window)` (master spec s6.1).
    #[must_use]
    pub fn safety_margin(&self, context_window: u32) -> u32 {
        let pct = context_window.saturating_mul(self.safety_margin_pct) / 100;
        self.safety_margin_floor.max(pct)
    }

    /// `min(pct% of available, summary_max_tokens)` (master spec s6.2).
    #[must_use]
    pub fn summary_budget(&self, available: u32) -> u32 {
        let pct = available.saturating_mul(self.summary_budget_pct) / 100;
        pct.min(self.summary_max_tokens)
    }

    /// The largest summary the builder will accept before asking for a redo.
    #[must_use]
    pub fn summary_overflow_limit(&self, budget: u32) -> u32 {
        budget.saturating_add(budget.saturating_mul(self.summary_overflow_pct) / 100)
    }
}
