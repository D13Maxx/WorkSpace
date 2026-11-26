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

/// Network steps never wait on a hidden terminal prompt; git's credential
/// helper (its own sign-in window) still runs.
const NO_PROMPT: &[(&str, &str)] = &[("GIT_TERMINAL_PROMPT", "0")];

/// The branch checked out and where it stands against its upstream.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Branches {
    /// Empty when no branch is checked out (a detached HEAD).
    pub current: String,
    /// Every local branch, in name order.
    pub all: Vec<String>,
    /// Like `origin/main`; None before the branch is first pushed.
    pub upstream: Option<String>,
    /// Commits here that the upstream lacks, and the other way round.
    pub ahead: u32,
    pub behind: u32,
}

/// The local branches and the current one's standing.
///
/// # Errors
/// `root` is not in a git repository, or git cannot run.
pub fn branches(root: &str) -> Result<Branches, String> {
    let current = git(root, &["branch", "--show-current"])?.trim().to_owned();
    let all = git(
        root,
        &["for-each-ref", "--format=%(refname:short)", "refs/heads"],
    )?
    .lines()
    .map(str::to_owned)
    .collect();
    let upstream = git(root, &["rev-parse", "--abbrev-ref", "@{upstream}"])
        .ok()
        .map(|u| u.trim().to_owned());
    let (mut behind, mut ahead) = (0, 0);
    if upstream.is_some() {
        let counts = git(
            root,
            &["rev-list", "--left-right", "--count", "@{upstream}...HEAD"],
        )?;
        let mut n = counts.split_whitespace().map(|c| c.parse().unwrap_or(0));
        behind = n.next().unwrap_or(0);
        ahead = n.next().unwrap_or(0);
    }
    Ok(Branches {
        current,
        all,
        upstream,
        ahead,
        behind,
    })
}

/// A name git accepts for a branch, and one that cannot pass as an option.
fn branch_name(root: &str, name: &str) -> Result<String, String> {
    let name = name.trim();
    if name.is_empty() || name.starts_with('-') {
        return Err(format!("\"{name}\" is not a branch name"));
    }
    git(root, &["check-ref-format", "--branch", name])
        .map(|n| n.trim().to_owned())
        .map_err(|_| format!("\"{name}\" is not a branch name"))
}

/// Check out `name`, making it first from the current commit when `create`.
/// git refuses when uncommitted changes would be overwritten.
///
/// # Errors
/// The name is invalid, or git refuses; git's words say why.
pub fn switch(root: &str, name: &str, create: bool) -> Result<(), String> {
    let name = branch_name(root, name)?;
    let args: &[&str] = if create {
        &["switch", "-c", &name]
    } else {
        &["switch", &name]
    };
    git(root, args).map(drop)
}

/// Push the current branch, setting its upstream on `origin` the first time.
///
/// # Errors
/// No branch is checked out, there is no `origin`, sign-in failed, or the
/// remote has commits this branch lacks (pull first).
pub fn push(root: &str) -> Result<(), String> {
    let b = branches(root)?;
    if b.current.is_empty() {
        return Err("no branch is checked out".to_owned());
    }
    let args: &[&str] = if b.upstream.is_some() {
        &["push", "-q"]
    } else {
        &["push", "-q", "-u", "origin", "HEAD"]
    };
    run(root, args, None, NO_PROMPT).map(drop)
}

/// Bring in the upstream's new commits, only when that needs no merge.
///
/// # Errors
/// No upstream, sign-in failed, or both sides have new commits.
pub fn pull(root: &str) -> Result<(), String> {
    run(root, &["pull", "-q", "--ff-only"], None, NO_PROMPT).map(drop)
}

/// The address to clone: a URL as given, or `owner/repo` for GitHub.
///
/// # Errors
/// Neither form.
pub fn clone_url(input: &str) -> Result<String, String> {
    let s = input.trim().trim_end_matches('/');
    let ok_part = |p: &str| {
        !p.is_empty()
            && !p.starts_with('.')
            && p.chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
    };
    if ["https://", "ssh://", "file://", "git@"]
        .iter()
        .any(|p| s.starts_with(p))
    {
        return Ok(s.to_owned());
    }
    match s.split_once('/') {
        Some((owner, repo)) if ok_part(owner) && ok_part(repo) => {
            Ok(format!("https://github.com/{owner}/{repo}.git"))
        }
        _ => Err(format!(
            "\"{s}\" is not a repo address: paste its https:// URL or write owner/repo"
        )),
    }
}

/// Clone `input` (see `clone_url`) into a new folder under `parent`, named
/// after the repo; returns that folder.
///
/// # Errors
/// The address is not valid, the folder already exists, or git fails.
pub fn clone(input: &str, parent: &str) -> Result<String, String> {
    let url = clone_url(input)?;
    let name = url
        .rsplit(['/', ':'])
        .next()
        .unwrap_or_default()
        .trim_end_matches(".git")
        .to_owned();
    if name.is_empty() || name.starts_with('.') {
        return Err(format!("cannot tell the repo's name from {url}"));
    }
    let dest = std::path::Path::new(parent.trim()).join(&name);
    if dest.exists() {
        return Err(format!("{} already exists", dest.display()));
    }
    run(parent, &["clone", "-q", "--", &url, &name], None, NO_PROMPT)?;
    Ok(dest.to_string_lossy().into_owned())
}

