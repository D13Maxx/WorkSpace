//! Files Workspace was asked to open by Windows ("Open with", a double-click):
//! blueprints get their own window (see `blueprint`), anything else opens in
//! the main window's editor, its folder on the left.

use serde::Serialize;
use std::sync::Mutex;
use tauri::{AppHandle, Emitter as _};

/// A file to open: its folder, and its name in it. Shaped like `open-code`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct OpenCode {
    pub root: String,
    pub path: String,
}

/// Files from the first launch, kept until the window has loaded and asks:
/// an event sent before then has no one listening.
static PENDING: Mutex<Vec<OpenCode>> = Mutex::new(Vec::new());

/// The existing files in `args` (after the program), except blueprints.
fn files(args: &[String], cwd: &str) -> Vec<OpenCode> {
    args.iter()
        .skip(1)
        .map(|a| std::path::Path::new(cwd).join(a))
        .filter(|p| p.is_file())
        .filter(|p| {
            !p.extension()
                .is_some_and(|e| e.eq_ignore_ascii_case("blueprint"))
        })
        .filter_map(|p| {
            Some(OpenCode {
                root: p.parent()?.to_string_lossy().into_owned(),
                path: p.file_name()?.to_string_lossy().into_owned(),
            })
        })
        .collect()
}

/// At startup: hold the files for the window to take.
pub fn hold(args: &[String], cwd: &str) {
    if let Ok(mut p) = PENDING.lock() {
        p.extend(files(args, cwd));
    }
}

/// A second launch while running: the window is listening, so open them now.
pub fn open_now(app: &AppHandle, args: &[String], cwd: &str) {
    for file in files(args, cwd) {
        let _ = app.emit("open-code", file);
    }
}

/// The files Workspace was launched with, once.
#[tauri::command]
pub fn take_launch_files() -> Vec<OpenCode> {
    PENDING
        .lock()
        .map(|mut p| std::mem::take(&mut *p))
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;

    #[test]
    fn existing_files_open_and_blueprints_and_flags_do_not() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("config.toml"), "a = 1").unwrap();
        std::fs::write(dir.path().join("plan.blueprint"), "{}").unwrap();
        let cwd = dir.path().to_string_lossy().into_owned();
        let args = [
            "Workspace.exe",
            "config.toml",
            "plan.blueprint",
            "--flag",
            "missing.rs",
        ]
        .map(str::to_owned);
        assert_eq!(
            files(&args, &cwd),
            [OpenCode {
                root: cwd.clone(),
                path: "config.toml".to_owned()
            }]
        );
        // An absolute path ignores the working directory.
        let abs = dir
            .path()
            .join("config.toml")
            .to_string_lossy()
            .into_owned();
        assert_eq!(files(&["x".to_owned(), abs], "C:\\elsewhere").len(), 1);
    }
}
