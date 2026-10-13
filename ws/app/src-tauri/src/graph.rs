//! The Understand-Anything dashboard, served by Workspace itself.
//!
//! The dashboard is UA's prebuilt web app, shipped inside Workspace (`public/ua`,
//! MIT). It reads the graph from a few JSON endpoints, which this module
//! answers the way UA's own viewer (`bin/viewer.mjs`) does: on 127.0.0.1 only,
//! behind a one-time token, with source previews limited to files the graph
//! lists. Nothing is downloaded and Node is not needed.
//!
//! The window shows a loopback http address, which Tauri treats as remote, so
//! the dashboard's code gets no access to Workspace's commands.

use serde_json::{Value, json};
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tauri::{Manager, WebviewUrl, WebviewWindowBuilder};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

const WINDOW: &str = "knowledge-graph";
/// As in UA's viewer: larger files are not previewed.
const MAX_SOURCE_FILE_BYTES: u64 = 1024 * 1024;
/// A dashboard request is a short GET; anything longer is refused.
const MAX_REQUEST_BYTES: usize = 16 * 1024;

/// UA's data folder, legacy name first as UA's viewer checks it.
fn graph_dir(project: &Path) -> Option<PathBuf> {
    [".understand-anything", ".ua"]
        .iter()
        .map(|d| project.join(d))
        .find(|d| d.join("knowledge-graph.json").is_file())
}

/// Looks up one of the dashboard's files: (mime type, bytes).
type Assets = dyn Fn(&str) -> Option<(String, Vec<u8>)> + Send + Sync;

/// Whether `project` already has a knowledge graph.
pub fn exists(project: &str) -> bool {
    graph_dir(Path::new(project.trim())).is_some()
}

/// One open dashboard: what it serves and who may ask.
struct Site {
    /// The dashboard's own files by path: (mime type, bytes).
    assets: Box<Assets>,
    /// As the user gave it, for matching the absolute paths UA may record.
    root: PathBuf,
    data: PathBuf,
    token: String,
    /// `127.0.0.1:port`. Other Host headers are refused, so a web page cannot
    /// reach this server through a rebound DNS name.
    host: String,
}

struct Reply {
    status: u16,
    kind: String,
    body: Vec<u8>,
}

fn reply_json(status: u16, value: &Value) -> Reply {
    Reply {
        status,
        kind: "application/json".to_owned(),
        body: value.to_string().into_bytes(),
    }
}

fn error(status: u16, message: &str) -> Reply {
    reply_json(status, &json!({ "error": message }))
}

/// A node's `filePath` relative to the project, or `None` if it points
/// outside it.
fn relative(root: &Path, file_path: &str) -> Option<String> {
    let path = Path::new(file_path);
    let rel = if path.is_absolute() {
        path.strip_prefix(root).ok()?
    } else {
        path
    };
    let text = rel.to_string_lossy().replace('\\', "/");
    let text = text.trim_start_matches("./");
    (!text.is_empty() && !text.split('/').any(|c| c == "..")).then(|| text.to_owned())
}

fn read_json(path: &Path) -> Option<Value> {
    serde_json::from_slice(&std::fs::read(path).ok()?).ok()
}

/// Every file the graph mentions: the only files a preview may show.
fn graph_files(site: &Site) -> HashSet<String> {
    read_json(&site.data.join("knowledge-graph.json"))
        .and_then(|g| g.get("nodes").cloned())
        .and_then(|n| n.as_array().cloned())
        .unwrap_or_default()
        .iter()
        .filter_map(|n| n.get("filePath")?.as_str().map(str::to_owned))
        .filter_map(|p| relative(&site.root, &p))
        .collect()
}

fn language(path: &str) -> &'static str {
    let ext = path.rsplit('.').next().unwrap_or("").to_ascii_lowercase();
    match ext.as_str() {
        "bash" | "sh" => "bash",
        "c" | "h" => "c",
        "cc" | "cpp" | "hpp" => "cpp",
        "cs" => "csharp",
        "css" => "css",
        "go" => "go",
        "html" => "markup",
        "java" => "java",
        "js" | "mjs" => "javascript",
        "jsx" => "jsx",
        "json" => "json",
        "md" => "markdown",
        "py" => "python",
        "rb" => "ruby",
        "rs" => "rust",
        "ts" => "typescript",
        "tsx" => "tsx",
        "yaml" | "yml" => "yaml",
        _ => "text",
    }
}

