//! Read-only file access for the explorer and viewer.
//!
//! Every path is a project root plus a path relative to it, and nothing may
//! resolve outside the root: the webview has no filesystem access of its own
//! (master spec s11.2), and these two commands must not quietly become one.

use serde::Serialize;
use std::path::{Component, Path, PathBuf};

/// Larger files are refused rather than shipped whole into the webview.
// shortcut: no paging, upgrade if huge files need viewing.
const MAX_VIEW_BYTES: u64 = 4 * 1024 * 1024;

#[derive(Debug, Serialize, PartialEq, Eq)]
pub struct Entry {
    pub name: String,
    /// Relative to the project root, with `/` separators.
    pub path: String,
    pub is_dir: bool,
}

/// Join `rel` onto `root`, refusing anything that would leave it.
fn resolve(root: &str, rel: &str) -> Result<PathBuf, String> {
    let root = Path::new(root.trim());
    let rel = Path::new(rel);
    if rel
        .components()
        .any(|c| !matches!(c, Component::Normal(_) | Component::CurDir))
    {
        return Err(format!("{} is not inside the project", rel.display()));
    }
    let root = root
        .canonicalize()
        .map_err(|e| format!("cannot open {}: {e}", root.display()))?;
    let full = root
        .join(rel)
        .canonicalize()
        .map_err(|e| format!("cannot open {}: {e}", rel.display()))?;
    // Catches a symlink or junction pointing out of the project.
    if !full.starts_with(&root) {
        return Err(format!("{} is not inside the project", rel.display()));
    }
    Ok(full)
}

/// One directory's entries, folders first, each group by name. Listed one
/// level at a time, so a huge repo is only read as far as it is opened.
pub fn list_dir(root: &str, rel: &str) -> Result<Vec<Entry>, String> {
    let dir = resolve(root, rel)?;
    let read = std::fs::read_dir(&dir).map_err(|e| format!("cannot list {rel}: {e}"))?;
    let mut entries: Vec<Entry> = read
        .flatten()
        .filter(|e| e.file_name() != ".git")
        .map(|e| {
            let name = e.file_name().to_string_lossy().into_owned();
            let path = if rel.is_empty() {
                name.clone()
            } else {
                format!("{}/{name}", rel.trim_end_matches('/'))
            };
            Entry {
                is_dir: e.file_type().is_ok_and(|t| t.is_dir()),
                name,
                path,
            }
        })
        .collect();
    entries.sort_by(|a, b| {
        b.is_dir
            .cmp(&a.is_dir)
            .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
    });
    Ok(entries)
}

/// A text file's contents, for viewing.
pub fn read_text(root: &str, rel: &str) -> Result<String, String> {
    let path = resolve(root, rel)?;
    let size = std::fs::metadata(&path).map_err(|e| e.to_string())?.len();
    if size > MAX_VIEW_BYTES {
        return Err(format!(
            "{rel} is {} MiB, too large to view here",
            size / (1024 * 1024)
        ));
    }
    let bytes = std::fs::read(&path).map_err(|e| e.to_string())?;
    String::from_utf8(bytes).map_err(|_| format!("{rel} is not a text file"))
}

#[cfg(test)]
mod tests {
    // Spec s14 permits unwrap/expect in tests.
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;

    fn project() -> (tempfile::TempDir, String) {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path();
        std::fs::create_dir_all(p.join("src/inner")).unwrap();
        std::fs::create_dir_all(p.join(".git")).unwrap();
        std::fs::write(p.join("src/main.rs"), "fn main() {}\n").unwrap();
        std::fs::write(p.join("README.md"), "# hi").unwrap();
        std::fs::write(p.join("blob.bin"), [0xff, 0xfe, 0x00]).unwrap();
        let root = p.display().to_string();
        (dir, root)
    }

    #[test]
    fn a_directory_lists_folders_first_without_git() {
        let (_d, root) = project();
        let names: Vec<_> = list_dir(&root, "")
            .unwrap()
            .into_iter()
            .map(|e| e.name)
            .collect();
        assert_eq!(names, ["src", "blob.bin", "README.md"]);
        let inner = list_dir(&root, "src").unwrap();
        assert_eq!(inner[0].path, "src/inner");
        assert!(inner[0].is_dir);
        assert_eq!(inner[1].path, "src/main.rs");
    }

    #[test]
    fn text_files_are_read_and_binary_files_refused() {
        let (_d, root) = project();
        assert_eq!(read_text(&root, "src/main.rs").unwrap(), "fn main() {}\n");
        assert!(
            read_text(&root, "blob.bin")
                .unwrap_err()
                .contains("not a text file")
        );
    }

    #[test]
    fn nothing_outside_the_project_can_be_reached() {
        let (_d, root) = project();
        for escape in [
            "../outside",
            "src/../../outside",
            "/etc/passwd",
            "C:/Windows/win.ini",
        ] {
            assert!(
                read_text(&root, escape).is_err(),
                "{escape} must be refused"
            );
            assert!(list_dir(&root, escape).is_err(), "{escape} must be refused");
        }
    }
}
