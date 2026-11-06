//! The agent's own tools for the folder open in Workspace, shared by the app
//! and the command-line agent.

pub mod files;
pub mod git;
pub mod workspace;

use std::path::PathBuf;

/// The first of `names` on PATH.
pub fn find(names: &[&str]) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    let exts: Vec<String> = if cfg!(windows) {
        std::env::var("PATHEXT")
            .unwrap_or_else(|_| ".EXE;.CMD;.BAT".to_owned())
            .split(';')
            .map(str::to_owned)
            .collect()
    } else {
        vec![String::new()]
    };
    names.iter().find_map(|name| {
        std::env::split_paths(&path).find_map(|dir| {
            exts.iter()
                .map(|ext| dir.join(format!("{name}{ext}")))
                .find(|p| p.is_file())
        })
    })
}
