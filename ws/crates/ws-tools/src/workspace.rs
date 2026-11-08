//! The agent's own tools for the folder open in Workspace: read, write and edit
//! files, and run commands. They work with no tool server installed.
//!
//! Writes go through `file_for`, so every one is snapshotted and shown as a
//! diff with Undo. Commands cannot be tracked that way, so they always ask.

use serde_json::{Value, json};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use ws_context::ToolSpec;
use ws_core::{ToolHost, ToolOutput};

/// Search stops after this many matching lines.
const MAX_MATCHES: usize = 200;
/// Files bigger than this are not searched (a knowledge graph is well under).
const MAX_SEARCH_BYTES: u64 = 32 * 1024 * 1024;

/// The project outline's size in the prompt: about 2,000 tokens.
const OUTLINE_CHARS: usize = 8_000;

/// Lines of uses `find_symbol` lists after the definitions.
const MAX_USES: usize = 30;

/// Definitions of `name` (exact, or else names containing it, ignoring
/// case), then lines that use it as a whole word.
fn find_symbol(index: &crate::index::Index, root: &str, name: &str) -> Result<String, String> {
    let defs = index.scan(Path::new(root));
    let mut found: Vec<_> = defs.iter().filter(|d| d.name == name).collect();
    if found.is_empty() {
        let lower = name.to_lowercase();
        found = defs
            .iter()
            .filter(|d| d.name.to_lowercase().contains(&lower))
            .take(MAX_USES)
            .collect();
    }
    if found.is_empty() {
        return Ok(format!(
            "No definition named like {name}. Try search_files for text, or check the spelling."
        ));
    }
    let mut out = String::from("Defined at:\n");
    for d in &found {
        out.push_str(&format!("{}:{} {} {}\n", d.path, d.line, d.kind, d.name));
    }
    let whole_word = |text: &str| {
        text.match_indices(name).any(|(at, _)| {
            let word = |c: char| c.is_alphanumeric() || c == '_';
            !text[..at].ends_with(word) && !text[at + name.len()..].starts_with(word)
        })
    };
    let uses: Vec<_> = search_matches(root, "", name)
        .map(|(m, _)| m)
        .unwrap_or_default()
        .into_iter()
        .filter(|m| whole_word(&m.text))
        .filter(|m| !found.iter().any(|d| d.path == m.path && d.line == m.line))
        .take(MAX_USES)
        .collect();
    if uses.is_empty() {
        out.push_str("No other lines use it.\n");
    } else {
        out.push_str("Used at:\n");
        for m in uses {
            out.push_str(&format!("{}:{}: {}\n", m.path, m.line, m.text.trim()));
        }
    }
    Ok(out)
}

/// A command still running after this is stopped.
const COMMAND_TIMEOUT: Duration = Duration::from_secs(600);

/// What a chooser returns: the folder picked, or `None` if declined.
pub type Choice = std::pin::Pin<Box<dyn Future<Output = Option<PathBuf>> + Send>>;

/// Asks the user to pick a folder, saying why; `None` if they decline.
#[derive(Clone)]
pub struct Chooser(pub Arc<dyn Fn(String) -> Choice + Send + Sync>);

impl std::fmt::Debug for Chooser {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Chooser")
    }
}

/// The folder and file the user has open, as the UI last reported them.
#[derive(Debug, Default)]
pub struct Workspace {
    open: Mutex<(Option<String>, Option<String>)>,
    /// How the agent asks for a place outside the open folder; without one it
    /// cannot leave the folder.
    chooser: Option<Chooser>,
    /// Folders the user picked when asked. The agent may write inside these
    /// too, by absolute path, until the app closes.
    chosen: Mutex<Vec<PathBuf>>,
    /// Definitions in the open project, for its outline and find_symbol.
    index: Arc<crate::index::Index>,
    /// Git for Windows' bash, when installed. Skills written for Claude Code
    /// (Understand-Anything's) give their steps as bash, so commands run in it.
    bash: Option<std::path::PathBuf>,
    /// The shipped Understand-Anything pipeline, exported to commands as
    /// `CLAUDE_PLUGIN_ROOT`, which is where its skill looks first.
    plugin_root: Option<std::path::PathBuf>,
}

