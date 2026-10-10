//! The Understand-Anything dashboard: a knowledge graph of a codebase.
//!
//! Building the graph is UA's `/understand` skill; viewing it needs no model at
//! all. This launches UA's prebuilt viewer the same way UA's own dashboard
//! skill does (`npx <release tarball> <project>`), waits for the tokenized URL
//! it prints, and opens that in a window of its own.

use std::path::{Path, PathBuf};
use std::time::Duration;
use tauri::{Manager, WebviewUrl, WebviewWindowBuilder};
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::{Child, Command};

/// Pinned to a release that ships the viewer tarball (checked: 794,982 bytes).
/// shortcut: fixed version, upgrade by hand when a newer release ships the viewer.
const VIEWER_URL: &str = "https://github.com/Egonex-AI/Understand-Anything/releases/download/v2.9.0/understand-anything-viewer.tgz";

/// The first run downloads the viewer, so allow for a slow network.
const START_TIMEOUT: Duration = Duration::from_secs(120);

const WINDOW: &str = "knowledge-graph";

/// The graph file UA writes, in its current or legacy folder.
fn graph_file(project: &Path) -> Option<PathBuf> {
    [".ua", ".understand-anything"]
        .iter()
        .map(|d| project.join(d).join("knowledge-graph.json"))
        .find(|p| p.is_file())
}

/// Pull the dashboard URL out of a line of viewer output, accepting only a
/// loopback address: this URL is opened in an app window, and it comes from a
/// downloaded program's output.
fn dashboard_url(line: &str) -> Option<String> {
    let start = line.find("http://")?;
    let url = line[start..].split_whitespace().next()?;
    let host = url
        .trim_start_matches("http://")
        .split([':', '/', '?'])
        .next()?;
    matches!(host, "127.0.0.1" | "localhost").then(|| url.to_owned())
}

/// Start the viewer for `project` and open it. Replaces any viewer already
/// running.
pub async fn open(app: &tauri::AppHandle, project: &str) -> Result<String, String> {
    let project = PathBuf::from(project.trim());
    if graph_file(&project).is_none() {
        return Err(format!(
            "No knowledge graph in {}. Build one with Understand-Anything's /understand first.",
            project.display()
        ));
    }

    let state = app.state::<crate::state::AppState>();
    stop(&state);

    // npx is a .cmd script on Windows, which Command does not find by bare name.
    let npx = if cfg!(windows) { "npx.cmd" } else { "npx" };
    let mut child = Command::new(npx)
        .args(["--yes", VIEWER_URL])
        .arg(&project)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .kill_on_drop(true)
        .spawn()
        .map_err(|e| format!("could not run npx (is Node.js installed?): {e}"))?;

    let stdout = child.stdout.take().ok_or("the viewer has no output")?;
    let mut lines = BufReader::new(stdout).lines();
    let mut seen = Vec::new();
    let url = tokio::time::timeout(START_TIMEOUT, async {
        while let Ok(Some(line)) = lines.next_line().await {
            if let Some(url) = dashboard_url(&line) {
                return Some(url);
            }
            seen.push(line);
        }
        None
    })
    .await
    .map_err(|_| "the viewer did not start within two minutes".to_owned())?
    .ok_or_else(|| format!("the viewer stopped without a URL: {}", seen.join(" | ")))?;

    // Keep draining output, or the viewer blocks once the pipe fills.
    tauri::async_runtime::spawn(async move { while let Ok(Some(_)) = lines.next_line().await {} });

    if let Ok(mut viewer) = state.viewer.lock() {
        *viewer = Some(child);
    }

    let target: tauri::Url = url.parse().map_err(|e| format!("bad dashboard URL: {e}"))?;
    let title = format!("Knowledge graph — {}", project.display());
    if let Some(window) = app.get_webview_window(WINDOW) {
        window.navigate(target).map_err(|e| e.to_string())?;
        let _ = window.set_title(&title);
        let _ = window.set_focus();
    } else {
        let handle = app.clone();
        WebviewWindowBuilder::new(app, WINDOW, WebviewUrl::External(target))
            .title(title)
            .inner_size(1280.0, 860.0)
            .build()
            .map_err(|e| e.to_string())?
            // Closing the window stops the viewer it was showing.
            .on_window_event(move |event| {
                if matches!(event, tauri::WindowEvent::Destroyed) {
                    stop(&handle.state::<crate::state::AppState>());
                }
            });
    }
    Ok(url)
}

/// Stop the running viewer, if any.
pub fn stop(state: &crate::state::AppState) {
    if let Ok(mut viewer) = state.viewer.lock() {
        if let Some(mut child) = viewer.take() {
            let _ = child.start_kill();
        }
    }
}

/// The viewer process, held so it can be stopped.
pub type Viewer = std::sync::Mutex<Option<Child>>;

#[cfg(test)]
mod tests {
    // Spec s14 permits unwrap/expect in tests.
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;

    #[test]
    fn the_dashboard_url_is_read_from_the_viewers_output() {
        assert_eq!(
            dashboard_url("🔑  Dashboard URL: http://127.0.0.1:5173?token=abc123").as_deref(),
            Some("http://127.0.0.1:5173?token=abc123")
        );
        assert_eq!(
            dashboard_url("Dashboard URL: http://localhost:5174/?token=x  ").as_deref(),
            Some("http://localhost:5174/?token=x")
        );
        assert_eq!(dashboard_url("Downloading viewer..."), None);
    }

    #[test]
    fn only_loopback_urls_are_opened() {
        assert_eq!(
            dashboard_url("go to http://evil.example.com:5173?token=x"),
            None
        );
        assert_eq!(dashboard_url("http://127.0.0.1.evil.com/x"), None);
        assert_eq!(
            dashboard_url("https://127.0.0.1:5173"),
            None,
            "the viewer serves plain http"
        );
    }

    #[test]
    fn the_graph_is_found_in_either_folder() {
        let dir = tempfile::tempdir().unwrap();
        assert!(graph_file(dir.path()).is_none());
        std::fs::create_dir(dir.path().join(".understand-anything")).unwrap();
        std::fs::write(
            dir.path().join(".understand-anything/knowledge-graph.json"),
            "{}",
        )
        .unwrap();
        assert!(graph_file(dir.path()).is_some(), "legacy folder");
    }
}
