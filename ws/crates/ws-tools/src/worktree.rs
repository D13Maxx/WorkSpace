//! A private copy of the project for each helper agent: a git worktree on its
//! own branch, kept inside `.git` so it never shows among the project's files.
//! Its work reaches the user's files only when they merge it.

use crate::git::run;
use serde::Serialize;
use std::path::Path;

/// Commits made here are the app's, not the user's: git refuses to commit
/// without a name, and the user may have none set.
const IDENTITY: [&str; 4] = [
    "-c",
    "user.name=Workspace",
    "-c",
    "user.email=workspace@localhost",
];

/// A diff longer than this is cut, so one rewritten file cannot flood the pane.
const MAX_DIFF_CHARS: usize = 400_000;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Worktree {
    /// Where the helper works: the worktree's copy of the open folder.
    pub folder: String,
    /// The worktree's top folder, where its work is committed.
    pub top: String,
    pub branch: String,
    /// The snapshot of the project it started from.
    pub base: String,
}

/// One changed file; the counts are `None` for a binary file.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Changed {
    pub path: String,
    pub added: Option<u32>,
    pub removed: Option<u32>,
}

fn git(root: &str, args: &[&str]) -> Result<String, String> {
    run(root, args, None, &[]).map(|s| s.trim().to_owned())
}

/// Make a worktree for helper `id`, starting from the project as it is now,
/// uncommitted and new files included. `None` when `root` is not in a git
/// repository (or git is missing): the helper then works in the project.
///
/// # Errors
/// Git fails partway, for example the branch already exists.
pub fn create(root: &str, id: &str) -> Result<Option<Worktree>, String> {
    let Ok(top) = git(root, &["rev-parse", "--show-toplevel"]) else {
        return Ok(None);
    };
    // The open folder's place in the repository, as "sub/dir/" or "".
    let prefix = git(root, &["rev-parse", "--show-prefix"])?;
    let base = snapshot(&top)?;
    let common = git(
        &top,
        &["rev-parse", "--path-format=absolute", "--git-common-dir"],
    )?;
    let name: String = id
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' {
                c
            } else {
                '-'
            }
        })
        .take(48)
        .collect();
    let path = Path::new(&common).join("ws-worktrees").join(&name);
    let path = path.to_string_lossy().into_owned();
    let branch = format!("ws/{name}");
    git(
        &top,
        &["worktree", "add", "-q", "-b", &branch, &path, &base],
    )?;
    let folder = Path::new(&path)
        .join(&prefix)
        .to_string_lossy()
        .into_owned();
    Ok(Some(Worktree {
        folder,
        top: path,
        branch,
        base,
    }))
}

/// A commit of the working files as they are, without touching the user's
/// index or branch: through a throwaway index file.
fn snapshot(top: &str) -> Result<String, String> {
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_nanos());
    let index = std::env::temp_dir().join(format!("ws-index-{}-{stamp}", std::process::id()));
    let index_path = index.to_string_lossy().into_owned();
    let env = [("GIT_INDEX_FILE", index_path.as_str())];
    let head = git(top, &["rev-parse", "--verify", "-q", "HEAD"]).ok();
    let tree = (|| {
        if head.is_some() {
            run(top, &["read-tree", "HEAD"], None, &env)?;
        }
        // Respects .gitignore, so build output and dependencies stay out.
        run(top, &["add", "-A"], None, &env)?;
        run(top, &["write-tree"], None, &env).map(|t| t.trim().to_owned())
    })();
    let _ = std::fs::remove_file(&index);
    let tree = tree?;
    if let Some(head) = &head {
        if git(top, &["rev-parse", "HEAD^{tree}"]).is_ok_and(|t| t == tree) {
            return Ok(head.clone());
        }
    }
    let mut args: Vec<&str> = IDENTITY.to_vec();
    args.extend([
        "commit-tree",
        &tree,
        "-m",
        "Workspace: the project when a helper started",
    ]);
    if let Some(head) = &head {
        args.extend(["-p", head]);
    }
    git(top, &args)
}

