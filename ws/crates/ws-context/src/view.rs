//! The context builder (master spec s6).
//!
//! Pure: no I/O, no clocks, no randomness. Given a path and a target model it
//! returns either a finished view or a request for a summary. The stored
//! conversation is never modified -- only the view sent to a model changes.
//!
//! Two details the spec leaves open, decided here and recorded in
//! `docs/adr/001-context-view-shape.md`:
//!
//! 1. A summary is injected by appending it to the system prompt rather than
//!    as a synthetic message, so it cannot disturb user/assistant alternation
//!    on providers that require it.
//! 2. The recent window is sized against the *maximum* summary budget, not the
//!    actual summary length. Sizing against the actual length is circular --
//!    the summary covers whatever the window excludes -- and would make the
//!    window depend on summariser verbosity.

use crate::config::ContextConfig;
use crate::error::ContextError;
use crate::ids::{MsgId, SummaryId};
use crate::message::{Message, Part, ProviderOpaque, Role};
use crate::model::{ModelCapabilities, ModelRef};
use crate::summary::ContextSummary;
use crate::token::TokenCounter;
use serde::{Deserialize, Serialize};
use std::ops::Range;

/// Per-message wire overhead (role marker, delimiters). Approximate on
/// purpose; the safety margin covers the difference.
const MESSAGE_OVERHEAD_TOKENS: u32 = 4;

/// Heading placed above an injected summary in the system prompt.
pub const SUMMARY_HEADER: &str = "## Summary of earlier conversation\n\
     The following summarises earlier messages that are not shown verbatim below.";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ViewMode {
    /// Whole path sent verbatim. Only in this mode may provider-native
    /// blocks be replayed (master spec s6.4).
    Full,
    SummaryPlusRecent,
}

/// A content change made to fit a target model. Surfaced in the inspector so
/// the user can see exactly why a reply was based on less than the full text.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum TransformNote {
    ImageDropped {
        message: MsgId,
        name: Option<String>,
    },
    FileDropped {
        message: MsgId,
        name: String,
    },
    ReasoningWithheld {
        message: MsgId,
    },
    OpaqueDropped {
        message: MsgId,
        count: u32,
        reason: OpaqueDropReason,
    },
    ToolsFlattened {
        message: MsgId,
    },
    LeadingAssistantTrimmed {
        message: MsgId,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OpaqueDropReason {
    /// A different model than the one that produced the block.
    DifferentModel,
    /// Summarised views do not reproduce the context the block was signed in.
    NotFullView,
}

/// The persistable description of what a model was sent. Stored per assistant
/// message so the inspector can explain any past reply (master spec s6.6).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ContextView {
    pub target: ModelRef,
    pub mode: ViewMode,
    pub summary: Option<SummaryId>,
    pub verbatim_messages: Vec<MsgId>,
    pub transformations: Vec<TransformNote>,
    pub opaque_replayed: bool,
    pub estimated_input_tokens: u32,
}

/// One message as it will actually go on the wire.
#[derive(Debug, Clone, PartialEq)]
pub struct RenderedMessage {
    pub source_id: MsgId,
    pub role: Role,
    pub parts: Vec<Part>,
}

/// Everything a provider adapter needs to build a request body.
#[derive(Debug, Clone, PartialEq)]
pub struct RenderedView {
    /// System prompt with any summary already appended.
    pub system: Option<String>,
    pub messages: Vec<RenderedMessage>,
}

/// The finished product: what to send, plus what to persist about it.
#[derive(Debug, Clone, PartialEq)]
pub struct BuiltView {
    pub view: ContextView,
    pub rendered: RenderedView,
}

