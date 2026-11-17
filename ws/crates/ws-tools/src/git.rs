//! Git for the side panel and the editor: what changed, a file's diff, the
//! changed lines of the text being edited, staging (a file or one change),
//! committing, and blame.

use serde::Serialize;
use std::collections::HashMap;
use std::io::Write as _;
use std::process::{Command, Stdio};

/// A diff longer than this is cut, so one rewritten file cannot flood the panel.
const MAX_DIFF_CHARS: usize = 400_000;

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Change {
    /// Relative to the project root, with `/` separators.
    pub path: String,
    /// One letter: M modified, A added, D deleted, R renamed, ? untracked.
    pub status: char,
    /// Some of it is staged for the next commit.
    pub staged: bool,
    /// Some of it is not staged (untracked files count).
    pub unstaged: bool,
}

fn git(root: &str, args: &[&str]) -> Result<String, String> {
    git_with_input(root, args, None)
}

fn git_with_input(root: &str, args: &[&str], input: Option<&[u8]>) -> Result<String, String> {
    run(root, args, input, &[])
}

/// Run git in `root` with `env` set, feeding it `input`; its stdout, or its
/// stderr as the error.
pub(crate) fn run(
    root: &str,
    args: &[&str],
    input: Option<&[u8]>,
    env: &[(&str, &str)],
) -> Result<String, String> {
    let program = crate::find(&["git"]).ok_or("git is not installed or not on PATH")?;
    let mut cmd = Command::new(program);
    cmd.args(args)
        .envs(env.iter().copied())
        .current_dir(root.trim())
        .stdin(if input.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    #[cfg(windows)]
    std::os::windows::process::CommandExt::creation_flags(&mut cmd, 0x0800_0000);
    let mut child = cmd.spawn().map_err(|e| e.to_string())?;
    if let (Some(bytes), Some(mut stdin)) = (input, child.stdin.take()) {
        stdin.write_all(bytes).map_err(|e| e.to_string())?;
    }
    let out = child.wait_with_output().map_err(|e| e.to_string())?;
    if out.status.success() {
        Ok(String::from_utf8_lossy(&out.stdout).into_owned())
    } else {
        Err(String::from_utf8_lossy(&out.stderr).trim().to_owned())
    }
}

/// Files that differ from the last commit, with untracked ones, by path.
///
/// # Errors
/// Fails if `root` is not in a git repository or git cannot run.
pub fn status(root: &str) -> Result<Vec<Change>, String> {
    // -z: NUL-separated and unquoted, so odd file names survive.
    let out = git(
        root,
        &["status", "--porcelain=v1", "-z", "--untracked-files=all"],
    )?;
    let mut parts = out.split('\0').filter(|p| !p.is_empty());
    let mut changes = Vec::new();
    while let Some(entry) = parts.next() {
        let mut chars = entry.chars();
        let (Some(x), Some(y)) = (chars.next(), chars.next()) else {
            continue;
        };
        // A rename or copy is followed by the path it came from.
        if matches!(x, 'R' | 'C') {
            parts.next();
        }
        let status = match (x, y) {
            ('?', _) => '?',
            (' ', y) => y,
            (x, _) => x,
        };
        changes.push(Change {
            path: entry.get(3..).unwrap_or_default().to_owned(),
            status,
            staged: !matches!(x, ' ' | '?'),
            unstaged: y != ' ',
        });
    }
    changes.sort_by(|a, b| a.path.cmp(&b.path));
    Ok(changes)
}

/// One file's change since the last commit, as a unified diff.
///
/// # Errors
/// Fails if the path leaves the project, or git cannot run.
pub fn diff(root: &str, path: &str) -> Result<String, String> {
    crate::files::resolve_for_write(root, path)?;
    let tracked = git(root, &["ls-files", "--error-unmatch", "--", path]).is_ok();
    let mut text = if tracked {
        // shortcut: a repo with no commits has no HEAD, so it shows only unstaged changes.
        git(root, &["diff", "HEAD", "--", path]).or_else(|_| git(root, &["diff", "--", path]))?
    } else {
        let body = crate::files::read_text(root, path)?;
        let lines: String = body.lines().map(|l| format!("+{l}\n")).collect();
        format!("new file {path}\n{lines}")
    };
    if text.trim().is_empty() {
        text = "no changes".to_owned();
    }
    if text.len() > MAX_DIFF_CHARS {
        let cut = (0..=MAX_DIFF_CHARS)
            .rev()
            .find(|i| text.is_char_boundary(*i))
            .unwrap_or(0);
        text.truncate(cut);
        text.push_str("\n… diff truncated\n");
    }
    Ok(text)
}

/// A run of changed lines in the text being edited, against the staged copy.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Hunk {
    /// `add`, `change` or `delete`.
    pub kind: &'static str,
    /// The first line in the edited text (1-based). For `delete`, the line
    /// the removed ones were above (0: they were at the very top).
    pub line: usize,
    /// Lines in the edited text; 0 for `delete`.
    pub count: usize,
}

