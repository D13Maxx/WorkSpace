//! Shared fixtures for context-builder tests. Each test binary uses only
//! part of this module, so dead code is expected here.
#![allow(dead_code)]
// Spec s14 permits unwrap/expect in tests, where a panic is the failure report.
#![allow(clippy::unwrap_used, clippy::expect_used)]
use chrono::{TimeZone, Utc};
use ws_context::{
    ContextSummary, ConvId, Conversation, Message, ModelCapabilities, ModelRef, MsgId, MsgStatus,
    Part, ProviderKind, ProviderOpaque, ReasoningSupport, Role, SummaryId, TokenCounting,
};

/// A token counter with a fixed, predictable rate: one token per 4 bytes,
/// rounded up. Tests assert exact budgets, so they need a counter whose
/// arithmetic is obvious rather than the shipped estimate's byte/3 plus 10%.
#[derive(Debug, Clone, Copy)]
pub struct FixedCounter;

impl ws_context::TokenCounter for FixedCounter {
    fn count_text(&self, text: &str) -> u32 {
        u32::try_from(text.len().div_ceil(4)).unwrap_or(u32::MAX)
    }
}

pub fn anthropic_model() -> ModelRef {
    ModelRef::new("anthropic", "claude-test-1")
}

pub fn other_model() -> ModelRef {
    ModelRef::new("anthropic", "claude-test-2")
}

pub fn openai_model() -> ModelRef {
    ModelRef::new("openai", "gpt-test-1")
}

pub fn caps(context_window: u32) -> ModelCapabilities {
    ModelCapabilities {
        context_window,
        max_output_tokens: 256,
        vision: false,
        tools: false,
        reasoning: ReasoningSupport::Available,
        token_counting: TokenCounting::Estimate,
        image_token_cost: 100,
    }
}

pub fn vision_caps(context_window: u32) -> ModelCapabilities {
    ModelCapabilities {
        vision: true,
        ..caps(context_window)
    }
}

/// Builds a linear path with correct parent links and increasing ids.
#[derive(Debug, Default)]
pub struct PathBuilder {
    conv: Option<ConvId>,
    messages: Vec<Message>,
}

impl PathBuilder {
    pub fn new() -> Self {
        Self {
            conv: Some(ConvId::new()),
            messages: Vec::new(),
        }
    }

    fn conv_id(&self) -> ConvId {
        self.conv.unwrap_or_default()
    }

    fn push(&mut self, role: Role, author: Option<ModelRef>, parts: Vec<Part>) -> MsgId {
        let parent = self.messages.last().map(|m| m.id);
        let id = MsgId::new();
        let n = self.messages.len() as i64;
        self.messages.push(Message {
            id,
            conversation_id: self.conv_id(),
            parent_id: parent,
            role,
            author_model: author,
            parts,
            // Fixed clock: tests must not depend on wall time.
            created_at: Utc.timestamp_opt(1_700_000_000 + n, 0).unwrap(),
            status: MsgStatus::Complete,
        });
        id
    }

    pub fn user(mut self, text: &str) -> Self {
        self.push(Role::User, None, vec![Part::text(text)]);
        self
    }

    pub fn assistant(mut self, model: &ModelRef, text: &str) -> Self {
        self.push(Role::Assistant, Some(model.clone()), vec![Part::text(text)]);
        self
    }

    /// An assistant message carrying a provider-native block alongside text.
    pub fn assistant_with_opaque(mut self, model: &ModelRef, text: &str, blocks: u32) -> Self {
        let mut parts = Vec::new();
        for position in 0..blocks {
            parts.push(Part::Opaque(ProviderOpaque {
                provider: ProviderKind::Anthropic,
                model_id: model.model_id.clone(),
                kind: "anthropic.thinking".to_owned(),
                position,
                payload: serde_json::json!({
                    "type": "thinking",
                    "thinking": format!("step {position}"),
                    "signature": "sig-abc",
                }),
            }));
        }
        parts.push(Part::text(text));
        self.push(Role::Assistant, Some(model.clone()), parts);
        self
    }

    /// One tool round: an assistant tool call, then its result.
    pub fn tool_round(mut self, model: &ModelRef, call_id: &str, output: &str) -> Self {
        let call = Part::ToolCall {
            call_id: call_id.to_owned(),
            name: "read_file".to_owned(),
            args_json: "{}".to_owned(),
        };
        self.push(Role::Assistant, Some(model.clone()), vec![call]);
        let result = Part::ToolResult {
            call_id: call_id.to_owned(),
            content: vec![Part::text(output)],
            is_error: false,
        };
        self.push(Role::Tool, None, vec![result]);
        self
    }

    pub fn user_with_parts(mut self, parts: Vec<Part>) -> Self {
        self.push(Role::User, None, parts);
        self
    }

    pub fn build(self) -> Vec<Message> {
        self.messages
    }
}

/// A summary covering `path[..=end]`, sized at `tokens`.
pub fn summary_for(path: &[Message], end: usize, tokens: u32, writer: &ModelRef) -> ContextSummary {
    ContextSummary {
        id: SummaryId::new(),
        conversation_id: path[0].conversation_id,
        branch_leaf_at_creation: path[path.len() - 1].id,
        covers_from: path[0].id,
        covers_to: path[end].id,
        text: "- earlier context".to_owned(),
        written_by: writer.clone(),
        prompt_version: ws_context::SUMMARY_PROMPT_VERSION,
        token_estimate: tokens,
        created_at: Utc.timestamp_opt(1_700_000_000, 0).unwrap(),
    }
}

pub fn conversation(model: &ModelRef) -> Conversation {
    Conversation {
        id: ConvId::new(),
        title: "test".to_owned(),
        system_prompt: None,
        active_leaf_id: None,
        current_model: model.clone(),
        created_at: Utc.timestamp_opt(1_700_000_000, 0).unwrap(),
    }
}
