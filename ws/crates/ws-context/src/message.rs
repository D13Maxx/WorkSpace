//! The canonical conversation format (master spec s5).
//!
//! Invariant 1: messages are immutable once persisted. Nothing in this module
//! offers interior mutability, and `ws-store` backs the rule with a trigger.
//!
//! Invariant 3: provider-native data is kept byte-for-byte in [`ProviderOpaque`]
//! alongside the neutral representation, never instead of it.

use crate::ids::{BlobRef, ConvId, EventId, MsgId};
use crate::model::{ModelRef, ProviderKind};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Role {
    System,
    User,
    Assistant,
    Tool,
}

/// How a message ended. A cancelled or failed message keeps whatever text
/// arrived before the stream stopped; it is never silently discarded.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum MsgStatus {
    Complete,
    Cancelled,
    Failed { reason: String },
}

/// Provider-native data that must survive a round trip unchanged.
///
/// Anthropic thinking signatures, OpenAI encrypted reasoning items and Gemini
/// thought signatures all refer cryptographically to the exact context that
/// produced them, so the payload is stored verbatim and never rebuilt.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProviderOpaque {
    pub provider: ProviderKind,
    /// The exact model id that produced this block. Replay compares against
    /// this, not against the provider (master spec s6.4).
    pub model_id: String,
    /// e.g. `anthropic.thinking`, `anthropic.redacted_thinking`,
    /// `openai.reasoning_item`, `gemini.part_with_signature`.
    pub kind: String,
    /// Index among this message's provider-native blocks, so replay can
    /// restore original ordering.
    pub position: u32,
    pub payload: serde_json::Value,
}

/// One piece of message content.
///
/// `ToolCall` and `ToolResult` are unused in M1 but present so that adding
/// tool calling later needs no data migration (master spec s3).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Part {
    Text {
        text: String,
    },
    Image {
        blob: BlobRef,
        mime: String,
        name: Option<String>,
    },
    File {
        blob: BlobRef,
        mime: String,
        name: String,
    },
    /// Human-readable reasoning the UI may show. Never sent to any model by
    /// default (master spec s6.3); the replayable form is `Opaque`.
    Reasoning {
        visible_text: Option<String>,
    },
    ToolCall {
        call_id: String,
        name: String,
        args_json: String,
    },
    ToolResult {
        call_id: String,
        content: Vec<Part>,
        is_error: bool,
    },
    Opaque(ProviderOpaque),
}

impl Part {
    #[must_use]
    pub fn text(s: impl Into<String>) -> Self {
        Self::Text { text: s.into() }
    }

    /// Plain text carried by this part, if any. Used for token estimation and
    /// for rendering placeholders when a target lacks a capability.
    #[must_use]
    pub fn as_text(&self) -> Option<&str> {
        match self {
            Self::Text { text } => Some(text),
            _ => None,
        }
    }
}

/// A single message. Immutable after persistence (invariant 1).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Message {
    pub id: MsgId,
    pub conversation_id: ConvId,
    /// `None` only for the root of a conversation.
    pub parent_id: Option<MsgId>,
    pub role: Role,
    /// Set for assistant messages: which model wrote this.
    pub author_model: Option<ModelRef>,
    pub parts: Vec<Part>,
    pub created_at: DateTime<Utc>,
    pub status: MsgStatus,
}

impl Message {
    /// Concatenated plain text of this message, ignoring non-text parts.
    #[must_use]
    pub fn text_content(&self) -> String {
        self.parts
            .iter()
            .filter_map(Part::as_text)
            .collect::<Vec<_>>()
            .join("")
    }

    /// Provider-native blocks in stored order.
    pub fn opaque_parts(&self) -> impl Iterator<Item = &ProviderOpaque> {
        self.parts.iter().filter_map(|p| match p {
            Part::Opaque(o) => Some(o),
            _ => None,
        })
    }
}

/// A conversation. Only `active_leaf_id`, `title`, `current_model` and
/// `system_prompt` are mutable; the message tree itself is append-only.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Conversation {
    pub id: ConvId,
    pub title: String,
    pub system_prompt: Option<String>,
    /// Pointer into the message tree. The visible thread is root -> this leaf.
    pub active_leaf_id: Option<MsgId>,
    pub current_model: ModelRef,
    pub created_at: DateTime<Utc>,
}

/// Something that happened in a conversation but is not a message and is
/// never sent to a model (master spec s5.3).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum EventKind {
    ModelSwitch { from: ModelRef, to: ModelRef },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ConversationEvent {
    pub id: EventId,
    pub conversation_id: ConvId,
    /// The event is rendered inline after this message.
    pub after_message_id: Option<MsgId>,
    pub kind: EventKind,
    pub at: DateTime<Utc>,
}