/// Git for Windows' bash, found from `git` on PATH. Not System32's bash.exe,
/// which is WSL.
fn git_bash() -> Option<std::path::PathBuf> {
    let git = crate::find(&["git"])?;
    // git.exe lives in Git\cmd, Git\bin or Git\mingw64\bin.
    git.ancestors()
        .map(|dir| dir.join("bin").join("bash.exe"))
        .find(|bash| bash.is_file())
}

impl Workspace {
    #[must_use]
    pub fn new(plugin_root: Option<std::path::PathBuf>) -> Self {
        Self {
            open: Mutex::default(),
            bash: if cfg!(windows) { git_bash() } else { None },
            plugin_root,
            chooser: None,
            chosen: Mutex::default(),
            index: Arc::default(),
        }
    }

    /// Let the agent ask the user for a folder outside the open one.
    #[must_use]
    pub fn with_chooser(mut self, chooser: Chooser) -> Self {
        self.chooser = Some(chooser);
        self
    }

    /// Where `path` lives: relative paths are in the open folder `root`; an
    /// absolute one must be inside a folder the user chose. Returns the base
    /// folder and the path relative to it.
    fn place(&self, root: &str, path: &str) -> Result<(String, String), String> {
        let p = Path::new(path);
        if !p.is_absolute() {
            return Ok((root.to_owned(), path.to_owned()));
        }
        let chosen = self.chosen.lock().map(|c| c.clone()).unwrap_or_default();
        for base in &chosen {
            if let Ok(rest) = p.strip_prefix(base) {
                return Ok((
                    base.display().to_string(),
                    rest.to_string_lossy().replace('\\', "/"),
                ));
            }
        }
        Err(if self.chooser.is_some() {
            format!(
                "{path} is outside the open folder; call choose_location so the user can pick where it goes, then use paths inside the folder they chose"
            )
        } else {
            format!("{path} is outside the open folder")
        })
    }

    /// Ask the user for a folder and, if they pick one, allow writing there.
    /// With `name`, the user picks where a new folder of that name goes, and
    /// only that folder is given to the agent: picking Desktop must not hand
    /// it the whole Desktop.
    async fn choose(&self, reason: &str, name: Option<&str>) -> Result<String, String> {
        let Some(chooser) = &self.chooser else {
            return Err("there is no way to ask the user for a folder here".to_owned());
        };
        let name = name.map(str::trim).filter(|n| !n.is_empty());
        if let Some(n) = name {
            let mut parts = Path::new(n).components();
            if !matches!(
                (parts.next(), parts.next()),
                (Some(std::path::Component::Normal(_)), None)
            ) {
                return Err(format!("{n} is not a folder name"));
            }
        }
        let asked = match name {
            Some(n) => format!("{reason} (pick where it goes; a folder named {n} is made there)"),
            None => reason.to_owned(),
        };
        let Some(picked) = (chooser.0)(asked).await else {
            return Ok("The user did not choose a folder. Do not write outside the open folder; ask them what they want instead.".to_owned());
        };
        let dir = match name {
            // They may have made and picked the project folder itself.
            Some(n)
                if !picked
                    .file_name()
                    .is_some_and(|f| f.to_string_lossy().eq_ignore_ascii_case(n)) =>
            {
                picked.join(n)
            }
            _ => picked,
        };
        std::fs::create_dir_all(&dir).map_err(|e| format!("cannot use {}: {e}", dir.display()))?;
        if let Ok(mut c) = self.chosen.lock()
            && !c.contains(&dir)
        {
            c.push(dir.clone());
        }
        // With nothing open, the chosen folder is where the agent works: paths
        // there can be relative, and commands run in it.
        if let Ok(mut o) = self.open.lock()
            && o.0.is_none()
        {
            o.0 = Some(dir.display().to_string());
            return Ok(format!(
                "The user chose {}. It is now the working folder: use paths relative to it, and run_command runs there.",
                dir.display()
            ));
        }
        Ok(format!(
            "The user chose {}. Use absolute paths inside it with write_file, create_folder, read_file, edit_file and list_dir.",
            dir.display()
        ))
    }

