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

    /// Compact once the view would exceed this percentage of the available
    /// budget, rather than waiting until it no longer fits.
    ///
    /// Spec s6.2 compacts only on overflow, which means compacting on *every*
    /// turn once a conversation is long: each new exchange pushes it back over
    /// the line, so a fresh summary is written every time. Triggering early
    /// and compacting down to `compaction_target_pct` leaves headroom for
    /// several turns before the next one is needed.
    pub compaction_trigger_pct: u32,

    /// What the compacted view should come down to, as a percentage of the
    /// available budget. The gap between this and the trigger is the headroom.
    pub compaction_target_pct: u32,
}

impl Default for ContextConfig {
    fn default() -> Self {
        Self {
            summary_max_tokens: 2_000,
            summary_budget_pct: 25,
            safety_margin_floor: 256,
            safety_margin_pct: 5,
            summary_overflow_pct: 20,
            compaction_trigger_pct: 85,
            compaction_target_pct: 50,
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

    /// The size at which a view stops being sent whole and gets compacted.
    ///
    /// Never above `available`: a misconfigured trigger over 100% must not
    /// let a view exceed what the model can accept.
    #[must_use]
    pub fn compaction_trigger(&self, available: u32) -> u32 {
        let pct = available.saturating_mul(self.compaction_trigger_pct.min(100)) / 100;
        pct.min(available)
    }

    /// The size a compacted view aims for.
    ///
    /// Clamped below the trigger, because a target at or above the trigger
    /// would leave no headroom and compact again on the very next turn.
    #[must_use]
    pub fn compaction_target(&self, available: u32) -> u32 {
        let trigger = self.compaction_trigger(available);
        let pct = available.saturating_mul(self.compaction_target_pct.min(100)) / 100;
        pct.min(trigger)
    }
}

#[cfg(test)]
mod tests {
    // Spec s14 permits unwrap/expect in tests.
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;

    #[test]
    fn compaction_leaves_headroom_between_the_trigger_and_the_target() {
        let cfg = ContextConfig::default();
        let available = 100_000;
        let trigger = cfg.compaction_trigger(available);
        let target = cfg.compaction_target(available);

        assert_eq!(trigger, 85_000);
        assert_eq!(target, 50_000);
        assert!(
            target < trigger,
            "without headroom the next turn would compact again immediately"
        );
    }

    #[test]
    fn a_trigger_over_one_hundred_percent_cannot_exceed_what_fits() {
        let cfg = ContextConfig {
            compaction_trigger_pct: 150,
            ..ContextConfig::default()
        };
        assert_eq!(cfg.compaction_trigger(10_000), 10_000);
    }

    #[test]
    fn a_target_above_the_trigger_is_pulled_back_below_it() {
        let cfg = ContextConfig {
            compaction_trigger_pct: 60,
            compaction_target_pct: 90,
            ..ContextConfig::default()
        };
        let available = 10_000;
        assert_eq!(
            cfg.compaction_target(available),
            cfg.compaction_trigger(available),
            "a target above the trigger must not be honoured as written"
        );
    }
}