/// `text` cut into lines that keep their endings, and the same lines without
/// them for comparing, so a CRLF file matches its LF copy in git.
fn lines(text: &str) -> (Vec<&str>, Vec<&str>) {
    let kept: Vec<&str> = text.split_inclusive('\n').collect();
    let bare = kept
        .iter()
        .map(|l| l.trim_end_matches(['\n', '\r']))
        .collect();
    (kept, bare)
}

/// The staged copy of `path`, or None when git has none (untracked).
fn staged_text(root: &str, path: &str) -> Option<String> {
    git(root, &["show", &format!(":{path}")]).ok()
}

/// The changes from `base` to `text`, as (base range, text range) per change.
fn changes(base: &str, text: &str) -> Vec<(std::ops::Range<usize>, std::ops::Range<usize>)> {
    let ((_, old), (_, new)) = (lines(base), lines(text));
    similar::capture_diff_slices(similar::Algorithm::Myers, &old, &new)
        .into_iter()
        .filter(|op| !matches!(op, similar::DiffOp::Equal { .. }))
        .map(|op| (op.old_range(), op.new_range()))
        .collect()
}

/// Which lines of `text` (the editor's, saved or not) differ from the staged
/// copy of `path`. An untracked file has none: it is all new.
///
/// # Errors
/// The path leaves the project, or git cannot run there.
pub fn hunks(root: &str, path: &str, text: &str) -> Result<Vec<Hunk>, String> {
    crate::files::resolve_for_write(root, path)?;
    let Some(base) = staged_text(root, path) else {
        return Ok(Vec::new());
    };
    Ok(changes(&base, text)
        .into_iter()
        .map(|(old, new)| match (old.is_empty(), new.is_empty()) {
            (true, _) => Hunk {
                kind: "add",
                line: new.start + 1,
                count: new.len(),
            },
            (_, true) => Hunk {
                kind: "delete",
                line: new.start,
                count: 0,
            },
            _ => Hunk {
                kind: "change",
                line: new.start + 1,
                count: new.len(),
            },
        })
        .collect())
}

/// Stage the `index`th change (as `hunks` lists them) of `text`: the staged
/// copy gets just that change, in its own line endings; nothing else moves.
///
/// # Errors
/// The change is not there any more, or git refuses.
pub fn stage_hunk(root: &str, path: &str, text: &str, index: usize) -> Result<(), String> {
    crate::files::resolve_for_write(root, path)?;
    let base = staged_text(root, path).ok_or("the file is not tracked; stage the whole file")?;
    let (old, new) = changes(&base, text)
        .into_iter()
        .nth(index)
        .ok_or("that change is no longer there; look again")?;
    let ((base_lines, _), (text_lines, _)) = (lines(&base), lines(text));
    let eol = if base.contains("\r\n") { "\r\n" } else { "\n" };
    let mut staged = String::with_capacity(base.len() + 256);
    for l in &base_lines[..old.start] {
        staged.push_str(l);
    }
    for l in &text_lines[new] {
        // In the staged copy's line endings; a last line without one stays so.
        let bare = l.trim_end_matches(['\n', '\r']);
        staged.push_str(bare);
        if bare.len() < l.len() {
            staged.push_str(eol);
        }
    }
    for l in &base_lines[old.end..] {
        staged.push_str(l);
    }
    let sha = git_with_input(
        root,
        &["hash-object", "-w", "--stdin"],
        Some(staged.as_bytes()),
    )?;
    let mode = git(root, &["ls-files", "-s", "--", path])?
        .split_whitespace()
        .next()
        .unwrap_or("100644")
        .to_owned();
    git(
        root,
        &[
            "update-index",
            "--cacheinfo",
            &format!("{mode},{},{path}", sha.trim()),
        ],
    )
    .map(drop)
}

/// Stage the whole file (or its deletion).
///
/// # Errors
/// git refuses.
pub fn stage(root: &str, path: &str) -> Result<(), String> {
    git(root, &["add", "-A", "--", path]).map(drop)
}

/// Take the file out of the next commit, keeping its changes on disk.
///
/// # Errors
/// git refuses.
pub fn unstage(root: &str, path: &str) -> Result<(), String> {
    // A repo with no commits yet has no HEAD to restore from.
    git(root, &["restore", "--staged", "--", path])
        .or_else(|_| git(root, &["rm", "--cached", "-q", "--", path]))
        .map(drop)
}

