//! Blueprints: their own windows, and the inventory of parts and connectors
//! every blueprint shares.

use std::collections::{BTreeMap, BTreeSet};
use std::hash::{Hash, Hasher};
use std::sync::Mutex;
use std::sync::atomic::{AtomicU32, Ordering};
use tauri::{AppHandle, Manager, WebviewUrl, WebviewWindow, WebviewWindowBuilder};

type CmdResult<T> = Result<T, String>;

fn library_path(app: &AppHandle) -> CmdResult<std::path::PathBuf> {
    Ok(app
        .path()
        .app_data_dir()
        .map_err(|e| e.to_string())?
        .join("parts-library.json"))
}

/// The inventory as JSON; empty before anything is stored.
#[tauri::command]
pub fn read_parts_library(app: AppHandle) -> CmdResult<String> {
    match std::fs::read_to_string(library_path(&app)?) {
        Ok(text) => Ok(text),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(String::new()),
        Err(e) => Err(e.to_string()),
    }
}

/// Replace the inventory. Only a JSON object is accepted, so a bad write from
/// the page cannot leave a file the next read chokes on.
#[tauri::command]
pub fn write_parts_library(app: AppHandle, text: String) -> CmdResult<()> {
    let parsed: serde_json::Value = serde_json::from_str(&text).map_err(|e| e.to_string())?;
    if !parsed.is_object() {
        return Err("the parts library must be a JSON object".to_owned());
    }
    let path = library_path(&app)?;
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    }
    ws_core::edits::restore(&path, Some(text.as_bytes())).map_err(|e| e.to_string())
}

/// A blueprint file's full path, the same however it was reached: through an
/// open folder (`root` + `sub/x.blueprint`) or the save dialog (`dir` + `x.blueprint`).
fn key(root: &str, path: &str) -> String {
    format!("{}\\{}", root.trim_end_matches(['\\', '/']), path)
        .replace('/', "\\")
        .to_lowercase()
}

/// One window per blueprint file: opening it again brings that window forward.
fn label(root: &str, path: &str) -> String {
    let mut h = std::collections::hash_map::DefaultHasher::new();
    key(root, path).hash(&mut h);
    format!("blueprint-{:016x}", h.finish())
}

/// Untitled windows saved since: file key to window label, so opening that
/// file again finds the window it is already in.
static SAVED: Mutex<BTreeMap<String, String>> = Mutex::new(BTreeMap::new());
static UNTITLED: AtomicU32 = AtomicU32::new(0);
/// Blueprint windows with unsaved changes, by label.
static UNSAVED: Mutex<BTreeSet<String>> = Mutex::new(BTreeSet::new());

#[tauri::command]
pub fn blueprint_dirty(window: WebviewWindow, dirty: bool) -> CmdResult<()> {
    let mut unsaved = UNSAVED.lock().map_err(|e| e.to_string())?;
    if dirty {
        unsaved.insert(window.label().to_owned());
    } else {
        unsaved.remove(window.label());
    }
    Ok(())
}

/// How many open blueprint windows have unsaved changes, for the main window
/// to ask about before it closes Workspace (and them with it).
#[tauri::command]
pub fn unsaved_blueprints(app: AppHandle) -> CmdResult<usize> {
    let unsaved = UNSAVED.lock().map_err(|e| e.to_string())?;
    Ok(unsaved
        .iter()
        .filter(|l| app.get_webview_window(l).is_some())
        .count())
}

/// With no file, an untitled blueprint: nothing is written until it is saved,
/// and `dir` is where the save dialog starts.
///
/// Async: on Windows a window made inside a synchronous command deadlocks
/// and stays blank.
#[tauri::command]
pub async fn open_blueprint_window(
    app: AppHandle,
    root: Option<String>,
    path: Option<String>,
    dir: Option<String>,
) -> CmdResult<()> {
    let (label, name) = match (&root, &path) {
        (Some(root), Some(path)) => {
            let known = SAVED
                .lock()
                .map_err(|e| e.to_string())?
                .get(&key(root, path))
                .cloned();
            let label = known
                .filter(|l| app.get_webview_window(l).is_some())
                .unwrap_or_else(|| label(root, path));
            if let Some(w) = app.get_webview_window(&label) {
                let _ = w.unminimize();
                return w.set_focus().map_err(|e| e.to_string());
            }
            let name = path.rsplit('/').next().unwrap_or(path);
            (label, name.trim_end_matches(".blueprint").to_owned())
        }
        _ => {
            let n = UNTITLED.fetch_add(1, Ordering::Relaxed) + 1;
            (format!("blueprint-new-{n}"), "Untitled".to_owned())
        }
    };
    // The page reads which file it is from here; JSON keeps any path safe to embed.
    let file = serde_json::json!({ "root": root, "path": path, "dir": dir });
    WebviewWindowBuilder::new(&app, label, WebviewUrl::App("index.html".into()))
        .title(format!("{name} — Workshop — Workspace"))
        .inner_size(1280.0, 820.0)
        .min_inner_size(720.0, 480.0)
        .initialization_script(format!("window.__WORKSPACE_BLUEPRINT__ = {file};"))
        .build()
        .map(|_| ())
        .map_err(|e| e.to_string())
}

/// Open each `.blueprint` in a command line (`cwd` resolves relative ones):
/// how Explorer's double-click, and a second launch handed over, reach here.
pub fn open_from_args(app: &AppHandle, args: &[String], cwd: &str) {
    for arg in args.iter().skip(1) {
        let file = std::path::Path::new(cwd).join(arg);
        if !file
            .extension()
            .is_some_and(|e| e.eq_ignore_ascii_case("blueprint"))
        {
            continue;
        }
        let (Some(dir), Some(name)) = (file.parent(), file.file_name()) else {
            continue;
        };
        let (app, dir, name) = (
            app.clone(),
            dir.to_string_lossy().into_owned(),
            name.to_string_lossy().into_owned(),
        );
        // Off the main thread, as the command runs, or the window stays blank.
        tauri::async_runtime::spawn(async move {
            if let Err(e) = open_blueprint_window(app, Some(dir), Some(name), None).await {
                tracing::error!(error = %e, "could not open the blueprint");
            }
        });
    }
}

/// An untitled blueprint was saved as `path` in `root`: note which window has it.
#[tauri::command]
pub fn blueprint_saved_as(window: WebviewWindow, root: String, path: String) -> CmdResult<()> {
    SAVED
        .lock()
        .map_err(|e| e.to_string())?
        .insert(key(&root, &path), window.label().to_owned());
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{key, label};

    #[test]
    fn one_label_per_file_whatever_the_case() {
        assert_eq!(
            label("C:\\A", "x/y.blueprint"),
            label("c:\\a", "X/Y.blueprint")
        );
        assert_ne!(label("C:\\A", "x.blueprint"), label("C:\\A", "y.blueprint"));
        // Through an open folder or straight from the save dialog: one file.
        assert_eq!(
            key("C:\\A\\", "sub/x.blueprint"),
            key("C:\\A\\sub", "x.blueprint")
        );
        assert!(
            label("C:\\A", "x")
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '-')
        );
    }
}
