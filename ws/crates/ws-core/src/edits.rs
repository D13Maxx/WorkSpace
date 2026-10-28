//! Snapshots, diffs and undo for files an approved tool edits.
//!
//! The exact bytes on either side of an edit go to the blob store, so Undo
//! restores the file byte for byte (line endings included) rather than
//! re-creating it from text.

use std::path::Path;
use std::time::Duration;

/// A diff bigger than this is cut, so one rewritten file cannot flood the UI.
const MAX_DIFF_CHARS: usize = 200_000;

/// The bytes at `path` now: `Ok(None)` if there is no file.
///
/// # Errors
/// Fails if the file exists but cannot be read. The caller then skips the
/// snapshot rather than recording an edit it cannot undo.
pub fn snapshot(path: &Path) -> std::io::Result<Option<Vec<u8>>> {
    match std::fs::read(path) {
        Ok(bytes) => Ok(Some(bytes)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e),
    }
}

/// A unified diff for display. `None` on a side means the file was absent.
#[must_use]
pub fn unified_diff(name: &str, before: Option<&[u8]>, after: Option<&[u8]>) -> String {
    fn as_text(b: Option<&[u8]>) -> Result<Option<&str>, std::str::Utf8Error> {
        b.map(std::str::from_utf8).transpose()
    }
    let (Ok(old), Ok(new)) = (as_text(before), as_text(after)) else {
        return "binary file changed".to_owned();
    };
    let diff = similar::TextDiff::configure()
        // A huge rewrite still gets a diff, just a less minimal one.
        .timeout(Duration::from_secs(1))
        .diff_lines(old.unwrap_or(""), new.unwrap_or(""));
    let old_name = if before.is_some() { name } else { "/dev/null" };
    let new_name = if after.is_some() { name } else { "/dev/null" };
    let mut text = diff
        .unified_diff()
        .context_radius(3)
        .header(old_name, new_name)
        .to_string();
    if text.len() > MAX_DIFF_CHARS {
        let cut = (0..=MAX_DIFF_CHARS)
            .rev()
            .find(|i| text.is_char_boundary(*i))
            .unwrap_or(0);
        text.truncate(cut);
        text.push_str("\n… diff truncated\n");
    }
    text
}

/// Put `bytes` at `path` (or remove it, for `None`) without a moment where a
/// crash leaves the file half-written: write beside it, then rename over it.
///
/// # Errors
/// Fails if the file cannot be written or removed.
pub fn restore(path: &Path, bytes: Option<&[u8]>) -> std::io::Result<()> {
    let Some(bytes) = bytes else {
        return std::fs::remove_file(path);
    };
    let tmp = path.with_file_name(format!(
        ".{}.ws-undo",
        path.file_name()
            .map_or_else(String::new, |n| n.to_string_lossy().into_owned())
    ));
    // On disk before the rename: otherwise a power cut can keep the rename
    // and lose the data, leaving an empty file.
    let written = (|| {
        let mut file = std::fs::File::create(&tmp)?;
        std::io::Write::write_all(&mut file, bytes)?;
        file.sync_all()
    })();
    written
        .and_then(|()| std::fs::rename(&tmp, path))
        .inspect_err(|_| {
            let _ = std::fs::remove_file(&tmp);
        })
}

#[cfg(test)]
mod tests {
    // Spec s14 permits unwrap/expect in tests.
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;

    #[test]
    fn a_changed_line_shows_as_removed_and_added() {
        let d = unified_diff(
            "a.rs",
            Some(b"one\ntwo\nthree\n"),
            Some(b"one\nTWO\nthree\n"),
        );
        assert!(d.contains("--- a.rs") && d.contains("+++ a.rs"), "{d}");
        assert!(d.contains("-two\n") && d.contains("+TWO\n"), "{d}");
    }

    #[test]
    fn a_created_file_diffs_against_nothing() {
        let d = unified_diff("new.rs", None, Some(b"fn x() {}\n"));
        assert!(
            d.contains("--- /dev/null") && d.contains("+fn x() {}"),
            "{d}"
        );
    }

    #[test]
    fn binary_content_is_not_diffed() {
        assert_eq!(
            unified_diff("b", Some(&[0xff, 0xfe]), Some(b"x")),
            "binary file changed"
        );
    }

    #[test]
    fn restore_puts_back_exact_bytes_and_can_delete() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("f.txt");
        std::fs::write(&p, "changed").unwrap();
        restore(&p, Some(b"a\r\nb\r\n")).unwrap();
        assert_eq!(
            std::fs::read(&p).unwrap(),
            b"a\r\nb\r\n",
            "line endings survive"
        );
        assert_eq!(snapshot(&p).unwrap().as_deref(), Some(&b"a\r\nb\r\n"[..]));
        restore(&p, None).unwrap();
        assert_eq!(snapshot(&p).unwrap(), None);
        assert_eq!(
            std::fs::read_dir(dir.path()).unwrap().count(),
            0,
            "no temp file left"
        );
    }
}