fn source_file(site: &Site, requested: &str) -> Reply {
    if requested.is_empty() || requested.contains('\0') || Path::new(requested).is_absolute() {
        return error(400, "Invalid path");
    }
    let Some(rel) = relative(&site.root, requested) else {
        return error(400, "Path must stay inside the project");
    };
    if !graph_files(site).contains(&rel) {
        return error(404, "File is not in the knowledge graph");
    }
    // Also catches a symlink out of the project.
    let Ok(path) = crate::files::resolve(&site.root.to_string_lossy(), &rel) else {
        return error(404, "File not found");
    };
    match std::fs::metadata(&path) {
        Ok(m) if !m.is_file() => return error(400, "Path is not a file"),
        Ok(m) if m.len() > MAX_SOURCE_FILE_BYTES => {
            return error(413, "File is too large to preview");
        }
        Ok(_) => {}
        Err(_) => return error(404, "File not found"),
    }
    let Ok(bytes) = std::fs::read(&path) else {
        return error(404, "File not found");
    };
    if bytes.contains(&0) {
        return error(415, "Binary files cannot be previewed");
    }
    let content = String::from_utf8_lossy(&bytes);
    let lines = if content.is_empty() {
        0
    } else {
        content.replace("\r\n", "\n").split(['\n', '\r']).count()
    };
    reply_json(
        200,
        &json!({
            "path": rel,
            "language": language(&rel),
            "content": content,
            "sizeBytes": bytes.len(),
            "lineCount": lines,
        }),
    )
}

/// A graph file, with absolute paths cut back so the user's folder layout
/// never reaches the page.
fn graph_json(site: &Site, name: &str) -> Reply {
    let path = site.data.join(name);
    if !path.is_file() {
        return if name == "knowledge-graph.json" {
            error(404, "No knowledge graph found. Run /understand first.")
        } else {
            Reply {
                status: 404,
                kind: "text/plain".to_owned(),
                body: Vec::new(),
            }
        };
    }
    let Some(mut graph) = read_json(&path) else {
        return error(500, "Failed to read graph file");
    };
    if let Some(nodes) = graph.get_mut("nodes").and_then(Value::as_array_mut) {
        for node in nodes {
            let Some(file) = node.get("filePath").and_then(Value::as_str) else {
                continue;
            };
            let shown = relative(&site.root, file).unwrap_or_else(|| {
                Path::new(file)
                    .file_name()
                    .map_or_else(String::new, |n| n.to_string_lossy().into_owned())
            });
            node["filePath"] = Value::String(shown);
        }
    }
    reply_json(200, &graph)
}

fn static_file(site: &Site, path: &str) -> Reply {
    let rel = if path == "/" {
        "index.html"
    } else {
        path.trim_start_matches('/')
    };
    if rel.split('/').any(|c| c == ".." || c.is_empty()) {
        return error(403, "Forbidden");
    }
    match (site.assets)(rel) {
        Some((kind, body)) => Reply {
            status: 200,
            kind,
            body,
        },
        None => error(404, "Not found"),
    }
}

/// Answer one request target (path and query).
fn route(site: &Site, target: &str) -> Reply {
    let Ok(url) = tauri::Url::parse(&format!("http://{}{target}", site.host)) else {
        return error(400, "Bad request");
    };
    let param = |key: &str| {
        url.query_pairs()
            .find(|(k, _)| k == key)
            .map(|(_, v)| v.into_owned())
    };
    let path = url.path();
    let protected = [
        "/knowledge-graph.json",
        "/domain-graph.json",
        "/diff-overlay.json",
        "/meta.json",
        "/config.json",
        "/file-content.json",
    ];
    if !protected.contains(&path) {
        return static_file(site, path);
    }
    if param("token").as_deref() != Some(site.token.as_str()) {
        return error(403, "Forbidden: missing or invalid token");
    }
    match path {
        "/file-content.json" => source_file(site, &param("path").unwrap_or_default()),
        "/config.json" => read_json(&site.data.join("config.json")).map_or_else(
            || reply_json(200, &json!({"autoUpdate": false, "outputLanguage": "en"})),
            |c| reply_json(200, &c),
        ),
        _ => graph_json(site, path.trim_start_matches('/')),
    }
}

fn reason(status: u16) -> &'static str {
    match status {
        200 => "OK",
        400 => "Bad Request",
        403 => "Forbidden",
        404 => "Not Found",
        405 => "Method Not Allowed",
        413 => "Payload Too Large",
        415 => "Unsupported Media Type",
        _ => "Internal Server Error",
    }
}

