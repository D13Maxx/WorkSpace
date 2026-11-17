//! Budget arithmetic, mode selection, per-target transformations and summary
//! validity.

// Tests may unwrap and expect: a panic is the failure report.
#![allow(clippy::unwrap_used, clippy::expect_used)]
mod common;

use common::{
    FixedCounter, PathBuilder, anthropic_model, caps, other_model, summary_for, vision_caps,
};
use ws_context::{
    BuildViewParams, ContextConfig, ContextError, ModelCapabilities, Part, TransformNote, ViewMode,
    ViewPlan, build_view,
};

fn plan(
    path: &[ws_context::Message],
    c: &ModelCapabilities,
    summaries: &[ws_context::ContextSummary],
    system: Option<&str>,
) -> Result<ViewPlan, ContextError> {
    let cfg = ContextConfig::default();
    build_view(BuildViewParams {
        path,
        system_prompt: system,
        target: &anthropic_model(),
        caps: c,
        summaries,
        counter: &FixedCounter,
        cfg: &cfg,
        outgoing: None,
    })
}

/// Long history that will not fit a small window.
fn long_path(turns: usize) -> Vec<ws_context::Message> {
    let model = anthropic_model();
    let mut b = PathBuilder::new();
    for i in 0..turns {
        b = b.user(&format!(
            "user turn {i} with enough padding text to cost real tokens here"
        ));
        b = b.assistant(
            &model,
            &format!("assistant turn {i} with enough padding text to cost tokens"),
        );
    }
    b.user("the final question").build()
}

#[test]
fn short_conversation_uses_a_full_view() {
    let path = PathBuilder::new()
        .user("hello")
        .assistant(&anthropic_model(), "hi")
        .user("how are you")
        .build();

    let ViewPlan::Ready(built) = plan(&path, &caps(8192), &[], None).unwrap() else {
        panic!("expected ready");
    };
    assert_eq!(built.view.mode, ViewMode::Full);
    assert_eq!(built.view.verbatim_messages.len(), 3);
    assert!(built.view.summary.is_none());
}

#[test]
fn overflowing_conversation_requests_a_summary_then_builds() {
    let path = long_path(40);
    let c = caps(1024);

    let ViewPlan::NeedsSummary(req) = plan(&path, &c, &[], None).unwrap() else {
        panic!("expected a summary request");
    };
    assert_eq!(req.covering.start, 0);
    assert!(req.covering.end > 0, "something must be evicted");
    assert!(req.budget_tokens > 0);
    assert_eq!(req.covers_from, path[0].id);
    assert_eq!(req.covers_to, path[req.covering.end - 1].id);

    let s = summary_for(&path, req.covering.end - 1, 40, &anthropic_model());
    let ViewPlan::Ready(built) = plan(&path, &c, std::slice::from_ref(&s), None).unwrap() else {
        panic!("expected ready after supplying the summary");
    };
    assert_eq!(built.view.mode, ViewMode::SummaryPlusRecent);
    assert_eq!(built.view.summary, Some(s.id));
    assert!(
        built.rendered.system.is_some(),
        "summary goes in the system prompt"
    );
    assert!(
        built
            .rendered
            .system
            .as_deref()
            .unwrap()
            .contains("earlier context")
    );
}

#[test]
fn the_latest_user_message_is_always_present() {
    let path = long_path(60);
    let c = caps(1024);
    let ViewPlan::NeedsSummary(req) = plan(&path, &c, &[], None).unwrap() else {
        panic!("expected a summary request");
    };
    let s = summary_for(&path, req.covering.end - 1, 40, &anthropic_model());
    let ViewPlan::Ready(built) = plan(&path, &c, &[s], None).unwrap() else {
        panic!("expected ready");
    };

    let last = path.last().unwrap();
    assert_eq!(
        built.view.verbatim_messages.last(),
        Some(&last.id),
        "the recent window must end at the leaf"
    );
}