/// A summary the orchestrator must produce before the view can be built.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SummaryRequest {
    /// Indices into the path that the summary must cover.
    pub covering: Range<usize>,
    pub covers_from: MsgId,
    pub covers_to: MsgId,
    /// Preferred author: the outgoing model if one was supplied, else the
    /// target (master spec s6.5).
    pub suggested_writer: ModelRef,
    pub budget_tokens: u32,
    /// An existing summary covering a shorter prefix, to extend incrementally.
    pub extends: Option<SummaryId>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum ViewPlan {
    Ready(Box<BuiltView>),
    NeedsSummary(SummaryRequest),
}

/// Inputs to [`build_view`]. Grouped into a struct because the argument list
/// is long and mostly borrowed.
#[derive(Debug, Clone, Copy)]
pub struct BuildViewParams<'a> {
    /// Root -> active leaf, in order.
    pub path: &'a [Message],
    pub system_prompt: Option<&'a str>,
    pub target: &'a ModelRef,
    pub caps: &'a ModelCapabilities,
    pub summaries: &'a [ContextSummary],
    pub counter: &'a dyn TokenCounter,
    pub cfg: &'a ContextConfig,
    /// The model being switched away from, if this turn follows a switch.
    /// Spec s6.5 prefers it as summariser because it still holds the context.
    pub outgoing: Option<&'a ModelRef>,
}

/// Em dash used in placeholder text, escaped so the source stays ASCII.
const EM_DASH: char = '\u{2014}';

fn image_placeholder(name: Option<&str>) -> String {
    match name {
        Some(n) => format!("[Image {n:?} {EM_DASH} not visible to this model]"),
        None => format!("[Image {EM_DASH} not visible to this model]"),
    }
}

fn file_placeholder(name: &str) -> String {
    format!("[File {name:?} {EM_DASH} not readable by this model]")
}

/// Apply the per-target transformations of master spec s6.3 to one message.
///
/// `allow_opaque` encodes the s6.4 view-mode condition; the model-identity
/// condition is checked here.
fn transform(
    msg: &Message,
    target: &ModelRef,
    caps: &ModelCapabilities,
    allow_opaque: bool,
) -> (Vec<Part>, Vec<TransformNote>, u32) {
    let mut parts = Vec::with_capacity(msg.parts.len());
    let mut notes = Vec::new();
    let mut replayed = 0u32;

    let same_model = msg.author_model.as_ref() == Some(target);
    let mut dropped_opaque = 0u32;

    for part in &msg.parts {
        match part {
            Part::Text { .. } => parts.push(part.clone()),

            Part::Image { name, .. } if caps.vision => {
                let _ = name;
                parts.push(part.clone());
            }
            Part::Image { name, .. } => {
                notes.push(TransformNote::ImageDropped {
                    message: msg.id,
                    name: name.clone(),
                });
                parts.push(Part::text(image_placeholder(name.as_deref())));
            }

            Part::File { name, .. } => {
                notes.push(TransformNote::FileDropped {
                    message: msg.id,
                    name: name.clone(),
                });
                parts.push(Part::text(file_placeholder(name)));
            }

            // Never sent to any model, by any target (master spec s6.3).
            Part::Reasoning { visible_text } => {
                if visible_text.is_some() {
                    notes.push(TransformNote::ReasoningWithheld { message: msg.id });
                }
            }

            Part::ToolCall { .. } | Part::ToolResult { .. } => {
                if caps.tools {
                    parts.push(part.clone());
                } else {
                    notes.push(TransformNote::ToolsFlattened { message: msg.id });
                    parts.push(Part::text(flatten_tool_part(part)));
                }
            }

            Part::Opaque(o) => {
                if allow_opaque && same_model && o.model_id == target.model_id {
                    parts.push(part.clone());
                    replayed += 1;
                } else {
                    dropped_opaque += 1;
                }
            }
        }
    }

    if dropped_opaque > 0 {
        let reason = if allow_opaque {
            OpaqueDropReason::DifferentModel
        } else {
            OpaqueDropReason::NotFullView
        };
        notes.push(TransformNote::OpaqueDropped {
            message: msg.id,
            count: dropped_opaque,
            reason,
        });
    }

    (parts, notes, replayed)
}