/// Commit what is staged; returns the new commit's short id.
///
/// # Errors
/// The message is empty, nothing is staged, or git refuses (no name or email
/// set, a hook failed); git's own words say which.
pub fn commit(root: &str, message: &str) -> Result<String, String> {
    if message.trim().is_empty() {
        return Err("write a commit message first".to_owned());
    }
    git(root, &["commit", "-q", "-m", message])?;
    Ok(git(root, &["rev-parse", "--short", "HEAD"])?
        .trim()
        .to_owned())
}

/// Who last changed one line, and when.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct BlameLine {
    /// Short commit id; all zeros for lines not committed yet.
    pub commit: String,
    pub author: String,
    /// Seconds since 1970.
    pub time: i64,
    pub summary: String,
    /// The line as it was blamed, so the editor shows blame only where the
    /// line is still the same.
    pub text: String,
}

/// Blame for every line of the saved file, in order.
///
/// # Errors
/// The file is not tracked, or git cannot run.
pub fn blame(root: &str, path: &str) -> Result<Vec<BlameLine>, String> {
    crate::files::resolve_for_write(root, path)?;
    let out = git(root, &["blame", "--porcelain", "--", path])?;
    let mut known: HashMap<String, BlameLine> = HashMap::new();
    let mut lines = Vec::new();
    let mut current: Option<String> = None;
    for line in out.lines() {
        if let Some(text) = line.strip_prefix('\t') {
            // The line's own text ends its entry.
            if let Some(sha) = current.take() {
                let mut entry = known.get(&sha).cloned().unwrap_or(BlameLine {
                    commit: sha.chars().take(8).collect(),
                    author: String::new(),
                    time: 0,
                    summary: String::new(),
                    text: String::new(),
                });
                entry.text = text.trim_end_matches('\r').to_owned();
                lines.push(entry);
            }
            continue;
        }
        let mut words = line.splitn(2, ' ');
        let (key, rest) = (words.next().unwrap_or(""), words.next().unwrap_or(""));
        if key.len() == 40 && key.chars().all(|c| c.is_ascii_hexdigit()) {
            known.entry(key.to_owned()).or_insert_with(|| BlameLine {
                commit: key.chars().take(8).collect(),
                author: String::new(),
                time: 0,
                summary: String::new(),
                text: String::new(),
            });
            current = Some(key.to_owned());
        } else if let Some(entry) = current.as_ref().and_then(|c| known.get_mut(c)) {
            match key {
                "author" => entry.author = rest.to_owned(),
                "author-time" => entry.time = rest.parse().unwrap_or(0),
                "summary" => entry.summary = rest.to_owned(),
                _ => {}
            }
        }
    }
    Ok(lines)
}

#[cfg(test)]
mod tests {
    // Tests may unwrap and expect: a panic is the failure report.
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;

    /// A repo with `a.txt` and `gone.txt` committed; None when git is missing.
    fn repo() -> Option<tempfile::TempDir> {
        crate::find(&["git"])?;
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().to_str().unwrap();
        std::fs::write(dir.path().join("a.txt"), "one\ntwo\n").unwrap();
        std::fs::write(dir.path().join("gone.txt"), "bye\n").unwrap();
        for args in [
            &["init", "-q"][..],
            &["config", "user.email", "t@t"],
            &["config", "user.name", "t"],
            &["config", "commit.gpgsign", "false"],
            &["add", "."],
            &["commit", "-q", "-m", "first"],
        ] {
            git(root, args).unwrap();
        }
        Some(dir)
    }

    #[test]
    fn status_lists_modified_deleted_and_untracked_files() {
        let Some(dir) = repo() else { return };
        let root = dir.path().to_str().unwrap();
        std::fs::write(dir.path().join("a.txt"), "one\nTWO\n").unwrap();
        std::fs::remove_file(dir.path().join("gone.txt")).unwrap();
        std::fs::create_dir(dir.path().join("sub")).unwrap();
        std::fs::write(dir.path().join("sub/new file.txt"), "hi\n").unwrap();

        let got: Vec<_> = status(root)
            .unwrap()
            .into_iter()
            .map(|c| (c.path, c.status))
            .collect();
        assert_eq!(
            got,
            [
                ("a.txt".to_owned(), 'M'),
                ("gone.txt".to_owned(), 'D'),
                ("sub/new file.txt".to_owned(), '?'),
            ]
        );
    }