/// Commit everything the helper left in its worktree. Returns whether there
/// was anything to commit.
///
/// # Errors
/// Git fails.
pub fn seal(top: &str, message: &str) -> Result<bool, String> {
    git(top, &["add", "-A"])?;
    if git(top, &["status", "--porcelain"])?.is_empty() {
        return Ok(false);
    }
    let mut args: Vec<&str> = IDENTITY.to_vec();
    // Hooks are the user's checks for their own commits, not a helper's.
    args.extend(["commit", "-q", "--no-verify", "-m", message]);
    git(top, &args).map(|_| true)
}

fn exists(root: &str, branch: &str) -> bool {
    git(
        root,
        &[
            "rev-parse",
            "--verify",
            "-q",
            &format!("refs/heads/{branch}"),
        ],
    )
    .is_ok()
}

/// What the helper changed, or `None` once it has been merged or discarded.
///
/// # Errors
/// Git fails.
pub fn changes(root: &str, base: &str, branch: &str) -> Result<Option<Vec<Changed>>, String> {
    if !exists(root, branch) {
        return Ok(None);
    }
    let out = run(
        root,
        &["diff", "--numstat", "-z", "--no-renames", base, branch],
        None,
        &[],
    )?;
    // -z: "added\tremoved\tpath\0", with "-" counts for binary files.
    Ok(Some(
        out.split('\0')
            .filter_map(|record| {
                let mut f = record.splitn(3, '\t');
                let (added, removed, path) = (f.next()?, f.next()?, f.next()?);
                Some(Changed {
                    path: path.to_owned(),
                    added: added.parse().ok(),
                    removed: removed.parse().ok(),
                })
            })
            .collect(),
    ))
}

/// The helper's whole change, as a unified diff.
///
/// # Errors
/// Git fails.
pub fn diff(root: &str, base: &str, branch: &str) -> Result<String, String> {
    let mut text = run(root, &["diff", base, branch], None, &[])?;
    if text.len() > MAX_DIFF_CHARS {
        let mut cut = MAX_DIFF_CHARS;
        while !text.is_char_boundary(cut) {
            cut -= 1;
        }
        text.truncate(cut);
        text.push_str("\n[diff cut short]");
    }
    Ok(text)
}

/// Put the helper's changes into the user's files, uncommitted, then remove
/// its worktree. A clean apply first; if the user changed the same lines
/// meanwhile, a three-way merge that may leave conflict markers to resolve.
///
/// # Errors
/// The changes cannot be applied even with a three-way merge; nothing is
/// changed then, and the worktree is kept.
pub fn merge(root: &str, base: &str, branch: &str) -> Result<String, String> {
    let top = git(root, &["rev-parse", "--show-toplevel"])?;
    let patch = run(&top, &["diff", "--binary", base, branch], None, &[])?;
    let note = if patch.is_empty() {
        "There was nothing to merge.".to_owned()
    } else if run(
        &top,
        &["apply", "--whitespace=nowarn"],
        Some(patch.as_bytes()),
        &[],
    )
    .is_ok()
    {
        "Merged into your files. Review and commit them when ready.".to_owned()
    } else {
        run(
            &top,
            &["apply", "--3way", "--whitespace=nowarn"],
            Some(patch.as_bytes()),
            &[],
        )
        .map_err(|e| format!("Could not merge: {e}"))?;
        "Merged with a three-way merge. Look for conflict markers where you had changed the same lines.".to_owned()
    };
    discard(root, branch)?;
    Ok(note)
}