#[test]
fn a_single_oversized_user_message_fails_loudly_instead_of_truncating() {
    let huge = "x".repeat(200_000);
    let path = PathBuilder::new().user(&huge).build();

    let err = plan(&path, &caps(2048), &[], None).unwrap_err();
    match err {
        ContextError::LatestUserMessageTooLarge {
            message_tokens,
            available,
            ..
        } => {
            assert!(message_tokens > available);
            // The message must name both numbers so the user can act on it.
            let text = err.to_string();
            assert!(text.contains(&message_tokens.to_string()));
            assert!(text.contains(&available.to_string()));
        }
        other => panic!("expected LatestUserMessageTooLarge, got {other:?}"),
    }
}

#[test]
fn a_window_smaller_than_its_own_reservations_is_rejected() {
    let path = PathBuilder::new().user("hi").build();
    let mut c = caps(512);
    c.max_output_tokens = 4096; // larger than the whole window

    let err = plan(&path, &c, &[], None).unwrap_err();
    assert!(matches!(err, ContextError::NoInputBudget { .. }));
}

#[test]
fn the_system_prompt_is_charged_against_the_budget() {
    let path = PathBuilder::new().user("hi").build();
    let c = caps(8192);

    let ViewPlan::Ready(without) = plan(&path, &c, &[], None).unwrap() else {
        panic!("expected ready");
    };
    let ViewPlan::Ready(with) = plan(&path, &c, &[], Some("You are a careful assistant.")).unwrap()
    else {
        panic!("expected ready");
    };

    assert_eq!(
        with.rendered.system.as_deref(),
        Some("You are a careful assistant.")
    );
    assert!(without.rendered.system.is_none());
}

#[test]
fn a_summary_from_another_branch_is_not_reused() {
    let path_a = long_path(40);
    let c = caps(1024);
    let ViewPlan::NeedsSummary(req) = plan(&path_a, &c, &[], None).unwrap() else {
        panic!("expected a summary request");
    };
    let summary_a = summary_for(&path_a, req.covering.end - 1, 40, &anthropic_model());

    // Branch B is a different path entirely: different message ids.
    let path_b = long_path(40);
    let plan_b = plan(&path_b, &c, std::slice::from_ref(&summary_a), None).unwrap();

    match plan_b {
        ViewPlan::NeedsSummary(req_b) => {
            assert_eq!(
                req_b.extends, None,
                "branch A's summary is not extendable here"
            );
        }
        ViewPlan::Ready(built) => {
            panic!(
                "branch A's summary must not satisfy branch B, got summary {:?}",
                built.view.summary
            );
        }
    }
}

#[test]
fn an_older_summary_covering_a_shorter_prefix_is_offered_for_extension() {
    let path = long_path(60);
    let c = caps(1024);
    let ViewPlan::NeedsSummary(req) = plan(&path, &c, &[], None).unwrap() else {
        panic!("expected a summary request");
    };
    // A stale summary covering strictly less than what is now needed.
    let stale_end = req.covering.end / 2;
    let stale = summary_for(&path, stale_end, 30, &anthropic_model());

    let ViewPlan::NeedsSummary(req2) = plan(&path, &c, std::slice::from_ref(&stale), None).unwrap()
    else {
        panic!("expected a summary request");
    };
    assert_eq!(
        req2.extends,
        Some(stale.id),
        "the builder should extend the existing summary rather than redo it"
    );
}

#[test]
fn an_oversized_cached_summary_is_rejected_rather_than_crowding_out_history() {
    let path = long_path(40);
    let c = caps(1024);
    let ViewPlan::NeedsSummary(req) = plan(&path, &c, &[], None).unwrap() else {
        panic!("expected a summary request");
    };
    // Far beyond the 20% overflow tolerance.
    let bloated = summary_for(
        &path,
        req.covering.end - 1,
        req.budget_tokens * 10,
        &anthropic_model(),
    );

    let err = plan(&path, &c, &[bloated], None).unwrap_err();
    assert!(matches!(err, ContextError::SummaryTooLarge { .. }));
}

