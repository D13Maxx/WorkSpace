//! The opaque replay rule.
//!
//! Provider-native blocks are replayed only when the target model id matches
//! the producing model id AND the view is Full. Every other case drops them,
//! because a signed reasoning block refers to the exact context it was made in
//! and a summarised prefix is not that context.

// Tests may unwrap and expect: a panic is the failure report.
#![allow(clippy::unwrap_used, clippy::expect_used)]
mod common;

use common::{FixedCounter, PathBuilder, anthropic_model, caps, openai_model, other_model};
use ws_context::{
    BuildViewParams, ContextConfig, OpaqueDropReason, Part, TransformNote, ViewMode, ViewPlan,
    build_view,
};

fn plan_for(path: &[ws_context::Message], target: &ws_context::ModelRef, window: u32) -> ViewPlan {
    let cfg = ContextConfig::default();
    let c = caps(window);
    build_view(BuildViewParams {
        path,
        system_prompt: None,
        target,
        caps: &c,
        summaries: &[],
        counter: &FixedCounter,
        cfg: &cfg,
        outgoing: None,
    })
    .expect("view should build")
}

fn opaque_count(rendered: &ws_context::RenderedView) -> usize {
    rendered
        .messages
        .iter()
        .flat_map(|m| &m.parts)
        .filter(|p| matches!(p, Part::Opaque(_)))
        .count()
}

#[test]
fn same_model_in_full_view_replays_blocks_in_original_order() {
    let model = anthropic_model();
    let path = PathBuilder::new()
        .user("hello")
        .assistant_with_opaque(&model, "hi there", 3)
        .user("go on")
        .build();

    let ViewPlan::Ready(built) = plan_for(&path, &model, 8192) else {
        panic!("expected a ready view");
    };

    assert_eq!(built.view.mode, ViewMode::Full);
    assert!(built.view.opaque_replayed);
    assert_eq!(opaque_count(&built.rendered), 3);

    // Order must be preserved exactly as stored.
    let positions: Vec<u32> = built
        .rendered
        .messages
        .iter()
        .flat_map(|m| &m.parts)
        .filter_map(|p| match p {
            Part::Opaque(o) => Some(o.position),
            _ => None,
        })
        .collect();
    assert_eq!(positions, vec![0, 1, 2]);
}

#[test]
fn different_model_on_the_same_provider_drops_blocks() {
    let author = anthropic_model();
    let path = PathBuilder::new()
        .user("hello")
        .assistant_with_opaque(&author, "hi there", 2)
        .user("go on")
        .build();

    // other_model() is the same backend, a different model id.
    let ViewPlan::Ready(built) = plan_for(&path, &other_model(), 8192) else {
        panic!("expected a ready view");
    };

    assert_eq!(built.view.mode, ViewMode::Full);
    assert!(!built.view.opaque_replayed);
    assert_eq!(opaque_count(&built.rendered), 0);
    assert!(built.view.transformations.iter().any(|n| matches!(
        n,
        TransformNote::OpaqueDropped {
            reason: OpaqueDropReason::DifferentModel,
            count: 2,
            ..
        }
    )));
}

#[test]
fn different_provider_drops_blocks() {
    let author = anthropic_model();
    let path = PathBuilder::new()
        .user("hello")
        .assistant_with_opaque(&author, "hi there", 2)
        .user("go on")
        .build();

    let ViewPlan::Ready(built) = plan_for(&path, &openai_model(), 8192) else {
        panic!("expected a ready view");
    };

    assert!(!built.view.opaque_replayed);
    assert_eq!(opaque_count(&built.rendered), 0);
}

#[test]
fn summarised_view_drops_blocks_even_for_the_same_model() {
    let model = anthropic_model();
    let mut b = PathBuilder::new();
    // Enough history to overflow a small window and force eviction.
    for i in 0..40 {
        b = b.user(&format!("question number {i} with some padding text here"));
        b = b.assistant_with_opaque(&model, &format!("answer number {i} padded out"), 1);
    }
    let path = b.user("final question").build();

    let plan = plan_for(&path, &model, 1024);
    let built = match plan {
        ViewPlan::Ready(b) => b,
        ViewPlan::NeedsSummary(_) => {
            // Without a cached summary the builder asks for one first. Supply
            // it and rebuild, which is the orchestrator's loop.
            let cfg = ContextConfig::default();
            let c = caps(1024);
            let ViewPlan::NeedsSummary(req) = build_view(BuildViewParams {
                path: &path,
                system_prompt: None,
                target: &model,
                caps: &c,
                summaries: &[],
                counter: &FixedCounter,
                cfg: &cfg,
                outgoing: None,
            })
            .unwrap() else {
                panic!("expected a summary request");
            };
            let s = common::summary_for(&path, req.covering.end - 1, 50, &model);
            let ViewPlan::Ready(b) = build_view(BuildViewParams {
                path: &path,
                system_prompt: None,
                target: &model,
                caps: &c,
                summaries: &[s],
                counter: &FixedCounter,
                cfg: &cfg,
                outgoing: None,
            })
            .unwrap() else {
                panic!("expected a ready view after supplying the summary");
            };
            b
        }
    };

    assert_eq!(built.view.mode, ViewMode::SummaryPlusRecent);
    assert!(!built.view.opaque_replayed);
    assert_eq!(opaque_count(&built.rendered), 0);
    assert!(built.view.transformations.iter().any(|n| matches!(
        n,
        TransformNote::OpaqueDropped {
            reason: OpaqueDropReason::NotFullView,
            ..
        }
    )));
}

#[test]
fn visible_reasoning_is_never_sent_to_any_model() {
    let model = anthropic_model();
    let path = PathBuilder::new()
        .user("hello")
        .user_with_parts(vec![
            Part::Reasoning {
                visible_text: Some("internal deliberation".to_owned()),
            },
            Part::text("visible question"),
        ])
        .build();

    let ViewPlan::Ready(built) = plan_for(&path, &model, 8192) else {
        panic!("expected a ready view");
    };

    let has_reasoning = built
        .rendered
        .messages
        .iter()
        .flat_map(|m| &m.parts)
        .any(|p| matches!(p, Part::Reasoning { .. }));
    assert!(!has_reasoning, "reasoning must never reach a model");
    assert!(
        built
            .rendered
            .messages
            .iter()
            .flat_map(|m| &m.parts)
            .any(|p| p.as_text() == Some("visible question"))
    );
}
