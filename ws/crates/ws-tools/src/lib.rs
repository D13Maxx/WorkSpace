//! The agent's own tools for the folder open in Workspace, shared by the app
//! and the command-line agent.

pub mod files;
pub mod git;
pub mod index;
pub mod workspace;
pub mod worktree;

use std::ffi::OsStr;
use std::path::PathBuf;

/// The first of `names` on PATH.
pub fn find(names: &[&str]) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    names
        .iter()
        .find_map(|name| find_all(name, &path).into_iter().next())
}

/// Every `name` program on `path`, in PATH order.
pub fn find_all(name: &str, path: &OsStr) -> Vec<PathBuf> {
    let exts: Vec<String> = if cfg!(windows) {
        std::env::var("PATHEXT")
            .unwrap_or_else(|_| ".EXE;.CMD;.BAT".to_owned())
            .split(';')
            .map(str::to_owned)
            .collect()
    } else {
        vec![String::new()]
    };
    std::env::split_paths(path)
        .flat_map(|dir| {
            exts.iter()
                .map(|ext| dir.join(format!("{name}{ext}")))
                .collect::<Vec<_>>()
        })
        .filter(|p| p.is_file())
        .collect()
}