/// Remove the helper's worktree and branch.
///
/// # Errors
/// Git fails.
pub fn discard(root: &str, branch: &str) -> Result<(), String> {
    let top = git(root, &["rev-parse", "--show-toplevel"])?;
    let list = git(&top, &["worktree", "list", "--porcelain"])?;
    let want = format!("branch refs/heads/{branch}");
    // Blocks of "worktree <path>", "HEAD <id>", "branch refs/heads/<name>".
    let path = list
        .split("\n\n")
        .find(|block| block.lines().any(|l| l == want))
        .and_then(|block| block.lines().next()?.strip_prefix("worktree "));
    if let Some(path) = path {
        git(&top, &["worktree", "remove", "--force", path])?;
    }
    if exists(&top, branch) {
        git(&top, &["branch", "-D", branch])?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    // Tests may unwrap and expect: a panic is the failure report.
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;

    /// A repo with `a.txt` committed; None when git is missing.
    fn repo() -> Option<tempfile::TempDir> {
        crate::find(&["git"])?;
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().to_str().unwrap();
        std::fs::write(dir.path().join("a.txt"), "one\ntwo\n").unwrap();
        for args in [
            &["init", "-q"][..],
            &["config", "user.email", "t@t"],
            &["config", "user.name", "t"],
            &["config", "commit.gpgsign", "false"],
            &["config", "core.autocrlf", "false"],
            &["add", "."],
            &["commit", "-q", "-m", "first"],
        ] {
            git(root, args).unwrap();
        }
        Some(dir)
    }

    fn read(dir: &str, file: &str) -> String {
        std::fs::read_to_string(Path::new(dir).join(file)).unwrap()
    }

    #[test]
    fn a_helper_starts_from_the_files_as_they_are_and_its_work_merges_in() {
        let Some(dir) = repo() else { return };
        let root = dir.path().to_str().unwrap();
        // Uncommitted and new files are part of what the helper sees.
        std::fs::write(dir.path().join("a.txt"), "one\ntwo\nthree\n").unwrap();
        std::fs::write(dir.path().join("new.txt"), "fresh\n").unwrap();

        let wt = create(root, "toolu_01:x").unwrap().unwrap();
        assert_eq!(wt.branch, "ws/toolu-01-x");
        assert_eq!(read(&wt.folder, "a.txt"), "one\ntwo\nthree\n");
        assert_eq!(read(&wt.folder, "new.txt"), "fresh\n");
        // The user's index and status are untouched by the snapshot.
        assert!(
            git(root, &["diff", "--cached", "--name-only"])
                .unwrap()
                .is_empty()
        );
        // It lives inside .git, so it is not among the project's files.
        assert!(wt.top.replace('\\', "/").contains("/.git/ws-worktrees/"));

        std::fs::write(Path::new(&wt.folder).join("a.txt"), "ONE\ntwo\nthree\n").unwrap();
        std::fs::write(Path::new(&wt.folder).join("made.txt"), "by helper\n").unwrap();
        assert!(seal(&wt.top, "worker: tidy").unwrap());
        // The user's files have not changed yet.
        assert_eq!(read(root, "a.txt"), "one\ntwo\nthree\n");

        let changed = changes(root, &wt.base, &wt.branch).unwrap().unwrap();
        let paths: Vec<_> = changed.iter().map(|c| c.path.as_str()).collect();
        assert_eq!(paths, ["a.txt", "made.txt"]);
        assert_eq!(changed[0].added, Some(1));
        assert!(diff(root, &wt.base, &wt.branch).unwrap().contains("+ONE"));

        // The user edits another line meanwhile; the merge keeps both.
        std::fs::write(dir.path().join("a.txt"), "one\ntwo\nthree\nfour\n").unwrap();
        merge(root, &wt.base, &wt.branch).unwrap();
        assert_eq!(read(root, "a.txt"), "ONE\ntwo\nthree\nfour\n");
        assert_eq!(read(root, "made.txt"), "by helper\n");
        assert_eq!(
            changes(root, &wt.base, &wt.branch).unwrap(),
            None,
            "gone once merged"
        );
        assert!(!Path::new(&wt.top).exists());
    }

    #[test]
    fn a_discarded_helper_leaves_nothing_behind() {
        let Some(dir) = repo() else { return };
        let root = dir.path().to_str().unwrap();
        let wt = create(root, "d1").unwrap().unwrap();
        std::fs::write(Path::new(&wt.folder).join("a.txt"), "changed\n").unwrap();
        assert!(seal(&wt.top, "worker").unwrap());
        discard(root, &wt.branch).unwrap();
        assert_eq!(read(root, "a.txt"), "one\ntwo\n");
        assert!(!Path::new(&wt.top).exists());
        assert_eq!(changes(root, &wt.base, &wt.branch).unwrap(), None);
    }

    #[test]
    fn a_helper_that_changed_nothing_has_nothing_to_seal() {
        let Some(dir) = repo() else { return };
        let wt = create(dir.path().to_str().unwrap(), "n1").unwrap().unwrap();
        assert!(!seal(&wt.top, "worker").unwrap());
    }

    #[test]
    fn a_folder_outside_git_gets_no_worktree() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(create(dir.path().to_str().unwrap(), "x").unwrap(), None);
    }
}
