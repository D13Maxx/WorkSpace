//! Read-only git for the side panel: what changed, and one file's diff.

use serde::Serialize;
use std::process::{Command, Stdio};

/// A diff longer than this is cut, so one rewritten file cannot flood the panel.
const MAX_DIFF_CHARS: usize = 400_000;

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Change {
    /// Relative to the project root, with `/` separators.
    pub path: String,
    /// One letter: M modified, A added, D deleted, R renamed, ? untracked.
    pub status: char,
}

fn git(root: &str, args: &[&str]) -> Result<String, String> {
    let program = crate::find(&["git"]).ok_or("git is not installed or not on PATH")?;
    let mut cmd = Command::new(program);
    cmd.args(args).current_dir(root.trim()).stdin(Stdio::null());
    #[cfg(windows)]
    std::os::windows::process::CommandExt::creation_flags(&mut cmd, 0x0800_0000);
    let out = cmd.output().map_err(|e| e.to_string())?;
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

#[cfg(test)]
mod tests {
    // Spec s14 permits unwrap/expect in tests.
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
}