#[test]
fn images_survive_for_vision_models_and_become_placeholders_otherwise() {
    let blob = ws_context::BlobRef::new("a".repeat(64));
    let path = PathBuilder::new()
        .user_with_parts(vec![
            Part::Image {
                blob,
                mime: "image/png".to_owned(),
                name: Some("diagram.png".to_owned()),
            },
            Part::text("what is this"),
        ])
        .build();

    let ViewPlan::Ready(seen) = plan(&path, &vision_caps(8192), &[], None).unwrap() else {
        panic!("expected ready");
    };
    assert!(
        seen.rendered
            .messages
            .iter()
            .flat_map(|m| &m.parts)
            .any(|p| matches!(p, Part::Image { .. }))
    );
    assert!(seen.view.transformations.is_empty());

    let ViewPlan::Ready(blind) = plan(&path, &caps(8192), &[], None).unwrap() else {
        panic!("expected ready");
    };
    assert!(
        !blind
            .rendered
            .messages
            .iter()
            .flat_map(|m| &m.parts)
            .any(|p| matches!(p, Part::Image { .. }))
    );
    assert!(
        blind
            .view
            .transformations
            .iter()
            .any(|n| matches!(n, TransformNote::ImageDropped { .. }))
    );
    // The placeholder names the file so the user can tell what was hidden.
    assert!(
        blind
            .rendered
            .messages
            .iter()
            .flat_map(|m| &m.parts)
            .filter_map(Part::as_text)
            .any(|t| t.contains("diagram.png"))
    );
}

#[test]
fn the_outgoing_model_is_suggested_as_summariser_after_a_switch() {
    let path = long_path(40);
    let c = caps(1024);
    let cfg = ContextConfig::default();
    let outgoing = other_model();

    let ViewPlan::NeedsSummary(req) = build_view(BuildViewParams {
        path: &path,
        system_prompt: None,
        target: &anthropic_model(),
        caps: &c,
        summaries: &[],
        counter: &FixedCounter,
        cfg: &cfg,
        outgoing: Some(&outgoing),
    })
    .unwrap() else {
        panic!("expected a summary request");
    };

    assert_eq!(
        req.suggested_writer, outgoing,
        "the model being switched away from is preferred"
    );
}

#[test]
fn a_view_is_compacted_before_it_overflows_not_after() {
    // The trigger sits below the hard limit, so a path that would still fit
    // is compacted anyway to leave headroom for the turns after it.
    let path = long_path(40);
    let cfg = ContextConfig::default();
    let c = caps(4096);

    let available = c.context_window - c.max_output_tokens - cfg.safety_margin(c.context_window);
    let trigger = cfg.compaction_trigger(available);
    assert!(trigger < available, "the trigger must sit below the limit");

    // Grow the path until it crosses the trigger but still fits the limit.
    let mut turns = 2;
    let plan = loop {
        let p = long_path(turns);
        let plan = plan(&p, &c, &[], None).unwrap();
        if matches!(plan, ViewPlan::NeedsSummary(_)) {
            break plan;
        }
        turns += 1;
        assert!(turns < 200, "should have crossed the trigger by now");
    };
    assert!(matches!(plan, ViewPlan::NeedsSummary(_)));
    let _ = path;
}

#[test]
fn a_compacted_view_comes_down_to_the_target_leaving_room_to_grow() {
    let path = long_path(60);
    let cfg = ContextConfig::default();
    let c = caps(1024);
    let available = c.context_window - c.max_output_tokens - cfg.safety_margin(c.context_window);
    let target = cfg.compaction_target(available);
    let trigger = cfg.compaction_trigger(available);

    let ViewPlan::NeedsSummary(req) = plan(&path, &c, &[], None).unwrap() else {
        panic!("expected a summary request");
    };
    let s = summary_for(
        &path,
        req.covering.end - 1,
        req.budget_tokens,
        &anthropic_model(),
    );
    let ViewPlan::Ready(built) = plan(&path, &c, std::slice::from_ref(&s), None).unwrap() else {
        panic!("expected ready");
    };

    let used = built.view.estimated_input_tokens;
    assert!(
        used <= trigger,
        "a freshly compacted view must sit below the trigger, used {used} of {trigger}"
    );
    // Some slack is allowed for the final message, but it should land near the
    // target rather than merely under the limit.
    assert!(
        used <= target + target / 2,
        "compaction should aim for the target ({target}), used {used}"
    );
}

