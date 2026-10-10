//! File access for the explorer, the editor and the agent's file tools.
//!
//! Every path is a project root plus a path relative to it, and nothing may
//! resolve outside the root: the webview has no filesystem access of its own
//! (master spec s11.2), and these commands must not quietly become one.

use serde::Serialize;
use std::path::{Component, Path, PathBuf};

/// Larger files are refused rather than shipped whole into the webview.
// shortcut: no paging, upgrade if huge files need viewing.
const MAX_VIEW_BYTES: u64 = 4 * 1024 * 1024;

/// The most a PDF or Word file may be to open in the viewer.
const MAX_OPEN_BYTES: u64 = 64 * 1024 * 1024;

#[derive(Debug, Serialize, PartialEq, Eq)]
pub struct Entry {
    pub name: String,
    /// Relative to the project root, with `/` separators.
    pub path: String,
    pub is_dir: bool,
}

/// Join `rel` onto `root`, refusing anything that would leave it.
pub(crate) fn resolve(root: &str, rel: &str) -> Result<PathBuf, String> {
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

/// A file's bytes, for the PDF and Word viewers.
pub fn read_bytes(root: &str, rel: &str) -> Result<Vec<u8>, String> {
    let path = resolve(root, rel)?;
    let size = std::fs::metadata(&path).map_err(|e| e.to_string())?.len();
    if size > MAX_OPEN_BYTES {
        return Err(format!(
            "{rel} is {} MiB, too large to open here",
            size / (1024 * 1024)
        ));
    }
    std::fs::read(&path).map_err(|e| e.to_string())
}

/// Where `rel` is, or would be once written: like `resolve`, but the file
/// (and folders on the way to it) need not exist yet.
pub(crate) fn resolve_for_write(root: &str, rel: &str) -> Result<PathBuf, String> {
    let rel_path = Path::new(rel);
    if rel.trim().is_empty()
        || rel_path
            .components()
            .any(|c| !matches!(c, Component::Normal(_)))
    {
        return Err(format!("{rel} is not inside the project"));
    }
    let root = Path::new(root.trim())
        .canonicalize()
        .map_err(|e| format!("cannot open {}: {e}", root.trim()))?;
    let full = root.join(rel_path);
    // The deepest part that exists decides where a symlink or junction leads.
    let existing = full
        .ancestors()
        .find(|p| p.exists())
        .ok_or_else(|| format!("cannot open {rel}"))?;
    let real = existing.canonicalize().map_err(|e| e.to_string())?;
    if !real.starts_with(&root) {
        return Err(format!("{rel} is not inside the project"));
    }
    let rest = full.strip_prefix(existing).map_err(|e| e.to_string())?;
    Ok(if rest.as_os_str().is_empty() {
        real
    } else {
        real.join(rest)
    })
}

/// Save `text` to `rel`, creating it and its folders if needed. Written
/// beside the file and renamed over it, so a crash never leaves half a file.
pub fn write_text(root: &str, rel: &str, text: &str) -> Result<(), String> {
    write_bytes(root, rel, text.as_bytes())
}

/// `write_text` for any bytes: a PDF saved with its new annotations.
pub fn write_bytes(root: &str, rel: &str, bytes: &[u8]) -> Result<(), String> {
    let path = resolve_for_write(root, rel)?;
    if path.is_dir() {
        return Err(format!("{rel} is a folder"));
    }
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    ws_core::edits::restore(&path, Some(bytes)).map_err(|e| e.to_string())
}

/// A new, empty file. Refuses to touch one that already exists.
pub fn create_file(root: &str, rel: &str) -> Result<(), String> {
    let path = resolve_for_write(root, rel)?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&path)
        .map(drop)
        .map_err(|e| match e.kind() {
            std::io::ErrorKind::AlreadyExists => format!("{rel} already exists"),
            _ => e.to_string(),
        })
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

    #[test]
    fn files_are_written_and_created_only_inside_the_project() {
        let (_d, root) = project();
        write_text(
            &root,
            "src/main.rs",
            "fn main() { }
",
        )
        .unwrap();
        assert_eq!(
            read_text(&root, "src/main.rs").unwrap(),
            "fn main() { }
"
        );
        create_file(&root, "new/deeper/a.c").unwrap();
        assert_eq!(read_text(&root, "new/deeper/a.c").unwrap(), "");
        assert!(
            create_file(&root, "new/deeper/a.c")
                .unwrap_err()
                .contains("already exists")
        );
        for escape in ["../outside.txt", "src/../../x", "C:/Windows/x.txt", ""] {
            assert!(
                write_text(&root, escape, "x").is_err(),
                "{escape} must be refused"
            );
            assert!(
                create_file(&root, escape).is_err(),
                "{escape} must be refused"
            );
        }
        assert_eq!(read_bytes(&root, "blob.bin").unwrap(), [0xff, 0xfe, 0x00]);
    }
}
