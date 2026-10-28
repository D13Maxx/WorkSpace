//! Cached summaries of evicted history (master spec s6.5).

use crate::ids::{ConvId, MsgId, SummaryId};
use crate::model::ModelRef;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// The summarisation prompt, versioned so a stored summary records which
/// wording produced it. Bump on any edit; never edit in place.
pub const SUMMARY_PROMPT_VERSION: u32 = 1;

/// Build the summariser prompt for a budget of `max_tokens`.
#[must_use]
pub fn summary_prompt(max_tokens: u32) -> String {
    format!(
        "Summarize the conversation excerpt below for another AI model that will \
         continue it. Preserve: the user's goals, decisions made, facts and \
         constraints stated, open questions, the user's stated preferences, and \
         names of files, code, or artifacts with what was decided about them. Do \
         not add anything not present. Do not give advice. Use compact bullet \
         points. Stay under {max_tokens} tokens."
    )
}

/// A summary of a contiguous run of messages along one path.
///
/// Summaries belong to a path, not to a conversation: a summary built on
/// branch A is invalid on branch B because the messages it covers are not on
/// B. [`ContextSummary::covers`] enforces that at use time.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ContextSummary {
    pub id: SummaryId,
    pub conversation_id: ConvId,
    /// The leaf the conversation was on when this summary was written. Kept
    /// for provenance and debugging; validity is decided by `covers`.
    pub branch_leaf_at_creation: MsgId,
    pub covers_from: MsgId,
    pub covers_to: MsgId,
    pub text: String,
    pub written_by: ModelRef,
    pub prompt_version: u32,
    pub token_estimate: u32,
    pub created_at: DateTime<Utc>,
}

impl ContextSummary {
    /// True if this summary covers exactly `path[..=end_idx]`.
    ///
    /// Requires every covered message to sit on the given path, in order,
    /// starting at its root. A summary that covers a *shorter* prefix is not
    /// a match here; the caller asks for an incremental summary instead.
    #[must_use]
    pub fn covers(&self, path_ids: &[MsgId], end_idx: usize) -> bool {
        match (path_ids.first(), path_ids.get(end_idx)) {
            (Some(first), Some(last)) => self.covers_from == *first && self.covers_to == *last,
            _ => false,
        }
    }

    /// True if this summary covers a prefix of `path_ids`, returning the index
    /// it ends at. Used to build an incremental summary from an older one.
    #[must_use]
    pub fn covered_prefix_end(&self, path_ids: &[MsgId]) -> Option<usize> {
        if path_ids.first() != Some(&self.covers_from) {
            return None;
        }
        path_ids.iter().position(|id| *id == self.covers_to)
    }
}
