//! Blueprints: their own windows, and the inventory of parts and connectors
//! every blueprint shares.

use std::hash::{Hash, Hasher};
use tauri::{AppHandle, Manager, WebviewUrl, WebviewWindowBuilder};

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

/// One window per blueprint file: opening it again brings that window forward.
fn label(root: &str, path: &str) -> String {
    let mut h = std::collections::hash_map::DefaultHasher::new();
    (root.to_lowercase(), path.to_lowercase()).hash(&mut h);
    format!("blueprint-{:016x}", h.finish())
}

/// Async: on Windows a window made inside a synchronous command deadlocks
/// and stays blank.
#[tauri::command]
pub async fn open_blueprint_window(app: AppHandle, root: String, path: String) -> CmdResult<()> {
    let label = label(&root, &path);
    if let Some(w) = app.get_webview_window(&label) {
        let _ = w.unminimize();
        return w.set_focus().map_err(|e| e.to_string());
    }
    let name = path
        .rsplit('/')
        .next()
        .unwrap_or(&path)
        .trim_end_matches(".blueprint");
    // The page reads which file it is from here; JSON keeps any path safe to embed.
    let file = serde_json::json!({ "root": root, "path": path });
    WebviewWindowBuilder::new(&app, label, WebviewUrl::App("index.html".into()))
        .title(format!("{name} — Blueprint — F.R.I.D.A.Y"))
        .inner_size(1280.0, 820.0)
        .min_inner_size(720.0, 480.0)
        .initialization_script(format!("window.__FRIDAY_BLUEPRINT__ = {file};"))
        .build()
        .map(|_| ())
        .map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::label;

    #[test]
    fn one_label_per_file_whatever_the_case() {
        assert_eq!(
            label("C:\\A", "x/y.blueprint"),
            label("c:\\a", "X/Y.blueprint")
        );
        assert_ne!(label("C:\\A", "x.blueprint"), label("C:\\A", "y.blueprint"));
        assert!(
            label("C:\\A", "x")
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '-')
        );
    }
}