/// Parse a request head: the GET target, if the head is acceptable.
fn request_target(head: &str, host: &str) -> Result<String, u16> {
    let mut lines = head.split("\r\n");
    let mut first = lines.next().unwrap_or("").split(' ');
    let (method, target) = (first.next().unwrap_or(""), first.next().unwrap_or(""));
    let host_ok = lines.any(|l| {
        l.split_once(':')
            .is_some_and(|(k, v)| k.eq_ignore_ascii_case("host") && v.trim() == host)
    });
    if !host_ok {
        return Err(403);
    }
    if method != "GET" {
        return Err(405);
    }
    if !target.starts_with('/') {
        return Err(400);
    }
    Ok(target.to_owned())
}

async fn handle(mut stream: TcpStream, site: Arc<Site>) {
    let mut head = Vec::new();
    let mut buf = [0u8; 4096];
    while !head.windows(4).any(|w| w == b"\r\n\r\n") {
        match stream.read(&mut buf).await {
            Ok(0) | Err(_) => return,
            Ok(n) => head.extend_from_slice(&buf[..n]),
        }
        if head.len() > MAX_REQUEST_BYTES {
            return;
        }
    }
    let head = String::from_utf8_lossy(&head);
    let reply = match request_target(&head, &site.host) {
        Ok(target) => {
            let site = site.clone();
            // File reads are blocking; keep them off the async workers.
            tauri::async_runtime::spawn_blocking(move || route(&site, &target))
                .await
                .unwrap_or_else(|_| error(500, "Internal error"))
        }
        Err(status) => error(status, reason(status)),
    };
    let header = format!(
        "HTTP/1.1 {} {}\r\nContent-Type: {}\r\nContent-Length: {}\r\nCache-Control: no-store\r\nX-Content-Type-Options: nosniff\r\nConnection: close\r\n\r\n",
        reply.status,
        reason(reply.status),
        reply.kind,
        reply.body.len()
    );
    let _ = stream.write_all(header.as_bytes()).await;
    let _ = stream.write_all(&reply.body).await;
    let _ = stream.shutdown().await;
}

/// Serve `project`'s graph and open the dashboard. Replaces any dashboard
/// already open.
pub async fn open(app: &tauri::AppHandle, project: &str) -> Result<String, String> {
    let root = PathBuf::from(project.trim());
    let data = graph_dir(&root).ok_or_else(|| {
        format!(
            "No knowledge graph in {} yet. Understand-Anything's /understand builds one (its .ua folder).",
            root.display()
        )
    })?;

    let state = app.state::<crate::state::AppState>();
    stop(&state);

    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .map_err(|e| format!("could not start the graph viewer: {e}"))?;
    let port = listener.local_addr().map_err(|e| e.to_string())?.port();
    // 160 random bits (rand::rng, seeded from the OS).
    let token = format!(
        "{:020x}{:020x}",
        ulid::Ulid::generate().random(),
        ulid::Ulid::generate().random()
    );
    let resolver = app.asset_resolver();
    let site = Arc::new(Site {
        // Shipped in the app's own assets (public/ua); in dev, read from dist.
        assets: Box::new(move |rel| {
            resolver
                .get(format!("ua/{rel}"))
                .map(|a| (a.mime_type, a.bytes))
        }),
        root: root.clone(),
        data,
        token: token.clone(),
        host: format!("127.0.0.1:{port}"),
    });
    let server = tauri::async_runtime::spawn(async move {
        while let Ok((stream, _)) = listener.accept().await {
            tauri::async_runtime::spawn(handle(stream, site.clone()));
        }
    });
    if let Ok(mut viewer) = state.viewer.lock() {
        *viewer = Some(server);
    }

    let url = format!("http://127.0.0.1:{port}/?token={token}");
    let target: tauri::Url = url.parse().map_err(|e| format!("bad dashboard URL: {e}"))?;
    let title = format!("Knowledge graph — {}", root.display());
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
            // Closing the window stops the server it was showing.
            .on_window_event(move |event| {
                if matches!(event, tauri::WindowEvent::Destroyed) {
                    stop(&handle.state::<crate::state::AppState>());
                }
            });
    }
    Ok(url)
}

/// Stop serving the open dashboard, if any.
pub fn stop(state: &crate::state::AppState) {
    if let Ok(mut viewer) = state.viewer.lock() {
        if let Some(server) = viewer.take() {
            server.abort();
        }
    }
}