/// The password git's credential helper holds for `https://github.com` (for
/// Git Credential Manager, a GitHub token); it may show the helper's own
/// sign-in window first. None when there is no helper or no sign-in.
pub fn github_credential() -> Option<String> {
    let dir = std::env::temp_dir();
    let out = run(
        dir.to_str()?,
        &["credential", "fill"],
        Some(b"protocol=https\nhost=github.com\n\n"),
        NO_PROMPT,
    )
    .ok()?;
    out.lines()
        .find_map(|l| l.strip_prefix("password="))
        .map(str::to_owned)
        .filter(|p| !p.is_empty())
}

/// The commit checked out, in full.
///
/// # Errors
/// No commits yet, or git cannot run.
pub fn head(root: &str) -> Result<String, String> {
    Ok(git(root, &["rev-parse", "HEAD"])?.trim().to_owned())
}

/// `owner/repo` of `origin` when it is on GitHub.
pub fn github_repo(root: &str) -> Option<String> {
    let url = git(root, &["remote", "get-url", "origin"]).ok()?;
    github_repo_of(url.trim())
}

fn github_repo_of(url: &str) -> Option<String> {
    let rest = url
        .strip_prefix("https://github.com/")
        .or_else(|| url.strip_prefix("git@github.com:"))
        .or_else(|| url.strip_prefix("ssh://git@github.com/"))?;
    let rest = rest.trim_end_matches('/').trim_end_matches(".git");
    let (owner, repo) = rest.split_once('/')?;
    (!owner.is_empty() && !repo.is_empty() && !repo.contains('/'))
        .then(|| format!("{owner}/{repo}"))
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
    fn push_pull_switch_and_clone_against_a_local_remote() {
        let Some(dir) = repo() else { return };
        let root = dir.path().to_str().unwrap();
        let remote = tempfile::tempdir().unwrap();
        let bare = remote.path().join("r.git");
        let bare_s = bare.to_str().unwrap();
        git(
            remote.path().to_str().unwrap(),
            &["init", "-q", "--bare", bare_s],
        )
        .unwrap();
        git(root, &["remote", "add", "origin", bare_s]).unwrap();

        let b = branches(root).unwrap();
        assert_eq!(b.upstream, None, "not pushed yet");
        push(root).unwrap();
        let b = branches(root).unwrap();
        assert!(b.upstream.is_some_and(|u| u.starts_with("origin/")));
        assert_eq!((b.ahead, b.behind), (0, 0));

        // A second copy pushes a commit; the first is then behind, and pulls it.
        let other = tempfile::tempdir().unwrap();
        let url = format!("file:///{}", bare_s.replace('\\', "/"));
        let copy = clone(&url, other.path().to_str().unwrap()).unwrap();
        for args in [
            &["config", "user.email", "t@t"][..],
            &["config", "user.name", "t"],
            &["config", "commit.gpgsign", "false"],
        ] {
            git(&copy, args).unwrap();
        }
        std::fs::write(std::path::Path::new(&copy).join("b.txt"), "b\n").unwrap();
        stage(&copy, "b.txt").unwrap();
        commit(&copy, "second").unwrap();
        push(&copy).unwrap();
        git(root, &["fetch", "-q"]).unwrap();
        assert_eq!(branches(root).unwrap().behind, 1);
        pull(root).unwrap();
        assert!(dir.path().join("b.txt").exists());
        assert!(
            clone(&url, other.path().to_str().unwrap()).is_err(),
            "the folder is there already"
        );

        switch(root, "feature", true).unwrap();
        assert_eq!(branches(root).unwrap().current, "feature");
        assert!(branches(root).unwrap().all.contains(&"feature".to_owned()));
        assert!(switch(root, "-f", false).is_err(), "not an option");
        assert!(switch(root, "a b", true).is_err());
    }

    #[test]
    fn clone_addresses_and_github_repos() {
        assert_eq!(
            clone_url("D13Maxx/WorkSpace").unwrap(),
            "https://github.com/D13Maxx/WorkSpace.git"
        );
        assert_eq!(
            clone_url("https://github.com/a/b/").unwrap(),
            "https://github.com/a/b"
        );
        assert!(
            clone_url("-x/y").is_ok_and(|u| u.contains("-x")),
            "only as an argument after --"
        );
        assert!(clone_url("../etc").is_err());
        assert!(clone_url("nope").is_err());
        for url in [
            "https://github.com/a/b.git",
            "https://github.com/a/b",
            "git@github.com:a/b.git",
            "ssh://git@github.com/a/b",
        ] {
            assert_eq!(github_repo_of(url).as_deref(), Some("a/b"), "{url}");
        }
        assert_eq!(github_repo_of("https://gitlab.com/a/b"), None);
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