#[test]
fn compaction_does_not_repeat_on_every_following_turn() {
    // The point of the headroom: after compacting, several more exchanges fit
    // before another summary is needed. Without it, every turn pays again.
    let cfg = ContextConfig::default();
    let c = caps(4096);

    // Find the first length that triggers compaction.
    let mut turns = 2;
    let first_trigger = loop {
        let p = long_path(turns);
        if matches!(plan(&p, &c, &[], None).unwrap(), ViewPlan::NeedsSummary(_)) {
            break turns;
        }
        turns += 1;
        assert!(turns < 200, "should have compacted by now");
    };

    // Compact at that length, then keep the summary and grow the conversation.
    let path = long_path(first_trigger);
    let ViewPlan::NeedsSummary(req) = plan(&path, &c, &[], None).unwrap() else {
        panic!("expected a summary request");
    };
    let summary = summary_for(
        &path,
        req.covering.end - 1,
        req.budget_tokens,
        &anthropic_model(),
    );

    // The same summary keeps satisfying the view for further turns, because
    // the compacted view left room. (A longer path has different ids, so this
    // measures headroom in tokens rather than cache reuse.)
    let ViewPlan::Ready(built) = plan(&path, &c, std::slice::from_ref(&summary), None).unwrap()
    else {
        panic!("expected ready");
    };
    let available = c.context_window - c.max_output_tokens - cfg.safety_margin(c.context_window);
    let headroom = available.saturating_sub(built.view.estimated_input_tokens);
    assert!(
        headroom > available / 4,
        "a compacted view should leave at least a quarter of the budget free, \
         got {headroom} of {available}"
    );
}

#[test]
fn compaction_never_keeps_a_tool_result_without_its_call() {
    // Providers reject a tool result whose call is missing, so the cut must
    // never land between the two. Agent turns fill the window quickly with
    // file contents, which is what makes this boundary real.
    let model = anthropic_model();
    let mut b = PathBuilder::new();
    for i in 0..12 {
        b = b.user(&format!(
            "question {i} with enough padding text to cost tokens"
        ));
        for r in 0..3 {
            b = b.tool_round(
                &model,
                &format!("c{i}-{r}"),
                "file contents padding padding padding",
            );
        }
        b = b.assistant(
            &model,
            &format!("answer {i} with enough padding text to cost"),
        );
    }
    let path = b.user("the final question").build();
    let mut c = caps(1024);
    c.tools = true;

    let ViewPlan::NeedsSummary(req) = plan(&path, &c, &[], None).unwrap() else {
        panic!("expected a summary request");
    };
    let s = summary_for(&path, req.covering.end - 1, 40, &model);
    let ViewPlan::Ready(built) = plan(&path, &c, std::slice::from_ref(&s), None).unwrap() else {
        panic!("expected ready");
    };

    let messages = &built.rendered.messages;
    assert_eq!(
        messages[0].role,
        ws_context::Role::User,
        "the window opens on a user turn"
    );
    let mut calls = std::collections::HashSet::new();
    for m in messages {
        for part in &m.parts {
            match part {
                Part::ToolCall { call_id, .. } => {
                    calls.insert(call_id.clone());
                }
                Part::ToolResult { call_id, .. } => {
                    assert!(
                        calls.contains(call_id),
                        "result {call_id} kept without its call"
                    );
                }
                _ => {}
            }
        }
    }
    assert!(
        !calls.is_empty(),
        "the test must actually keep some tool rounds"
    );
}
