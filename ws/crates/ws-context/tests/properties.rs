//! Property tests for the context builder.
//!
//! Over randomly generated conversations and window sizes, four invariants
//! must hold for every view the builder is willing to return:
//!
//!   1. the view never exceeds the token budget;
//!   2. the recent window is contiguous and ends at the leaf;
//!   3. the latest user message is always present;
//!   4. the stored messages are unchanged by building a view.

// Tests may unwrap and expect: a panic is the failure report.
#![allow(clippy::unwrap_used, clippy::expect_used)]
mod common;

use common::{FixedCounter, PathBuilder, anthropic_model, caps, summary_for};
use proptest::prelude::*;
use ws_context::{
    BuildViewParams, BuiltView, ContextConfig, ContextError, Message, ModelCapabilities, Role,
    TokenCounter, ViewPlan, build_view,
};

/// Reproduces the context builder's budget arithmetic for assertions.
fn available_tokens(c: &ModelCapabilities, cfg: &ContextConfig, system: Option<&str>) -> u32 {
    let margin = cfg.safety_margin(c.context_window);
    let system_tokens = system.map_or(0, |s| FixedCounter.count_text(s));
    c.context_window
        .saturating_sub(c.max_output_tokens)
        .saturating_sub(system_tokens)
        .saturating_sub(margin)
}

/// Drives the orchestrator loop: build, and if a summary is wanted, supply one
/// of exactly the requested size and build again.
fn resolve(
    path: &[Message],
    c: &ModelCapabilities,
    cfg: &ContextConfig,
) -> Result<Option<BuiltView>, ContextError> {
    match build_view(BuildViewParams {
        path,
        system_prompt: None,
        target: &anthropic_model(),
        caps: c,
        summaries: &[],
        counter: &FixedCounter,
        cfg,
        outgoing: None,
    })? {
        ViewPlan::Ready(b) => Ok(Some(*b)),
        ViewPlan::NeedsSummary(req) => {
            let s = summary_for(
                path,
                req.covering.end - 1,
                req.budget_tokens,
                &anthropic_model(),
            );
            match build_view(BuildViewParams {
                path,
                system_prompt: None,
                target: &anthropic_model(),
                caps: c,
                summaries: &[s],
                counter: &FixedCounter,
                cfg,
                outgoing: None,
            })? {
                ViewPlan::Ready(b) => Ok(Some(*b)),
                // A second request would mean the loop does not converge.
                ViewPlan::NeedsSummary(_) => Ok(None),
            }
        }
    }
}

fn conversation_strategy() -> impl Strategy<Value = Vec<Message>> {
    (1usize..25, 1usize..120).prop_map(|(turns, text_len)| {
        let model = anthropic_model();
        let mut b = PathBuilder::new();
        for i in 0..turns {
            b = b.user(&format!("u{i} {}", "a".repeat(text_len)));
            b = b.assistant(&model, &format!("a{i} {}", "b".repeat(text_len)));
        }
        b.user(&format!("final {}", "c".repeat(text_len))).build()
    })
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(256))]

    /// Property 1 and 3.
    #[test]
    fn view_fits_budget_and_keeps_the_latest_user_message(
        path in conversation_strategy(),
        window in 512u32..16_384,
    ) {
        let cfg = ContextConfig::default();
        let c = caps(window);
        let Ok(Some(built)) = resolve(&path, &c, &cfg) else {
            // Erroring or asking again is allowed; producing a bad view is not.
            return Ok(());
        };

        let available = available_tokens(&c, &cfg, None);
        prop_assert!(
            built.view.estimated_input_tokens <= available,
            "view used {} tokens of {available} available",
            built.view.estimated_input_tokens
        );

        let latest_user = path
            .iter()
            .rposition(|m| m.role == Role::User)
            .expect("fixture always ends with a user message");
        prop_assert!(
            built.view.verbatim_messages.contains(&path[latest_user].id),
            "the latest user message was dropped"
        );
    }

    /// Property 2: the sent messages form an unbroken run of the path that
    /// finishes at the leaf.
    #[test]
    fn recent_window_is_contiguous_and_ends_at_the_leaf(
        path in conversation_strategy(),
        window in 512u32..16_384,
    ) {
        let cfg = ContextConfig::default();
        let c = caps(window);
        let Ok(Some(built)) = resolve(&path, &c, &cfg) else {
            return Ok(());
        };

        let sent = &built.view.verbatim_messages;
        prop_assert!(!sent.is_empty());

        let positions: Vec<usize> = sent
            .iter()
            .map(|id| path.iter().position(|m| m.id == *id).expect("id from path"))
            .collect();

        // Strictly increasing, and each step of exactly one: no gaps, no
        // reordering. (Messages emptied by transformation are skipped, which
        // the fixture never produces.)
        for pair in positions.windows(2) {
            prop_assert_eq!(pair[1], pair[0] + 1, "window is not contiguous");
        }
        prop_assert_eq!(
            *positions.last().expect("non-empty"),
            path.len() - 1,
            "window does not end at the leaf"
        );
    }

    /// Property 4: building a view is read-only with respect to history.
    #[test]
    fn building_a_view_does_not_alter_stored_messages(
        path in conversation_strategy(),
        window in 512u32..16_384,
    ) {
        let before = serde_json::to_string(&path).expect("messages serialise");
        let cfg = ContextConfig::default();
        let c = caps(window);
        let _ = resolve(&path, &c, &cfg);
        let after = serde_json::to_string(&path).expect("messages serialise");
        prop_assert_eq!(before, after, "stored messages changed");
    }

    /// The builder must terminate: one summary is always enough.
    #[test]
    fn the_summary_loop_converges_in_one_round(
        path in conversation_strategy(),
        window in 1_024u32..16_384,
    ) {
        let cfg = ContextConfig::default();
        let c = caps(window);
        if let Ok(outcome) = resolve(&path, &c, &cfg) {
            prop_assert!(
                outcome.is_some(),
                "builder asked for a second summary after being given the first"
            );
        }
    }
}