/// Render a tool part as plain text for a target without tool support.
/// Unused in M1 (no tool calling) but kept so the format needs no migration.
fn flatten_tool_part(part: &Part) -> String {
    match part {
        Part::ToolCall {
            name, args_json, ..
        } => {
            format!("[Tool call: {name}({args_json})]")
        }
        Part::ToolResult {
            content, is_error, ..
        } => {
            let body: String = content.iter().filter_map(Part::as_text).collect();
            if *is_error {
                format!("[Tool error: {body}]")
            } else {
                format!("[Tool result: {body}]")
            }
        }
        _ => String::new(),
    }
}

fn part_cost(part: &Part, caps: &ModelCapabilities, counter: &dyn TokenCounter) -> u32 {
    match part {
        Part::Text { text } => counter.count_text(text),
        Part::Image { .. } => caps.image_token_cost,
        Part::File { name, .. } => counter.count_text(name),
        Part::Reasoning { .. } => 0,
        Part::ToolCall {
            name, args_json, ..
        } => counter.count_text(name) + counter.count_text(args_json),
        Part::ToolResult { content, .. } => {
            content.iter().map(|p| part_cost(p, caps, counter)).sum()
        }
        // Opaque payloads go on the wire as JSON, so they are charged as JSON.
        Part::Opaque(o) => opaque_cost(o, counter),
    }
}

fn opaque_cost(o: &ProviderOpaque, counter: &dyn TokenCounter) -> u32 {
    match serde_json::to_string(&o.payload) {
        Ok(s) => counter.count_text(&s),
        // A payload that will not serialise cannot be replayed either; charging
        // zero here is consistent with it being dropped at send time.
        Err(_) => 0,
    }
}

fn parts_cost(parts: &[Part], caps: &ModelCapabilities, counter: &dyn TokenCounter) -> u32 {
    parts
        .iter()
        .map(|p| part_cost(p, caps, counter))
        .fold(0u32, u32::saturating_add)
        .saturating_add(MESSAGE_OVERHEAD_TOKENS)
}

/// One message after transformation, with its cost.
struct Prepared {
    source_id: MsgId,
    role: Role,
    parts: Vec<Part>,
    notes: Vec<TransformNote>,
    replayed: u32,
    cost: u32,
}

fn prepare(p: &BuildViewParams<'_>, allow_opaque: bool) -> Vec<Prepared> {
    p.path
        .iter()
        .map(|m| {
            let (parts, notes, replayed) = transform(m, p.target, p.caps, allow_opaque);
            let cost = parts_cost(&parts, p.caps, p.counter);
            Prepared {
                source_id: m.id,
                role: m.role,
                parts,
                notes,
                replayed,
                cost,
            }
        })
        .collect()
}

fn assemble(
    p: &BuildViewParams<'_>,
    prepared: &[Prepared],
    range: Range<usize>,
    mode: ViewMode,
    summary: Option<&ContextSummary>,
    extra_notes: Vec<TransformNote>,
) -> BuiltView {
    let mut messages = Vec::new();
    let mut verbatim = Vec::new();
    let mut notes = extra_notes;
    let mut replayed_total = 0u32;
    let mut tokens = 0u32;

    for item in &prepared[range] {
        notes.extend(item.notes.iter().cloned());
        replayed_total += item.replayed;
        // A message whose every part was dropped (e.g. assistant reasoning
        // only) would serialise as empty content, which some APIs reject.
        if item.parts.is_empty() {
            continue;
        }
        tokens = tokens.saturating_add(item.cost);
        verbatim.push(item.source_id);
        messages.push(RenderedMessage {
            source_id: item.source_id,
            role: item.role,
            parts: item.parts.clone(),
        });
    }

    let system = match (p.system_prompt, summary) {
        (Some(s), Some(sum)) => Some(format!("{s}\n\n{SUMMARY_HEADER}\n\n{}", sum.text)),
        (None, Some(sum)) => Some(format!("{SUMMARY_HEADER}\n\n{}", sum.text)),
        (Some(s), None) => Some(s.to_owned()),
        (None, None) => None,
    };
    if let Some(sum) = summary {
        tokens = tokens.saturating_add(sum.token_estimate);
    }

    BuiltView {
        view: ContextView {
            target: p.target.clone(),
            mode,
            summary: summary.map(|s| s.id),
            verbatim_messages: verbatim,
            transformations: notes,
            opaque_replayed: replayed_total > 0,
            estimated_input_tokens: tokens,
        },
        rendered: RenderedView { system, messages },
    }
}