/// The dashboard's server, held so it can be stopped.
pub type Viewer = std::sync::Mutex<Option<tauri::async_runtime::JoinHandle<()>>>;

#[cfg(test)]
mod tests {
    // Spec s14 permits unwrap/expect in tests.
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;

    #[test]
    fn the_graph_is_found_in_either_folder() {
        let dir = tempfile::tempdir().unwrap();
        assert!(graph_dir(dir.path()).is_none());
        std::fs::create_dir(dir.path().join(".ua")).unwrap();
        std::fs::write(dir.path().join(".ua/knowledge-graph.json"), "{}").unwrap();
        assert_eq!(graph_dir(dir.path()).unwrap(), dir.path().join(".ua"));
    }

    #[test]
    fn graph_paths_stay_inside_the_project() {
        let root = Path::new(if cfg!(windows) { r"C:\p" } else { "/p" });
        let abs = root.join("src").join("a.rs");
        assert_eq!(
            relative(root, &abs.to_string_lossy()).as_deref(),
            Some("src/a.rs")
        );
        assert_eq!(relative(root, "./src/b.rs").as_deref(), Some("src/b.rs"));
        assert_eq!(relative(root, "src\\c.rs").as_deref(), Some("src/c.rs"));
        assert_eq!(relative(root, "../etc/passwd"), None);
        let elsewhere = if cfg!(windows) {
            r"D:\x\y.rs"
        } else {
            "/x/y.rs"
        };
        assert_eq!(relative(root, elsewhere), None);
    }

    fn site(root: &Path) -> Site {
        Site {
            assets: Box::new(|rel| {
                (rel == "index.html").then(|| ("text/html".to_owned(), b"<html>".to_vec()))
            }),
            root: root.to_path_buf(),
            data: root.join(".ua"),
            token: "t0k".to_owned(),
            host: "127.0.0.1:1".to_owned(),
        }
    }

    #[test]
    fn data_needs_the_token_and_previews_only_graph_files() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        std::fs::create_dir_all(root.join(".ua")).unwrap();
        std::fs::create_dir_all(root.join("src")).unwrap();
        std::fs::write(
            root.join("src/a.rs"),
            "fn a() {}
",
        )
        .unwrap();
        std::fs::write(root.join("secret.txt"), "no").unwrap();
        let abs = root.join("src").join("a.rs").display().to_string();
        let graph = json!({"nodes": [{"id": "file:a", "filePath": abs}]});
        std::fs::write(root.join(".ua/knowledge-graph.json"), graph.to_string()).unwrap();
        let s = site(root);

        assert_eq!(route(&s, "/").status, 200, "the page itself is public");
        assert_eq!(route(&s, "/knowledge-graph.json").status, 403);
        assert_eq!(route(&s, "/knowledge-graph.json?token=bad").status, 403);
        let g = route(&s, "/knowledge-graph.json?token=t0k");
        assert_eq!(g.status, 200);
        let shown: Value = serde_json::from_slice(&g.body).unwrap();
        assert_eq!(
            shown["nodes"][0]["filePath"], "src/a.rs",
            "no absolute paths"
        );

        let ok = route(&s, "/file-content.json?token=t0k&path=src%2Fa.rs");
        assert_eq!(ok.status, 200);
        let body: Value = serde_json::from_slice(&ok.body).unwrap();
        assert_eq!(body["language"], "rust");
        assert_eq!(body["lineCount"], 2, "counted as UA's viewer counts");
        for refused in ["secret.txt", "..%2Fx", "%2Fetc%2Fpasswd"] {
            let r = route(&s, &format!("/file-content.json?token=t0k&path={refused}"));
            assert!(r.status >= 400, "{refused} must be refused");
        }
        assert_eq!(route(&s, "/config.json?token=t0k").status, 200);
        assert_eq!(route(&s, "/assets/..%2F..%2Fsecret.txt").status, 404);
    }

    #[test]
    fn only_a_get_for_this_host_is_answered() {
        let host = "127.0.0.1:5000";
        let get = |h: &str| format!("GET /x HTTP/1.1\r\nHost: {h}\r\n\r\n");
        assert_eq!(request_target(&get(host), host).unwrap(), "/x");
        assert_eq!(request_target(&get("evil.example:5000"), host), Err(403));
        assert_eq!(
            request_target(&format!("POST /x HTTP/1.1\r\nHost: {host}\r\n\r\n"), host),
            Err(405)
        );
    }
}