    #[test]
    fn diff_shows_a_change_a_new_file_and_a_clean_file() {
        let Some(dir) = repo() else { return };
        let root = dir.path().to_str().unwrap();
        std::fs::write(dir.path().join("a.txt"), "one\nTWO\n").unwrap();
        std::fs::write(dir.path().join("new.txt"), "x\ny\n").unwrap();

        let changed = diff(root, "a.txt").unwrap();
        assert!(
            changed.contains("-two") && changed.contains("+TWO"),
            "{changed}"
        );
        let added = diff(root, "new.txt").unwrap();
        assert!(added.contains("+x\n+y"), "{added}");
        assert_eq!(diff(root, "gone.txt").unwrap(), "no changes");
        assert!(
            diff(root, "../outside").is_err(),
            "stays inside the project"
        );
    }

    #[test]
    fn a_folder_that_is_not_a_repository_says_so() {
        if crate::find(&["git"]).is_none() {
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        let err = status(dir.path().to_str().unwrap()).unwrap_err();
        assert!(err.to_lowercase().contains("not a git repository"), "{err}");
    }

    #[test]
    fn hunks_compare_the_editor_text_with_the_staged_copy() {
        let Some(dir) = repo() else { return };
        let root = dir.path().to_str().unwrap();
        // CRLF in the editor against LF in git is not a change.
        assert_eq!(hunks(root, "a.txt", "one\r\ntwo\r\n").unwrap(), []);
        assert_eq!(
            hunks(root, "a.txt", "zero\none\n2\n").unwrap(),
            [
                Hunk {
                    kind: "add",
                    line: 1,
                    count: 1
                },
                Hunk {
                    kind: "change",
                    line: 3,
                    count: 1
                }
            ]
        );
        assert_eq!(
            hunks(root, "a.txt", "two\n").unwrap(),
            [Hunk {
                kind: "delete",
                line: 0,
                count: 0
            }]
        );
        std::fs::write(dir.path().join("new.txt"), "x\n").unwrap();
        assert_eq!(
            hunks(root, "new.txt", "x\ny\n").unwrap(),
            [],
            "untracked: all new, no marks"
        );
        assert!(hunks(root, "../outside.txt", "x").is_err());
    }

    #[test]
    fn staging_one_change_leaves_the_others_and_keeps_line_endings() {
        let Some(dir) = repo() else { return };
        let root = dir.path().to_str().unwrap();
        std::fs::write(dir.path().join("crlf.txt"), "a\r\nb\r\nc\r\n").unwrap();
        stage(root, "crlf.txt").unwrap();
        // Git may have stored it with LF (core.autocrlf); the staged copy keeps
        // whichever endings it has.
        let before = git(root, &["show", ":crlf.txt"]).unwrap();
        let eol = if before.contains("\r\n") {
            "\r\n"
        } else {
            "\n"
        };
        let edited = "A\r\nb\r\nC\r\n";
        assert_eq!(hunks(root, "crlf.txt", edited).unwrap().len(), 2);
        stage_hunk(root, "crlf.txt", edited, 1).unwrap();
        assert_eq!(
            git(root, &["show", ":crlf.txt"]).unwrap(),
            format!("a{eol}b{eol}C{eol}"),
            "only the second change, in the staged copy's endings"
        );
        assert_eq!(
            hunks(root, "crlf.txt", edited).unwrap(),
            [Hunk {
                kind: "change",
                line: 1,
                count: 1
            }],
            "the staged change's mark is gone"
        );
        assert!(
            stage_hunk(root, "crlf.txt", edited, 5).is_err(),
            "a change that is not there"
        );
    }

    #[test]
    fn stage_unstage_commit_and_blame() {
        let Some(dir) = repo() else { return };
        let root = dir.path().to_str().unwrap();
        std::fs::write(dir.path().join("a.txt"), "one\nTWO\n").unwrap();
        std::fs::write(dir.path().join("new.txt"), "x\n").unwrap();
        let find = |p: &str| {
            status(root)
                .unwrap()
                .into_iter()
                .find(|c| c.path == p)
                .unwrap()
        };
        assert!(!find("a.txt").staged && find("a.txt").unstaged);
        stage(root, "a.txt").unwrap();
        stage(root, "new.txt").unwrap();
        assert!(find("a.txt").staged && !find("a.txt").unstaged);
        unstage(root, "new.txt").unwrap();
        assert!(!find("new.txt").staged, "back to untracked, file kept");
        assert!(dir.path().join("new.txt").exists());

        assert!(commit(root, "  ").is_err(), "a message is needed");
        let id = commit(root, "Shout two").unwrap();
        assert!(!id.is_empty());
        let blamed = blame(root, "a.txt").unwrap();
        assert_eq!(blamed.len(), 2);
        assert_eq!(blamed[0].summary, "first");
        assert_eq!(blamed[1].summary, "Shout two");
        assert_eq!(blamed[1].author, "t");
        assert_eq!(
            blamed[1].text, "TWO",
            "each line's own text, to match against the editor"
        );
        assert!(blamed[1].time > 0);
        assert!(commit(root, "nothing staged").is_err());
    }
}
