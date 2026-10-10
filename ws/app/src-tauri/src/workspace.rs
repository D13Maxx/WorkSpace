//! The agent's own tools for the folder open in FRIDAY: read, write and edit
//! files, and run commands. They work with no tool server installed.
//!
//! Writes go through `file_for`, so every one is snapshotted and shown as a
//! diff with Undo. Commands cannot be tracked that way, so they always ask.

use serde_json::{Value, json};
use std::sync::Mutex;
use std::time::Duration;
use ws_context::ToolSpec;
use ws_core::{ToolHost, ToolOutput};

/// A command still running after this is stopped.
const COMMAND_TIMEOUT: Duration = Duration::from_secs(600);

/// The folder and file the user has open, as the UI last reported them.
#[derive(Debug, Default)]
pub struct Workspace {
    open: Mutex<(Option<String>, Option<String>)>,
}

impl Workspace {
    pub fn set(&self, root: Option<String>, file: Option<String>) {
        if let Ok(mut o) = self.open.lock() {
            *o = (root.filter(|r| !r.trim().is_empty()), file);
        }
    }

    fn root(&self) -> Result<String, ToolOutput> {
        self.open
            .lock()
            .ok()
            .and_then(|o| o.0.clone())
            .ok_or_else(|| {
                ToolOutput::error("No folder is open in FRIDAY. Ask the user to open one.")
            })
    }
}

fn spec(name: &str, description: &str, properties: Value, required: &[&str]) -> ToolSpec {
    ToolSpec {
        name: name.to_owned(),
        description: description.to_owned(),
        input_schema: json!({"type": "object", "properties": properties, "required": required}),
    }
}

fn arg<'a>(args: &'a Value, key: &str) -> Result<&'a str, ToolOutput> {
    args.get(key)
        .and_then(Value::as_str)
        .ok_or_else(|| ToolOutput::error(format!("missing \"{key}\"")))
}

/// Replace the one occurrence of `old` in `text`. CRLF files are matched with
/// the model's `\n` text too, since models rarely send `\r`.
fn replace_once(text: &str, old: &str, new: &str) -> Result<String, String> {
    let crlf = (
        old.replace("\r\n", "\n").replace('\n', "\r\n"),
        new.replace("\r\n", "\n").replace('\n', "\r\n"),
    );
    for (old, new) in [(old, new), (crlf.0.as_str(), crlf.1.as_str())] {
        match text.matches(old).count() {
            0 => continue,
            1 => return Ok(text.replacen(old, new, 1)),
            n => {
                return Err(format!(
                    "old_text appears {n} times; include more surrounding lines so it is unique"
                ));
            }
        }
    }
    Err("old_text was not found in the file; read it again and copy the text exactly".to_owned())
}

async fn run_command(root: &str, command: &str) -> ToolOutput {
    let mut cmd = if cfg!(windows) {
        let mut c = tokio::process::Command::new("cmd");
        c.arg("/C");
        #[cfg(windows)]
        {
            // As typed: Rust's own quoting would garble cmd's.
            c.raw_arg(command);
            // No console window flashing up over the app.
            c.creation_flags(0x0800_0000);
        }
        c
    } else {
        let mut c = tokio::process::Command::new("sh");
        c.arg("-c").arg(command);
        c
    };
    cmd.current_dir(root)
        .stdin(std::process::Stdio::null())
        .kill_on_drop(true);
    match tokio::time::timeout(COMMAND_TIMEOUT, cmd.output()).await {
        Err(_) => ToolOutput::error(format!(
            "stopped after {} seconds",
            COMMAND_TIMEOUT.as_secs()
        )),
        Ok(Err(e)) => ToolOutput::error(format!("could not run it: {e}")),
        Ok(Ok(out)) => {
            let text = format!(
                "exit code {}\n--- stdout ---\n{}\n--- stderr ---\n{}",
                out.status
                    .code()
                    .map_or_else(|| "none".to_owned(), |c| c.to_string()),
                String::from_utf8_lossy(&out.stdout),
                String::from_utf8_lossy(&out.stderr)
            );
            if out.status.success() {
                ToolOutput::ok(text)
            } else {
                ToolOutput::error(text)
            }
        }
    }
}

#[async_trait::async_trait]
impl ToolHost for Workspace {
    fn specs(&self) -> Vec<ToolSpec> {
        let path = json!({"type": "string", "description": "Relative to the open folder, with / separators."});
        vec![
            spec(
                "get_open_file",
                "The folder open in FRIDAY and the file the user is looking at. Call this first when the user says \"this file\" or \"this document\".",
                json!({}),
                &[],
            ),
            spec(
                "list_dir",
                "List one folder of the open project. Folders end with /.",
                json!({"path": {"type": "string", "description": "Relative folder; empty for the top."}}),
                &[],
            ),
            spec(
                "read_file",
                "Read a text file of the open project. PDF and Word files are not text: read those with run_command and Python (pypdf, python-docx; pip install them if missing).",
                json!({"path": path}),
                &["path"],
            ),
            spec(
                "write_file",
                "Create or overwrite a text file in the open project with the full content given. Folders are created as needed.",
                json!({"path": path, "content": {"type": "string"}}),
                &["path", "content"],
            ),
            spec(
                "edit_file",
                "Replace one exact, unique piece of a text file. Prefer this to write_file for changes to existing files.",
                json!({"path": path, "old_text": {"type": "string"}, "new_text": {"type": "string"}}),
                &["path", "old_text", "new_text"],
            ),
            spec(
                "run_command",
                "Run a shell command in the open project's folder (cmd.exe on Windows) and return its output. Use it to compile, run, test, or modify PDF and Word files with Python. No input can be typed into it.",
                json!({"command": {"type": "string"}}),
                &["command"],
            ),
        ]
    }