    pub fn set(&self, root: Option<String>, file: Option<String>) {
        if let Ok(mut o) = self.open.lock() {
            *o = (root.filter(|r| !r.trim().is_empty()), file);
        }
    }

    /// The folder open in Workspace, if any.
    pub fn folder(&self) -> Option<String> {
        self.open.lock().ok().and_then(|o| o.0.clone())
    }

    fn root(&self) -> Result<String, ToolOutput> {
        self.folder().ok_or_else(|| {
            ToolOutput::error(if self.chooser.is_some() {
                "No folder is open in Workspace. Call choose_location to ask the user where to work (it becomes the working folder), or ask them to open one."
            } else {
                "No folder is open in Workspace. Ask the user to open one."
            })
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

/// `command` for the system shell: cmd on Windows, sh elsewhere.
pub fn shell(command: &str) -> tokio::process::Command {
    if cfg!(windows) {
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
    }
}

async fn run_command(
    root: &str,
    command: &str,
    bash: Option<&std::path::Path>,
    plugin_root: Option<&std::path::Path>,
) -> ToolOutput {
    let mut cmd = if let Some(bash) = bash {
        let mut c = tokio::process::Command::new(bash);
        c.arg("-c").arg(command);
        #[cfg(windows)]
        c.creation_flags(0x0800_0000);
        c
    } else {
        shell(command)
    };
    cmd.current_dir(root)
        .stdin(std::process::Stdio::null())
        .kill_on_drop(true);
    if let Some(dir) = plugin_root {
        // Forward slashes read the same in bash, cmd and Node.
        let dir = dir.to_string_lossy().replace('\\', "/");
        cmd.env("CLAUDE_PLUGIN_ROOT", &dir).env("PLUGIN_ROOT", &dir);
    }
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

/// One line that contains the search text.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct Match {
    /// Relative to the project root, with `/` separators.
    pub path: String,
    /// 1-based.
    pub line: usize,
    pub text: String,
}

/// Lines containing `query` (any case) under `within` (a file or folder of the
/// project), at most `MAX_MATCHES`; the flag says the search stopped there.
pub fn search_matches(root: &str, within: &str, query: &str) -> Result<(Vec<Match>, bool), String> {
    if query.is_empty() {
        return Err("query is empty".to_owned());
    }
    let start = if within.trim().is_empty() {
        std::path::Path::new(root.trim())
            .canonicalize()
            .map_err(|e| e.to_string())?
    } else {
        crate::files::resolve(root, within)?
    };
    let base = std::path::Path::new(root.trim())
        .canonicalize()
        .map_err(|e| e.to_string())?;
    let needle = query.to_lowercase();
    let mut found: Vec<Match> = Vec::new();
    let mut stack = vec![start];
    while let Some(path) = stack.pop() {
        if found.len() >= MAX_MATCHES {
            break;
        }
        if path.is_dir() {
            let skip = path
                .file_name()
                .is_some_and(|n| matches!(n.to_str(), Some(".git" | "node_modules" | "target")));
            if !skip {
                if let Ok(entries) = std::fs::read_dir(&path) {
                    // Links and junctions are skipped: they can lead out of the
                    // project, or back up into it and round again.
                    let mut children: Vec<_> = entries
                        .flatten()
                        .filter(|e| e.file_type().is_ok_and(|t| !t.is_symlink()))
                        .map(|e| e.path())
                        .collect();
                    // Reversed, so the stack visits them in name order.
                    children.sort();
                    children.reverse();
                    stack.extend(children);
                }
            }
            continue;
        }
        let big = std::fs::metadata(&path).map_or(true, |m| m.len() > MAX_SEARCH_BYTES);
        let Ok(bytes) = (if big {
            Err(())
        } else {
            std::fs::read(&path).map_err(drop)
        }) else {
            continue;
        };
        // Binary files have NULs early on.
        if bytes.iter().take(8192).any(|b| *b == 0) {
            continue;
        }
        let rel = path
            .strip_prefix(&base)
            .unwrap_or(&path)
            .to_string_lossy()
            .replace('\\', "/");
        for (i, line) in String::from_utf8_lossy(&bytes).lines().enumerate() {
            if line.to_lowercase().contains(&needle) {
                found.push(Match {
                    path: rel.clone(),
                    line: i + 1,
                    text: line.trim().chars().take(300).collect(),
                });
                if found.len() >= MAX_MATCHES {
                    break;
                }
            }
        }
    }
    let full = found.len() >= MAX_MATCHES;
    Ok((found, full))
}

/// `search_matches` as the agent reads it: one `path:line: text` per line.
fn search(root: &str, within: &str, query: &str) -> Result<String, String> {
    let (found, full) = search_matches(root, within, query)?;
    if found.is_empty() {
        return Ok(format!("no lines contain {query:?}"));
    }
    let mut lines: Vec<String> = found
        .iter()
        .map(|m| format!("{}:{}: {}", m.path, m.line, m.text))
        .collect();
    if full {
        lines.push(format!(
            "... stopped at {MAX_MATCHES} matches; search a narrower path"
        ));
    }
    Ok(lines.join("\n"))
}

#[async_trait::async_trait]
impl ToolHost for Workspace {
    fn specs(&self) -> Vec<ToolSpec> {
        let path = json!({"type": "string", "description": "Relative to the open folder, with / separators; or an absolute path inside a folder the user picked with choose_location."});
        let mut specs = vec![
            spec(
                "get_open_file",
                "The folder open in Workspace and the file the user is looking at. Call this first when the user says \"this file\" or \"this document\".",
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
                "find_symbol",
                "Where a function, type, class or constant is defined in the open project, and the lines that use it. Faster and more exact than search_files for code names.",
                json!({"name": {"type": "string", "description": "The name, e.g. run_agent or Composer. Exact first; otherwise names containing it."}}),
                &["name"],
            ),
            spec(
                "search_files",
                "Find lines containing some text (case-insensitive) in the open project, or in one file or folder of it. Returns path:line: text. Skips .git, node_modules and target. Understand-Anything's graph is .ua/knowledge-graph.json.",
                json!({"query": {"type": "string"}, "path": {"type": "string", "description": "File or folder to search; empty for the whole project."}}),
                &["query"],
            ),
            spec(
                "read_file",
                "Read a text file of the open project.",
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
                "create_folder",
                "Create a folder, and any missing folders above it, in the open project or a folder the user picked. Files need no folder made first: write_file creates them.",
                json!({"path": path}),
                &["path"],
            ),
            spec(
                "edit_file",
                "Replace one exact, unique piece of a text file. Prefer this to write_file for changes to existing files.",
                json!({"path": path, "old_text": {"type": "string"}, "new_text": {"type": "string"}}),
                &["path", "old_text", "new_text"],
            ),
            spec(
                "run_command",
                if self.bash.is_some() {
                    "Run a bash command (Git Bash) in the open project's folder and return its output. Use it to compile, run and test. No input can be typed into it."
                } else if cfg!(windows) {
                    "Run a shell command in the open project's folder (cmd.exe) and return its output. Use it to compile, run and test. No input can be typed into it."
                } else {
                    "Run a shell command in the open project's folder and return its output. No input can be typed into it."
                },
                json!({"command": {"type": "string"}}),
                &["command"],
            ),
        ];
        if self.chooser.is_some() {
            specs.push(spec(
                "choose_location",
                "Ask the user to pick a folder outside the open project, when the task needs files somewhere else. Say why in `reason`; it is shown to them. For a new project, give its folder name in `name`: the user then picks where it goes and only that new folder is made and given to you. If they pick one, you may create files and folders inside it.",
                json!({
                    "reason": {"type": "string"},
                    "name": {"type": "string", "description": "A new project's folder name, e.g. rubiks-solver. Omit to use the folder the user picks as it is."}
                }),
                &["reason"],
            ));
        }
        specs
    }

    fn needs_approval(&self, name: &str) -> bool {
        !matches!(
            name,
            // choose_location asks the user itself; a second prompt adds nothing.
            "get_open_file"
                | "list_dir"
                | "read_file"
                | "search_files"
                | "find_symbol"
                | "choose_location"
        )
    }

    /// The open project's outline, so the agent starts knowing where things are.
    fn context(&self) -> Option<String> {
        let root = self.folder()?;
        let defs = self.index.scan(Path::new(&root));
        if defs.is_empty() {
            return None;
        }
        Some(format!(
            "Outline of the open project ({root}): files and what they define. find_symbol gives exact lines.\n{}",
            crate::index::repo_map(&defs, OUTLINE_CHARS)
        ))
    }

    fn file_for(&self, name: &str, args: &Value) -> Option<std::path::PathBuf> {
        if !matches!(name, "write_file" | "edit_file") {
            return None;
        }
        let root = self.root().ok()?;
        let (base, rel) = self.place(&root, args.get("path")?.as_str()?).ok()?;
        crate::files::resolve_for_write(&base, &rel).ok()
    }

    async fn call(&self, name: &str, args: Value) -> ToolOutput {
        let root = match self.root() {
            Ok(r) => r,
            Err(e) if name == "get_open_file" => return ToolOutput::ok(e.text),
            // Asking for a place is how the agent gets one when none is open.
            Err(_) if name == "choose_location" => String::new(),
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
            "find_symbol" => match arg(&args, "name") {
                Ok(n) => {
                    let (index, root, name) = (self.index.clone(), root.clone(), n.to_owned());
                    tokio::task::spawn_blocking(move || find_symbol(&index, &root, &name))
                        .await
                        .unwrap_or_else(|e| Err(e.to_string()))
                }
                Err(e) => return e,
            },
            "search_files" => match arg(&args, "query") {
                Ok(q) => {
                    let (root, q) = (root.clone(), q.to_owned());
                    let within = args
                        .get("path")
                        .and_then(Value::as_str)
                        .unwrap_or("")
                        .to_owned();
                    tokio::task::spawn_blocking(move || search(&root, &within, &q))
                        .await
                        .unwrap_or_else(|e| Err(e.to_string()))
                }
                Err(e) => return e,
            },
            "list_dir" => {
                let dir = args.get("path").and_then(Value::as_str).unwrap_or("");
                // Outside the open folder, entries are shown by absolute path.
                let prefix = if Path::new(dir).is_absolute() {
                    match self.place(&root, dir) {
                        Ok((base, _)) => format!("{}/", base.replace('\\', "/")),
                        Err(e) => return ToolOutput::error(e),
                    }
                } else {
                    String::new()
                };
                self.place(&root, dir)
                    .and_then(|(base, rel)| crate::files::list_dir(&base, &rel))
                    .map(|entries| {
                        entries
                            .iter()
                            .map(|e| {
                                let slash = if e.is_dir { "/" } else { "" };
                                format!("{prefix}{}{slash}", e.path)
                            })
                            .collect::<Vec<_>>()
                            .join("\n")
                    })
            }
            "read_file" => match arg(&args, "path") {
                Ok(p) => self
                    .place(&root, p)
                    .and_then(|(base, rel)| crate::files::read_text(&base, &rel)),
                Err(e) => return e,
            },
            "write_file" => match (arg(&args, "path"), arg(&args, "content")) {
                (Ok(p), Ok(c)) => self
                    .place(&root, p)
                    .and_then(|(base, rel)| crate::files::write_text(&base, &rel, c))
                    .map(|()| format!("wrote {p}")),
                (Err(e), _) | (_, Err(e)) => return e,
            },
            "create_folder" => match arg(&args, "path") {
                Ok(p) => self
                    .place(&root, p)
                    .and_then(|(base, rel)| crate::files::create_dir(&base, &rel))
                    .map(|()| format!("created {p}")),
                Err(e) => return e,
            },
            "choose_location" => match arg(&args, "reason") {
                Ok(r) => {
                    self.choose(r, args.get("name").and_then(Value::as_str))
                        .await
                }
                Err(e) => return e,
            },
            "edit_file" => match (
                arg(&args, "path"),
                arg(&args, "old_text"),
                arg(&args, "new_text"),
            ) {
                (Ok(p), Ok(old), Ok(new)) => self.place(&root, p).and_then(|(base, rel)| {
                    crate::files::read_text(&base, &rel)
                        .and_then(|text| replace_once(&text, old, new))
                        .and_then(|text| crate::files::write_text(&base, &rel, &text))
                        .map(|()| format!("edited {p}"))
                }),
                (Err(e), _, _) | (_, Err(e), _) | (_, _, Err(e)) => return e,
            },
            "run_command" => match arg(&args, "command") {
                Ok(c) => {
                    return run_command(
                        &root,
                        c,
                        self.bash.as_deref(),
                        self.plugin_root.as_deref(),
                    )
                    .await;
                }
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
    fn search_finds_lines_in_any_case_and_skips_git_dependencies_and_binaries() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path();
        std::fs::create_dir_all(p.join("src")).unwrap();
        std::fs::create_dir_all(p.join("node_modules/x")).unwrap();
        std::fs::create_dir_all(p.join(".git")).unwrap();
        std::fs::write(
            p.join("src/a.rs"),
            "fn main() {}
  // TODO: Fix it
",
        )
        .unwrap();
        std::fs::write(
            p.join("node_modules/x/b.js"),
            "todo skipped
",
        )
        .unwrap();
        std::fs::write(
            p.join(".git/c"),
            "todo skipped
",
        )
        .unwrap();
        std::fs::write(p.join("bin.dat"), b"todo ").unwrap();
        let root = p.to_str().unwrap();

        let (found, full) = search_matches(root, "", "todo").unwrap();
        assert!(!full);
        assert_eq!(
            found,
            [Match {
                path: "src/a.rs".to_owned(),
                line: 2,
                text: "// TODO: Fix it".to_owned()
            }]
        );
        assert_eq!(
            search(root, "", "todo").unwrap(),
            "src/a.rs:2: // TODO: Fix it"
        );
        assert!(
            search(root, "", "zzz")
                .unwrap()
                .starts_with("no lines contain")
        );
        assert!(search(root, "", "").is_err(), "an empty query is refused");
        assert!(
            search_matches(root, "../x", "a").is_err(),
            "stays inside the project"
        );
    }

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

    #[test]
    fn search_does_not_follow_links_out_of_the_project() {
        let outside = tempfile::tempdir().unwrap();
        std::fs::write(
            outside.path().join("secret.txt"),
            "password=hunter2
",
        )
        .unwrap();
        let project = tempfile::tempdir().unwrap();
        std::fs::write(
            project.path().join("a.txt"),
            "password here too
",
        )
        .unwrap();
        let link = project.path().join("out");
        // A junction on Windows: the usual link there, and it needs no admin rights.
        #[cfg(windows)]
        let made = std::process::Command::new("cmd")
            .arg("/C")
            .arg("mklink")
            .arg("/J")
            .arg(&link)
            .arg(outside.path())
            .output()
            .is_ok_and(|o| o.status.success());
        #[cfg(not(windows))]
        let made = std::os::unix::fs::symlink(outside.path(), &link).is_ok();
        assert!(made, "could not create the link");
        let root = project.path().display().to_string();
        let found = search(&root, "", "password").unwrap();
        assert_eq!(found, "a.txt:1: password here too", "{found}");
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
        assert_eq!(
            ws.call("search_files", json!({"query": "TWO"})).await.text,
            "no lines contain \"TWO\"",
            "the edit replaced it"
        );
        assert_eq!(
            ws.call("search_files", json!({"query": "ONE"})).await.text,
            "src/a.txt:1: one"
        );
        assert!(!ws.needs_approval("search_files"));
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

    /// A chooser that answers with `answer` and records each reason it was given.
    fn chooser(answer: Option<PathBuf>, asked: Arc<Mutex<Vec<String>>>) -> Chooser {
        Chooser(Arc::new(move |reason| {
            asked.lock().unwrap().push(reason);
            let answer = answer.clone();
            Box::pin(async move { answer })
        }))
    }

    #[tokio::test]
    async fn the_agent_writes_outside_the_open_folder_only_where_the_user_chose() {
        let (project, elsewhere) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
        let target = elsewhere.path().join("new-app");
        let file = target.join("src").join("main.rs");
        let abs = |p: &std::path::Path| p.display().to_string();

        // Without a way to ask, an absolute path is refused and the tool is not offered.
        let plain = Workspace::new(None);
        plain.set(Some(abs(project.path())), None);
        assert!(!plain.specs().iter().any(|t| t.name == "choose_location"));
        let out = plain
            .call("write_file", json!({"path": abs(&file), "content": "x"}))
            .await;
        assert!(out.is_error && !file.exists(), "{}", out.text);

        let asked = Arc::new(Mutex::new(Vec::new()));
        let ws = Workspace::new(None).with_chooser(chooser(Some(target.clone()), asked.clone()));
        ws.set(Some(abs(project.path())), None);
        assert!(
            !ws.needs_approval("choose_location"),
            "picking the folder is the approval"
        );
        assert!(ws.needs_approval("create_folder"));

        let out = ws
            .call("create_folder", json!({"path": abs(&target.join("docs"))}))
            .await;
        assert!(
            out.is_error && out.text.contains("choose_location"),
            "{}",
            out.text
        );

        let out = ws
            .call(
                "choose_location",
                json!({"reason": "where the new app goes"}),
            )
            .await;
        assert!(
            !out.is_error && out.text.contains(&abs(&target)),
            "{}",
            out.text
        );
        assert_eq!(*asked.lock().unwrap(), ["where the new app goes"]);

        let out = ws
            .call(
                "write_file",
                json!({"path": abs(&file), "content": "fn main() {}\n"}),
            )
            .await;
        assert!(!out.is_error, "{}", out.text);
        assert_eq!(std::fs::read_to_string(&file).unwrap(), "fn main() {}\n");
        assert!(
            ws.file_for("write_file", &json!({"path": abs(&file)}))
                .is_some(),
            "undo can snapshot it"
        );
        let out = ws
            .call("create_folder", json!({"path": abs(&target.join("docs"))}))
            .await;
        assert!(
            !out.is_error && target.join("docs").is_dir(),
            "{}",
            out.text
        );
        let listed = ws.call("list_dir", json!({"path": abs(&target)})).await;
        assert!(listed.text.contains("/new-app/docs/"), "{}", listed.text);

        // Next to the chosen folder, or climbing out of it, is still refused.
        for outside in [
            abs(&elsewhere.path().join("sibling.txt")),
            format!("{}/../escape.txt", abs(&target)),
        ] {
            let out = ws
                .call("write_file", json!({"path": outside, "content": "x"}))
                .await;
            assert!(out.is_error, "{outside}: {}", out.text);
        }
        assert!(!elsewhere.path().join("sibling.txt").exists());
        assert!(!elsewhere.path().join("escape.txt").exists());
    }

    #[tokio::test]
    async fn declining_to_choose_a_folder_allows_nothing() {
        let (project, elsewhere) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
        let ws = Workspace::new(None).with_chooser(chooser(None, Arc::default()));
        ws.set(Some(project.path().display().to_string()), None);
        let out = ws.call("choose_location", json!({"reason": "x"})).await;
        assert!(
            !out.is_error && out.text.contains("did not choose"),
            "{}",
            out.text
        );
        let file = elsewhere.path().join("a.txt");
        let out = ws
            .call(
                "write_file",
                json!({"path": file.display().to_string(), "content": "x"}),
            )
            .await;
        assert!(out.is_error && !file.exists());
    }

    #[tokio::test]
    async fn with_no_folder_open_the_agent_asks_and_the_chosen_folder_becomes_its_working_folder() {
        let parent = tempfile::tempdir().unwrap();
        let target = parent.path().join("cube-solver");
        let ws = Workspace::new(None).with_chooser(chooser(Some(target.clone()), Arc::default()));

        let out = ws.call("list_dir", json!({"path": "."})).await;
        assert!(
            out.is_error && out.text.contains("choose_location"),
            "{}",
            out.text
        );

        let out = ws
            .call("choose_location", json!({"reason": "a new Rust project"}))
            .await;
        assert!(
            !out.is_error && out.text.contains("working folder"),
            "{}",
            out.text
        );
        assert!(target.is_dir(), "the chosen folder is made if missing");

        let out = ws
            .call(
                "write_file",
                json!({"path": "src/main.rs", "content": "fn main() {}\n"}),
            )
            .await;
        assert!(!out.is_error, "{}", out.text);
        assert!(target.join("src").join("main.rs").is_file());
        assert_eq!(ws.folder(), Some(target.display().to_string()));
    }

    #[tokio::test]
    async fn a_named_project_is_made_inside_the_picked_folder_and_only_it_is_given() {
        let desktop = tempfile::tempdir().unwrap();
        let asked = Arc::new(Mutex::new(Vec::new()));
        let ws = Workspace::new(None)
            .with_chooser(chooser(Some(desktop.path().to_owned()), asked.clone()));
        let out = ws
            .call(
                "choose_location",
                json!({"reason": "a cube solver", "name": "rubiks-solver"}),
            )
            .await;
        assert!(!out.is_error, "{}", out.text);
        let project = desktop.path().join("rubiks-solver");
        assert!(project.is_dir());
        assert_eq!(
            ws.folder(),
            Some(project.display().to_string()),
            "not the whole desktop"
        );
        assert!(asked.lock().unwrap()[0].contains("rubiks-solver"));
        // The desktop itself is still out of reach.
        let out = ws
            .call(
                "write_file",
                json!({"path": desktop.path().join("x.txt").display().to_string(), "content": "x"}),
            )
            .await;
        assert!(out.is_error);

        // Picking a folder already named for the project uses it as it is.
        let made = desktop.path().join("Cube");
        let ws = Workspace::new(None).with_chooser(chooser(Some(made.clone()), Arc::default()));
        ws.call("choose_location", json!({"reason": "x", "name": "cube"}))
            .await;
        assert_eq!(ws.folder(), Some(made.display().to_string()));

        let out = Workspace::new(None)
            .with_chooser(chooser(Some(made), Arc::default()))
            .call("choose_location", json!({"reason": "x", "name": "../up"}))
            .await;
        assert!(out.is_error, "a name cannot climb out: {}", out.text);
    }

    #[tokio::test]
    async fn find_symbol_gives_definitions_then_uses_and_the_outline_names_them() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("src")).unwrap();
        std::fs::write(
            dir.path().join("src/cube.rs"),
            "pub fn solve(c: &Cube) {}\npub struct Cube;\n",
        )
        .unwrap();
        std::fs::write(
            dir.path().join("src/main.rs"),
            "fn main() {\n    solve(&c);\n    let resolve = 1;\n}\n",
        )
        .unwrap();
        let ws = Workspace::new(None);
        assert!(ws.context().is_none(), "no folder, no outline");
        ws.set(Some(dir.path().display().to_string()), None);

        let out = ws.call("find_symbol", json!({"name": "solve"})).await;
        assert!(!out.is_error, "{}", out.text);
        assert!(out.text.contains("src/cube.rs:1 fn solve"), "{}", out.text);
        assert!(
            out.text.contains("src/main.rs:2: solve(&c);"),
            "{}",
            out.text
        );
        assert!(
            !out.text.contains("resolve"),
            "whole words only: {}",
            out.text
        );

        let partial = ws.call("find_symbol", json!({"name": "cub"})).await;
        assert!(partial.text.contains("struct Cube"), "{}", partial.text);
        let none = ws
            .call("find_symbol", json!({"name": "nothing_here"}))
            .await;
        assert!(none.text.contains("No definition"), "{}", none.text);
        assert!(!ws.needs_approval("find_symbol"));

        let outline = ws.context().unwrap();
        assert!(
            outline.contains("src/cube.rs: fn solve, struct Cube"),
            "{outline}"
        );
        assert!(outline.contains("src/main.rs: fn main"), "{outline}");
    }
}
