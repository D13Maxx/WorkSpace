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

    /// The file a call will edit, if known, so it can be snapshotted for a
    /// diff and undo. Must stay inside the project: undo writes to it.
    fn file_for(&self, _name: &str, _args: &serde_json::Value) -> Option<std::path::PathBuf> {
        None
    }
}

/// Several hosts offered as one: the skill loader alongside the MCP servers.
#[derive(Debug)]
pub struct ToolSet(Vec<std::sync::Arc<dyn ToolHost>>);

impl ToolSet {
    #[must_use]
    pub fn new(hosts: Vec<std::sync::Arc<dyn ToolHost>>) -> Self {
        Self(hosts)
    }

    fn owner(&self, name: &str) -> Option<&std::sync::Arc<dyn ToolHost>> {
        self.0
            .iter()
            .find(|h| h.specs().iter().any(|s| s.name == name))
    }
}

#[async_trait::async_trait]
impl ToolHost for ToolSet {
    fn specs(&self) -> Vec<ToolSpec> {
        self.0.iter().flat_map(|h| h.specs()).collect()
    }

    fn needs_approval(&self, name: &str) -> bool {
        self.owner(name).is_none_or(|h| h.needs_approval(name))
    }

    fn file_for(&self, name: &str, args: &serde_json::Value) -> Option<std::path::PathBuf> {
        self.owner(name).and_then(|h| h.file_for(name, args))
    }

    async fn call(&self, name: &str, args: serde_json::Value) -> ToolOutput {
        match self.owner(name) {
            Some(h) => h.call(name, args).await,
            None => ToolOutput::error(format!("there is no tool called {name}")),
        }
    }
}

#[cfg(test)]
mod tests {
    // Spec s14 permits unwrap/expect in tests.
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;
    use std::sync::Arc;

    #[derive(Debug)]
    struct One(&'static str, bool);

    #[async_trait::async_trait]
    impl ToolHost for One {
        fn specs(&self) -> Vec<ToolSpec> {
            vec![ToolSpec {
                name: self.0.to_owned(),
                description: String::new(),
                input_schema: serde_json::json!({"type": "object"}),
            }]
        }
        fn needs_approval(&self, _: &str) -> bool {
            self.1
        }
        async fn call(&self, _: &str, _: serde_json::Value) -> ToolOutput {
            ToolOutput::ok(self.0)
        }
    }

    #[tokio::test]
    async fn a_tool_set_routes_each_call_to_the_host_that_offers_it() {
        let set = ToolSet::new(vec![
            Arc::new(One("load_skill", false)),
            Arc::new(One("serena__edit", true)),
        ]);
        assert_eq!(set.specs().len(), 2);
        assert_eq!(
            set.call("serena__edit", serde_json::json!({})).await.text,
            "serena__edit"
        );
        assert!(!set.needs_approval("load_skill"));
        assert!(set.needs_approval("serena__edit"));
        assert!(set.needs_approval("missing"), "unknown asks");
        assert!(set.call("missing", serde_json::json!({})).await.is_error);
    }
}
