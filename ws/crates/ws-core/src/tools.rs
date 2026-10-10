//! Tools the model can call during a turn.
//!
//! The orchestrator only knows this trait. The MCP client implements it for
//! real servers such as Serena; tests implement it with a fake.

use ws_context::ToolSpec;

/// What running one tool produced. Errors go back to the model as text, so
/// it can correct itself rather than the turn failing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolOutput {
    pub text: String,
    pub is_error: bool,
}

impl ToolOutput {
    #[must_use]
    pub fn ok(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            is_error: false,
        }
    }

    #[must_use]
    pub fn error(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            is_error: true,
        }
    }
}

#[async_trait::async_trait]
pub trait ToolHost: Send + Sync + std::fmt::Debug {
    /// Every tool on offer, with names unique across servers.
    fn specs(&self) -> Vec<ToolSpec>;

    /// Whether the user must approve a call before it runs. Anything that can
    /// change files or run commands must answer true.
    fn needs_approval(&self, name: &str) -> bool;

    async fn call(&self, name: &str, args: serde_json::Value) -> ToolOutput;
}