    fn needs_approval(&self, name: &str) -> bool {
        !matches!(name, "get_open_file" | "list_dir" | "read_file")
    }

    fn file_for(&self, name: &str, args: &Value) -> Option<std::path::PathBuf> {
        if !matches!(name, "write_file" | "edit_file") {
            return None;
        }
        let root = self.root().ok()?;
        crate::files::resolve_for_write(&root, args.get("path")?.as_str()?).ok()
    }

    async fn call(&self, name: &str, args: Value) -> ToolOutput {
        let root = match self.root() {
            Ok(r) => r,
            Err(e) if name == "get_open_file" => return ToolOutput::ok(e.text),
            Err(e) => return e,
        };
        let result = match name {
            "get_open_file" => {
                let file = self.open.lock().ok().and_then(|o| o.1.clone());
                Ok(match file {
                    Some(f) => format!("Folder: {root}\nOpen file: {f}"),
                    None => format!("Folder: {root}\nNo file is open."),
                })
            }
            "list_dir" => {
                let dir = args.get("path").and_then(Value::as_str).unwrap_or("");
                crate::files::list_dir(&root, dir).map(|entries| {
                    entries
                        .iter()
                        .map(|e| {
                            if e.is_dir {
                                format!("{}/", e.path)
                            } else {
                                e.path.clone()
                            }
                        })
                        .collect::<Vec<_>>()
                        .join("\n")
                })
            }
            "read_file" => match arg(&args, "path") {
                Ok(p) => crate::files::read_text(&root, p),
                Err(e) => return e,
            },
            "write_file" => match (arg(&args, "path"), arg(&args, "content")) {
                (Ok(p), Ok(c)) => {
                    crate::files::write_text(&root, p, c).map(|()| format!("wrote {p}"))
                }
                (Err(e), _) | (_, Err(e)) => return e,
            },
            "edit_file" => match (
                arg(&args, "path"),
                arg(&args, "old_text"),
                arg(&args, "new_text"),
            ) {
                (Ok(p), Ok(old), Ok(new)) => crate::files::read_text(&root, p)
                    .and_then(|text| replace_once(&text, old, new))
                    .and_then(|text| crate::files::write_text(&root, p, &text))
                    .map(|()| format!("edited {p}")),
                (Err(e), _, _) | (_, Err(e), _) | (_, _, Err(e)) => return e,
            },
            "run_command" => match arg(&args, "command") {
                Ok(c) => return run_command(&root, c).await,
                Err(e) => return e,
            },
            _ => Err(format!("there is no tool called {name}")),
        };
        match result {
            Ok(text) => ToolOutput::ok(text),
            Err(e) => ToolOutput::error(e),
        }
    }
}

#[cfg(test)]
mod tests {
    // Spec s14 permits unwrap/expect in tests.
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;

    #[test]
    fn an_edit_must_match_exactly_once() {
        assert_eq!(replace_once("a b a", "b", "c").unwrap(), "a c a");
        assert!(
            replace_once("a b a", "a", "c")
                .unwrap_err()
                .contains("2 times")
        );
        assert!(
            replace_once("a", "z", "c")
                .unwrap_err()
                .contains("not found")
        );
        assert_eq!(
            replace_once("x\r\ny\r\n", "x\ny", "x\nz").unwrap(),
            "x\r\nz\r\n",
            "CRLF files take the model's LF text"
        );
    }

    #[tokio::test]
    async fn the_tools_work_on_the_open_folder_only() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().display().to_string();
        let ws = Workspace::default();
        let read = |p: &str| json!({ "path": p });
        assert!(
            ws.call("read_file", read("a.txt")).await.is_error,
            "no folder yet"
        );

        ws.set(Some(root), Some("a.txt".to_owned()));
        let w = ws
            .call(
                "write_file",
                json!({"path": "src/a.txt", "content": "one\ntwo\n"}),
            )
            .await;
        assert!(!w.is_error, "{}", w.text);
        let e = ws
            .call(
                "edit_file",
                json!({"path": "src/a.txt", "old_text": "two", "new_text": "2"}),
            )
            .await;
        assert!(!e.is_error, "{}", e.text);
        assert_eq!(
            ws.call("read_file", read("src/a.txt")).await.text,
            "one\n2\n"
        );
        assert_eq!(ws.call("list_dir", json!({})).await.text, "src/");
        assert!(
            ws.call("get_open_file", json!({}))
                .await
                .text
                .contains("a.txt")
        );
        assert!(ws.call("read_file", read("../x")).await.is_error);
        assert!(ws.file_for("write_file", &read("../x")).is_none());
        assert!(ws.file_for("write_file", &read("b.txt")).is_some());

        assert!(!ws.needs_approval("read_file"));
        assert!(ws.needs_approval("write_file"));
        assert!(ws.needs_approval("run_command"));
        let r = ws.call("run_command", json!({"command": "echo hi"})).await;
        assert!(r.text.contains("hi"), "{}", r.text);
    }
}