/// Build the view a target model should receive for the current path.
///
/// Returns `NeedsSummary` when history must be compressed; the orchestrator
/// produces the summary, stores it, and calls again.
///
/// # Errors
/// Fails rather than truncating silently when the target has no usable input
/// budget, when the latest user message cannot fit at all, or when a cached
/// summary is far over budget (master spec s6.2 step 4).
pub fn build_view(p: BuildViewParams<'_>) -> Result<ViewPlan, ContextError> {
    if p.path.is_empty() {
        return Err(ContextError::EmptyPath);
    }
    let latest_user = p
        .path
        .iter()
        .rposition(|m| m.role == Role::User)
        .ok_or(ContextError::NoUserMessage)?;

    let margin = p.cfg.safety_margin(p.caps.context_window);
    let system_tokens = p.system_prompt.map_or(0, |s| p.counter.count_text(s));
    let available = p
        .caps
        .context_window
        .checked_sub(p.caps.max_output_tokens)
        .and_then(|v| v.checked_sub(system_tokens))
        .and_then(|v| v.checked_sub(margin))
        .filter(|v| *v > 0)
        .ok_or_else(|| ContextError::NoInputBudget {
            target: p.target.clone(),
            context_window: p.caps.context_window,
            max_output: p.caps.max_output_tokens,
            system_tokens,
            margin,
        })?;

    // Step 2: does the whole path fit verbatim, opaque blocks included?
    //
    // The bar is the compaction trigger, not the hard limit. Compacting only
    // on overflow means compacting every turn once a conversation is long,
    // because each new exchange pushes it back over the line.
    let compact_above = p.cfg.compaction_trigger(available);
    let full = prepare(&p, true);
    let full_total: u32 = full.iter().map(|i| i.cost).fold(0, u32::saturating_add);
    if full_total <= compact_above {
        return Ok(ViewPlan::Ready(Box::new(assemble(
            &p,
            &full,
            0..full.len(),
            ViewMode::Full,
            None,
            Vec::new(),
        ))));
    }

    // Step 3: summary + recent. Opaque blocks are dropped in this mode, so
    // costs must be recomputed rather than reused from the full pass.
    let recent = prepare(&p, false);
    let path_ids: Vec<MsgId> = p.path.iter().map(|m| m.id).collect();

    // Step 3a: hold the existing boundary if it still fits.
    //
    // This is what makes the headroom real. Recomputing the window from
    // scratch every turn would creep the boundary forward as the path grows,
    // evicting one more message each time and invalidating the summary — so
    // every turn would pay for a new one. Keeping the boundary where the last
    // compaction put it lets the recent window absorb new turns until it
    // reaches the trigger again.
    if let Some((summary, covered_to)) = p
        .summaries
        .iter()
        .filter_map(|s| s.covered_prefix_end(&path_ids).map(|end| (s, end)))
        .filter(|(s, _)| s.token_estimate <= p.cfg.summary_overflow_limit(available))
        .max_by_key(|(_, end)| *end)
    {
        let mut start = covered_to + 1;
        let mut extra_notes = Vec::new();
        while start < latest_user && recent[start].role != Role::User {
            extra_notes.push(TransformNote::LeadingAssistantTrimmed {
                message: recent[start].source_id,
            });
            start += 1;
        }

        if start <= latest_user {
            let kept: u32 = recent[start..]
                .iter()
                .map(|i| i.cost)
                .fold(0, u32::saturating_add);
            let total = kept.saturating_add(summary.token_estimate);
            if total <= compact_above {
                return Ok(ViewPlan::Ready(Box::new(assemble(
                    &p,
                    &recent,
                    start..recent.len(),
                    ViewMode::SummaryPlusRecent,
                    Some(summary),
                    extra_notes,
                ))));
            }
        }
    }

    let latest_cost = recent[latest_user].cost;
    if latest_cost > available {
        return Err(ContextError::LatestUserMessageTooLarge {
            target: p.target.clone(),
            message_tokens: latest_cost,
            available,
        });
    }

    // Compact down to the target, not merely to what fits. The gap between
    // the target and the trigger is the headroom that keeps the next few
    // turns from compacting again.
    let target = p.cfg.compaction_target(available);

    // Reserve room for a summary, but never at the cost of the latest user
    // message, which is always sent verbatim (master spec s6.2 step 4).
    let mut summary_budget = p.cfg.summary_budget(target);
    if latest_cost > available.saturating_sub(summary_budget) {
        summary_budget = available.saturating_sub(latest_cost);
    }

    // Aim for the target, but overshoot it as far as the real limit when the
    // latest message alone needs the room. Being over the target only costs
    // headroom; dropping the user's question would be wrong.
    let recent_budget = target
        .saturating_sub(summary_budget)
        .max(latest_cost)
        .min(available.saturating_sub(summary_budget));

    // Walk backwards from the leaf, whole messages only, never splitting one.
    let mut used = 0u32;
    let mut start = recent.len();
    for idx in (0..recent.len()).rev() {
        let next = used.saturating_add(recent[idx].cost);
        if next > recent_budget && idx < latest_user {
            break;
        }
        used = next;
        start = idx;
    }

    // Keep the window opening on a user message: a window that starts with an
    // assistant turn reads as a reply to nothing, and some providers reject it.
    let mut extra_notes = Vec::new();
    while start < latest_user && recent[start].role != Role::User {
        extra_notes.push(TransformNote::LeadingAssistantTrimmed {
            message: recent[start].source_id,
        });
        start += 1;
    }

    if start == 0 {
        // Nothing was evicted: the path only overflowed because of opaque
        // payloads, which this mode drops. No summary is needed.
        return Ok(ViewPlan::Ready(Box::new(assemble(
            &p,
            &recent,
            0..recent.len(),
            ViewMode::SummaryPlusRecent,
            None,
            extra_notes,
        ))));
    }

    let covering = 0..start;
    let covers_from = path_ids[0];
    let covers_to = path_ids[start - 1];
    let summary_limit = p.cfg.summary_overflow_limit(summary_budget);

    if let Some(found) = p.summaries.iter().find(|s| s.covers(&path_ids, start - 1)) {
        if found.token_estimate > summary_limit {
            return Err(ContextError::SummaryTooLarge {
                summary_tokens: found.token_estimate,
                limit: summary_limit,
            });
        }
        return Ok(ViewPlan::Ready(Box::new(assemble(
            &p,
            &recent,
            start..recent.len(),
            ViewMode::SummaryPlusRecent,
            Some(found),
            extra_notes,
        ))));
    }

    // No exact match. An older summary covering a shorter prefix of this same
    // path can be extended instead of resummarising from scratch.
    let extends = p
        .summaries
        .iter()
        .filter_map(|s| s.covered_prefix_end(&path_ids).map(|end| (s.id, end)))
        .filter(|(_, end)| *end < start - 1)
        .max_by_key(|(_, end)| *end)
        .map(|(id, _)| id);

    Ok(ViewPlan::NeedsSummary(SummaryRequest {
        covering,
        covers_from,
        covers_to,
        suggested_writer: p.outgoing.unwrap_or(p.target).clone(),
        budget_tokens: summary_budget,
        extends,
    }))
}
