//! Language servers and debug adapters: programs spoken to over stdio in JSON
//! messages, each framed by a `Content-Length` header. LSP and DAP share the
//! framing, so one bridge serves both; the webview speaks the protocol.
//!
//! Which program runs is decided here from the file type, never by the page.

use serde::Serialize;
use std::collections::HashMap;
use std::ffi::OsStr;
use std::io::{BufRead, BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{LazyLock, Mutex};

/// The last of a program's stderr kept, to say why it stopped.
const STDERR_TAIL: usize = 4_000;

/// gdb's debug adapter answers reliably from 15 on; 14's times out on launch.
const GDB_MIN: u32 = 15;

/// The major version from `gdb --version`, whose first line ends with it:
/// "GNU gdb (GDB for MinGW-W64 x86_64, built by Brecht Sanders, r8) 14.2".
fn gdb_major(version: &str) -> Option<u32> {
    let last = version.lines().next()?.split_whitespace().last()?;
    last.split(['.', '-']).next()?.parse().ok()
}

/// The newest gdb on `path` that can debug, or `None`. Other software
/// (Strawberry Perl, an old MinGW) often puts an older gdb first on PATH.
pub fn newest_gdb(path: &OsStr) -> Option<PathBuf> {
    ws_tools::find_all("gdb", path)
        .into_iter()
        .filter_map(|gdb| {
            let mut cmd = Command::new(&gdb);
            cmd.arg("--version").stdin(Stdio::null());
            #[cfg(windows)]
            std::os::windows::process::CommandExt::creation_flags(&mut cmd, 0x0800_0000);
            let out = cmd.output().ok()?;
            let major = gdb_major(&String::from_utf8_lossy(&out.stdout))?;
            (major >= GDB_MIN).then_some((major, gdb))
        })
        // Reversed so that of equal versions the first on PATH wins.
        .rev()
        .max_by_key(|(major, _)| *major)
        .map(|(_, gdb)| gdb)
}

fn gdb_on_path() -> Option<PathBuf> {
    newest_gdb(&std::env::var_os("PATH")?)
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum BridgeEvent {
    /// One JSON message from the program, without its header.
    Message { text: String },
    /// The program ended; `stderr` is the end of what it printed there.
    Exit { code: Option<i32>, stderr: String },
}

/// Read one framed message. `Ok(None)` at a clean end of the stream.
fn read_frame(r: &mut impl BufRead) -> std::io::Result<Option<String>> {
    let mut length = None;
    loop {
        let mut line = String::new();
        if r.read_line(&mut line)? == 0 {
            return Ok(None);
        }
        let line = line.trim_end();
        if line.is_empty() {
            if length.is_some() {
                break;
            }
            continue;
        }
        if let Some((name, value)) = line.split_once(':') {
            if name.trim().eq_ignore_ascii_case("content-length") {
                length = value.trim().parse::<usize>().ok();
            }
        }
    }
    let mut body = vec![0; length.unwrap_or(0)];
    r.read_exact(&mut body)?;
    Ok(Some(String::from_utf8_lossy(&body).into_owned()))
}

fn write_frame(w: &mut impl Write, text: &str) -> std::io::Result<()> {
    write!(w, "Content-Length: {}\r\n\r\n{text}", text.len())?;
    w.flush()
}

struct Live {
    stdin: ChildStdin,
    child: Child,
}

static NEXT: AtomicU64 = AtomicU64::new(1);
static LIVE: LazyLock<Mutex<HashMap<u64, Live>>> = LazyLock::new(Mutex::default);

/// Start `cmd` and pass each message it sends to `on_event`, then its exit.
/// Returns the id that `send` and `stop` take.
///
/// # Errors
/// The program could not be started.
pub fn start(
    mut cmd: Command,
    on_event: impl Fn(BridgeEvent) + Send + Sync + 'static,
) -> Result<u64, String> {
    cmd.stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    #[cfg(windows)]
    std::os::windows::process::CommandExt::creation_flags(&mut cmd, 0x0800_0000);
    let mut child = cmd
        .spawn()
        .map_err(|e| format!("could not start {:?}: {e}", cmd.get_program()))?;
    let (Some(stdin), Some(stdout), Some(stderr)) =
        (child.stdin.take(), child.stdout.take(), child.stderr.take())
    else {
        return Err("the program's pipes were not available".to_owned());
    };
    let id = NEXT.fetch_add(1, Ordering::SeqCst);
    LIVE.lock()
        .map_err(|_| "bridge unavailable")?
        .insert(id, Live { stdin, child });

    let tail = std::sync::Arc::new(Mutex::new(String::new()));
    let keep = tail.clone();
    std::thread::spawn(move || {
        let mut buf = [0u8; 1024];
        let mut stderr = stderr;
        while let Ok(n) = stderr.read(&mut buf) {
            if n == 0 {
                break;
            }
            if let Ok(mut t) = keep.lock() {
                t.push_str(&String::from_utf8_lossy(&buf[..n]));
                if t.len() > STDERR_TAIL {
                    let cut = t.len() - STDERR_TAIL;
                    let cut = (cut..t.len()).find(|i| t.is_char_boundary(*i)).unwrap_or(0);
                    t.drain(..cut);
                }
            }
        }
    });
    std::thread::spawn(move || {
        let mut out = BufReader::new(stdout);
        while let Ok(Some(text)) = read_frame(&mut out) {
            on_event(BridgeEvent::Message { text });
        }
        let code = LIVE
            .lock()
            .ok()
            .and_then(|mut m| m.remove(&id))
            .and_then(|mut l| l.child.wait().ok())
            .and_then(|s| s.code());
        // The stderr reader ends with the program; give it a moment to finish.
        std::thread::sleep(std::time::Duration::from_millis(100));
        let stderr = tail.lock().map(|t| t.clone()).unwrap_or_default();
        on_event(BridgeEvent::Exit { code, stderr });
    });
    Ok(id)
}

/// Send one JSON message to the program behind `id`.
///
/// # Errors
/// The program has ended or its input is closed.
pub fn send(id: u64, text: &str) -> Result<(), String> {
    let mut live = LIVE.lock().map_err(|_| "bridge unavailable")?;
    let l = live
        .get_mut(&id)
        .ok_or("that program is no longer running")?;
    write_frame(&mut l.stdin, text).map_err(|e| e.to_string())
}

/// Stop the program behind `id`, if it is still running.
pub fn stop(id: u64) {
    if let Some(mut l) = LIVE.lock().ok().and_then(|mut m| m.remove(&id)) {
        let _ = l.child.kill();
    }
}

/// Stop every program still running. For when the app quits: rust-analyzer,
/// for one, keeps running after its input closes.
pub fn stop_all() {
    let all: Vec<Live> = LIVE
        .lock()
        .map(|mut m| m.drain().map(|(_, l)| l).collect())
        .unwrap_or_default();
    for mut l in all {
        let _ = l.child.kill();
    }
}

/// `C:\x` for `\\?\C:\x`: language servers and gdb want plain paths.
fn plain(path: &Path) -> PathBuf {
    let text = path.to_string_lossy();
    match text.strip_prefix(r"\\?\") {
        Some(rest) if rest.as_bytes().get(1) == Some(&b':') => PathBuf::from(rest),
        _ => path.to_path_buf(),
    }
}

/// Programs to try, in order, each with its arguments.
type Candidates = &'static [(&'static str, &'static [&'static str])];

/// The language server for a file type: candidates in order of preference,
/// each a program and its arguments, and what to install when none is found.
fn server_for(ext: &str) -> Option<(Candidates, &'static str)> {
    Some(match ext {
        "rs" => (
            &[("rust-analyzer", &[])],
            "rust-analyzer (rustup component add rust-analyzer)",
        ),
        "c" | "h" | "cpp" | "cc" | "cxx" | "hpp" | "hh" | "hxx" => {
            (&[("clangd", &[])], "clangd (it comes with LLVM)")
        }
        "py" => (
            &[("pyright-langserver", &["--stdio"]), ("pylsp", &[])],
            "pyright (npm i -g pyright) or python-lsp-server (pip install python-lsp-server)",
        ),
        "ts" | "tsx" | "js" | "jsx" | "mjs" | "cjs" | "mts" | "cts" => (
            &[
                // TypeScript 7 has its own server; older ones need the wrapper.
                ("tsc", &["--lsp", "--stdio"]),
                ("typescript-language-server", &["--stdio"]),
            ],
            "TypeScript 7 (npm i -g typescript) or typescript-language-server",
        ),
        "go" => (
            &[("gopls", &[])],
            "gopls (go install golang.org/x/tools/gopls@latest)",
        ),
        _ => return None,
    })
}

/// `name` on PATH, or where its usual installer puts it without adding it to
/// PATH (LLVM's Windows installer does that by default).
fn installed(name: &str) -> Option<PathBuf> {
    ws_tools::find(&[name]).or_else(|| {
        let program_files = std::env::var_os("ProgramFiles")?;
        let path = match name {
            "clangd" | "lldb-dap" => {
                Path::new(&program_files).join(format!(r"LLVM\bin\{name}.exe"))
            }
            _ => return None,
        };
        path.is_file().then_some(path)
    })
}

/// The major version of the TypeScript compiler at `tsc`; 0 if it will not say.
fn ts_major(tsc: &Path) -> u32 {
    let mut cmd = Command::new(tsc);
    cmd.arg("--version").stdin(Stdio::null());
    #[cfg(windows)]
    std::os::windows::process::CommandExt::creation_flags(&mut cmd, 0x0800_0000);
    cmd.output()
        .ok()
        .and_then(|o| {
            let text = String::from_utf8_lossy(&o.stdout).into_owned();
            // "Version 7.0.2"
            text.split_whitespace()
                .nth(1)?
                .split('.')
                .next()?
                .parse()
                .ok()
        })
        .unwrap_or(0)
}

/// The command that starts the language server for `.ext` files in `root`.
///
/// # Errors
/// No server is known for the type, or none is installed.
pub fn language_server(root: &Path, ext: &str) -> Result<Command, String> {
    let (candidates, install) =
        server_for(ext).ok_or_else(|| format!("no language server is known for .{ext} files"))?;
    let (program, args) = candidates
        .iter()
        .find_map(|(name, args)| {
            installed(name)
                .filter(|p| *name != "tsc" || ts_major(p) >= 7)
                .map(|p| (p, *args))
        })
        .ok_or_else(|| {
            format!("no language server for .{ext} files is installed; install {install}")
        })?;
    let mut cmd = Command::new(program);
    cmd.args(args).current_dir(plain(root));
    Ok(cmd)
}

/// How a debug session starts: the adapter to run, and the `launch` request's
/// arguments for it, which the page sends unchanged.
pub struct Debug {
    pub adapter: Command,
    pub launch: serde_json::Value,
}

/// Prepare to debug `file`: build it with debug information if it needs
/// building, and choose the adapter. Blocks while compiling.
///
/// # Errors
/// The type cannot be debugged here, a tool is missing, or the build failed.
pub fn debugger(file: &Path) -> Result<Debug, String> {
    let file = plain(file);
    let ext = file
        .extension()
        .and_then(|e| e.to_str())
        .map(str::to_ascii_lowercase)
        .unwrap_or_default();
    let cwd = file.parent().map(Path::to_path_buf).unwrap_or_default();
    match ext.as_str() {
        "c" | "cpp" | "cc" | "cxx" => {
            let compilers: &[&str] = if ext == "c" {
                &["gcc", "clang"]
            } else {
                &["g++", "clang++"]
            };
            let cc =
                ws_tools::find(compilers).ok_or("no C/C++ compiler is installed (gcc or clang)")?;
            let gdb = gdb_on_path().ok_or(
                "debugging C and C++ needs gdb 15 or newer; install the C and C++ language pack in Settings",
            )?;
            let exe = file.with_extension(std::env::consts::EXE_EXTENSION);
            build(
                Command::new(cc)
                    .arg(&file)
                    .args(["-g", "-O0", "-o"])
                    .arg(&exe)
                    .current_dir(&cwd),
            )?;
            let mut adapter = Command::new(gdb);
            adapter.args(["-q", "-i", "dap"]).current_dir(&cwd);
            // The program's own output would otherwise mix into gdb's protocol
            // stream; on Windows it gets a console window of its own instead.
            if cfg!(windows) {
                adapter.args(["-iex", "set new-console on"]);
            }
            Ok(Debug {
                adapter,
                launch: serde_json::json!({ "program": exe, "cwd": cwd }),
            })
        }
        "py" => {
            let python =
                ws_tools::find(&["python", "python3", "py"]).ok_or("Python is not installed")?;
            let has = Command::new(&python)
                .args(["-c", "import debugpy"])
                .stdin(Stdio::null())
                .output()
                .is_ok_and(|o| o.status.success());
            if !has {
                return Err(
                    "debugging Python needs debugpy; install it with: pip install debugpy"
                        .to_owned(),
                );
            }
            let mut adapter = Command::new(python);
            adapter.args(["-m", "debugpy.adapter"]).current_dir(&cwd);
            Ok(Debug {
                adapter,
                launch: serde_json::json!({
                    "program": file, "cwd": cwd, "console": "internalConsole", "justMyCode": true
                }),
            })
        }
        "rs" => {
            let lldb = installed("lldb-dap").ok_or(
                "debugging Rust needs lldb-dap, which comes with LLVM: winget install LLVM.LLVM",
            )?;
            let exe = rust_build(&file, &cwd)?;
            let mut adapter = Command::new(lldb);
            adapter.current_dir(&cwd);
            Ok(Debug {
                adapter,
                launch: serde_json::json!({ "program": exe, "cwd": cwd }),
            })
        }
        _ => Err(format!(
            "Workspace can debug .c, .cpp, .rs and .py files, not .{ext} files"
        )),
    }
}

/// Run a build; its output on success, the compiler's messages on failure.
fn build(cmd: &mut Command) -> Result<std::process::Output, String> {
    cmd.stdin(Stdio::null());
    #[cfg(windows)]
    std::os::windows::process::CommandExt::creation_flags(cmd, 0x0800_0000);
    let out = cmd
        .output()
        .map_err(|e| format!("could not run the compiler: {e}"))?;
    if !out.status.success() {
        return Err(format!(
            "the build failed:\n{}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        ));
    }
    Ok(out)
}

/// Build the program `file` belongs to, with debug information: its Cargo
/// package's binary if it is in one, otherwise the file alone with rustc.
fn rust_build(file: &Path, cwd: &Path) -> Result<PathBuf, String> {
    let manifest = file
        .ancestors()
        .skip(1)
        .map(|d| d.join("Cargo.toml"))
        .find(|m| m.is_file());
    let Some(manifest) = manifest else {
        let rustc = ws_tools::find(&["rustc"]).ok_or("Rust is not installed")?;
        let exe = file.with_extension(std::env::consts::EXE_EXTENSION);
        build(
            Command::new(rustc)
                .arg(file)
                .args(["-g", "-o"])
                .arg(&exe)
                .current_dir(cwd),
        )?;
        return Ok(exe);
    };
    let cargo = ws_tools::find(&["cargo"]).ok_or("Rust is not installed")?;
    let out = build(
        Command::new(cargo)
            .args([
                "build",
                "--message-format=json-render-diagnostics",
                "--manifest-path",
            ])
            .arg(&manifest)
            .current_dir(cwd),
    )?;
    // (the source of its main, the executable) for each binary built.
    let bins: Vec<(PathBuf, PathBuf)> = String::from_utf8_lossy(&out.stdout)
        .lines()
        .filter_map(|line| {
            let v: serde_json::Value = serde_json::from_str(line).ok()?;
            let is_bin = v["target"]["kind"].as_array()?.iter().any(|k| k == "bin");
            let exe = v["executable"].as_str().filter(|_| is_bin)?;
            Some((v["target"]["src_path"].as_str()?.into(), exe.into()))
        })
        .collect();
    let same = |a: &Path, b: &Path| a.canonicalize().ok() == b.canonicalize().ok();
    if let Some((_, exe)) = bins.iter().find(|(src, _)| same(src, file)) {
        return Ok(exe.clone());
    }
    match bins.as_slice() {
        [(_, exe)] => Ok(exe.clone()),
        [] => Err("the package has no binary to debug".to_owned()),
        _ => Err("the package has several binaries; start debugging from the file with the main you want".to_owned()),
    }
}

#[cfg(test)]
mod tests {
    // Tests may unwrap and expect: a panic is the failure report.
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;

    #[test]
    fn gdb_versions_are_read_from_the_end_of_the_first_line() {
        let mingw =
            "GNU gdb (GDB for MinGW-W64 x86_64, built by Brecht Sanders, r8) 14.2\nCopyright";
        assert_eq!(gdb_major(mingw), Some(14));
        assert_eq!(
            gdb_major("GNU gdb (Ubuntu 15.0.50.20240403-0ubuntu1) 15.0.50.20240403-git"),
            Some(15)
        );
        assert_eq!(gdb_major("GNU gdb (Fedora Linux) 14.2-1.fc40"), Some(14));
        assert_eq!(gdb_major("not gdb"), None);
        assert_eq!(gdb_major(""), None);
    }

    #[test]
    fn frames_round_trip_and_extra_headers_are_ignored() {
        let mut buf = Vec::new();
        write_frame(&mut buf, r#"{"id":1}"#).unwrap();
        buf.extend_from_slice(
            b"Content-Type: application/vscode-jsonrpc\r\ncontent-length: 7\r\n\r\n{\"a\":2}",
        );
        write_frame(&mut buf, "é").unwrap(); // the length is in bytes
        let mut r = std::io::Cursor::new(buf);
        assert_eq!(read_frame(&mut r).unwrap().as_deref(), Some(r#"{"id":1}"#));
        assert_eq!(read_frame(&mut r).unwrap().as_deref(), Some(r#"{"a":2}"#));
        assert_eq!(read_frame(&mut r).unwrap().as_deref(), Some("é"));
        assert_eq!(read_frame(&mut r).unwrap(), None, "a clean end");
    }

    #[test]
    fn a_stream_cut_off_mid_message_is_an_error_not_a_message() {
        let mut r = std::io::Cursor::new(b"Content-Length: 10\r\n\r\nabc".to_vec());
        assert!(read_frame(&mut r).is_err());
    }

    #[test]
    fn only_known_file_types_get_a_server() {
        assert!(server_for("rs").is_some() && server_for("tsx").is_some());
        assert!(server_for("txt").is_none());
        let err = language_server(Path::new("."), "txt").unwrap_err();
        assert!(err.contains("no language server is known"), "{err}");
        assert!(debugger(Path::new("notes.txt")).is_err());
    }

    #[test]
    fn a_program_is_spoken_to_and_its_exit_reported() {
        // A one-message echo server, in Python, standing in for a real one.
        let Some(python) = ws_tools::find(&["python", "python3"]) else {
            return;
        };
        let script = r#"
import sys
h = sys.stdin.buffer.readline(); sys.stdin.buffer.readline()
n = int(h.split(b':')[1]); body = sys.stdin.buffer.read(n)
sys.stdout.buffer.write(b'Content-Length: %d\r\n\r\n' % len(body) + body); sys.stdout.flush()
sys.stderr.write('bye'); sys.exit(3)
"#;
        let mut cmd = Command::new(python);
        cmd.args(["-c", script]);
        let (tx, rx) = std::sync::mpsc::channel();
        let id = start(cmd, move |e| {
            let _ = tx.send(e);
        })
        .unwrap();
        send(id, r#"{"jsonrpc":"2.0","method":"ping"}"#).unwrap();
        let timeout = std::time::Duration::from_secs(10);
        assert_eq!(
            rx.recv_timeout(timeout).unwrap(),
            BridgeEvent::Message {
                text: r#"{"jsonrpc":"2.0","method":"ping"}"#.to_owned()
            }
        );
        assert_eq!(
            rx.recv_timeout(timeout).unwrap(),
            BridgeEvent::Exit {
                code: Some(3),
                stderr: "bye".to_owned()
            }
        );
        assert!(send(id, "{}").is_err(), "gone once it has exited");
    }

    #[test]
    fn a_c_file_is_built_with_debug_information_for_gdb() {
        if ws_tools::find(&["gcc"]).is_none() || gdb_on_path().is_none() {
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("m.c");
        std::fs::write(&src, "int main(void) { return 0; }\n").unwrap();
        let d = debugger(&src).unwrap();
        let program = d.launch["program"].as_str().unwrap().to_owned();
        assert!(Path::new(&program).is_file(), "built {program}");

        std::fs::write(&src, "int main(void) { return }\n").unwrap();
        let err = debugger(&src).err().unwrap();
        assert!(err.starts_with("the build failed"), "{err}");
    }

    #[test]
    fn gdb_stops_at_a_breakpoint_and_reports_the_frame() {
        if ws_tools::find(&["gcc"]).is_none() || gdb_on_path().is_none() {
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("b.c");
        std::fs::write(
            &src,
            "int main(void) {
  int x = 41;
  x++;
  return x;
}
",
        )
        .unwrap();
        let top = stops_at(debugger(&src).unwrap(), &src, 3);
        assert_eq!(top["name"], "main", "{top}");
    }

    #[test]
    fn lldb_stops_at_a_breakpoint_in_a_cargo_binary_and_a_lone_file() {
        // CI images ship an lldb-dap that exits at once; this checks a real install.
        if installed("lldb-dap").is_none()
            || ws_tools::find(&["cargo"]).is_none()
            || std::env::var_os("CI").is_some()
        {
            return;
        }
        let code = "fn main() {\n    let mut x = 41;\n    x += 1;\n    println!(\"{x}\");\n}\n";
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("Cargo.toml"),
            "[package]\nname = \"t\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
        )
        .unwrap();
        std::fs::create_dir(dir.path().join("src")).unwrap();
        let src = dir.path().join("src").join("main.rs");
        std::fs::write(&src, code).unwrap();
        let top = stops_at(debugger(&src).unwrap(), &src, 3);
        assert!(top["name"].as_str().unwrap().contains("main"), "{top}");

        let lone = tempfile::tempdir().unwrap();
        let src = lone.path().join("one.rs");
        std::fs::write(&src, code).unwrap();
        stops_at(debugger(&src).unwrap(), &src, 3);

        std::fs::write(&src, "fn main() { let }\n").unwrap();
        let err = debugger(&src).err().unwrap();
        assert!(err.starts_with("the build failed"), "{err}");
    }

    #[test]
    fn debugpy_stops_at_a_breakpoint() {
        if debugger(Path::new("x.py")).is_err() {
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("p.py");
        std::fs::write(&src, "x = 41\nx += 1\nprint(x)\n").unwrap();
        let top = stops_at(debugger(&src).unwrap(), &src, 2);
        assert_eq!(top["name"], "<module>", "{top}");
    }

    /// Run `d` with a breakpoint at `line` of `src`; the top frame once it stops there.
    fn stops_at(d: Debug, src: &Path, line: u32) -> serde_json::Value {
        let (tx, rx) = std::sync::mpsc::channel();
        let id = start(d.adapter, move |e| {
            let _ = tx.send(e);
        })
        .unwrap();
        let req = |seq: u32, command: &str, args: serde_json::Value| {
            send(id, &serde_json::json!({"seq": seq, "type": "request", "command": command, "arguments": args}).to_string()).unwrap();
        };
        // Wait for a message matching `want`, failing on exit or after 30 s.
        let wait = |want: &dyn Fn(&serde_json::Value) -> bool| -> serde_json::Value {
            loop {
                match rx
                    .recv_timeout(std::time::Duration::from_secs(30))
                    .expect("the adapter answered")
                {
                    BridgeEvent::Message { text } => {
                        let v: serde_json::Value = serde_json::from_str(&text).unwrap();
                        if want(&v) {
                            return v;
                        }
                    }
                    BridgeEvent::Exit { stderr, .. } => panic!("the adapter exited: {stderr}"),
                }
            }
        };
        req(
            1,
            "initialize",
            serde_json::json!({"adapterID": "ws", "linesStartAt1": true, "columnsStartAt1": true, "pathFormat": "path"}),
        );
        wait(&|v| v["command"] == "initialize");
        req(2, "launch", d.launch.clone());
        wait(&|v| {
            v["event"] == "initialized" || (v["command"] == "launch" && v["success"] == false)
        });
        req(
            3,
            "setBreakpoints",
            serde_json::json!({"source": {"path": src}, "breakpoints": [{"line": line}]}),
        );
        let bp = wait(&|v| v["command"] == "setBreakpoints");
        assert_eq!(bp["success"], true, "{bp}");
        req(4, "configurationDone", serde_json::json!({}));
        let stopped = wait(&|v| v["event"] == "stopped");
        let thread = stopped["body"]["threadId"].clone();
        req(5, "stackTrace", serde_json::json!({"threadId": thread}));
        let trace = wait(&|v| v["command"] == "stackTrace");
        let top = trace["body"]["stackFrames"][0].clone();
        assert_eq!(top["line"], line, "{trace}");
        // Every program stops after `x = 41`, before it changes.
        req(6, "scopes", serde_json::json!({"frameId": top["id"]}));
        let scopes = wait(&|v| v["command"] == "scopes");
        let vars = scopes["body"]["scopes"][0]["variablesReference"].clone();
        req(
            7,
            "variables",
            serde_json::json!({"variablesReference": vars}),
        );
        let vars = wait(&|v| v["command"] == "variables");
        let x = vars["body"]["variables"]
            .as_array()
            .unwrap()
            .iter()
            .find(|v| v["name"] == "x")
            .unwrap_or_else(|| panic!("no x in {vars}"));
        assert_eq!(x["value"], "41", "{vars}");
        stop(id);
        top
    }

    #[test]
    fn rust_analyzer_reports_an_error_and_answers_hover() {
        let dir = tempfile::tempdir().unwrap();
        let Ok(cmd) = language_server(dir.path(), "rs") else {
            return;
        };
        // The rustup shim is on PATH even without the component.
        let runs = Command::new(cmd.get_program())
            .arg("--version")
            .output()
            .is_ok_and(|o| o.status.success());
        if !runs {
            return;
        }
        std::fs::write(
            dir.path().join("Cargo.toml"),
            "[package]\nname = \"t\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
        )
        .unwrap();
        std::fs::create_dir(dir.path().join("src")).unwrap();
        let text = "fn main() {\n    let n: u32 = 5;\n    let s: String = n;\n}\n";
        std::fs::write(dir.path().join("src/main.rs"), text).unwrap();
        let root = plain(&dir.path().canonicalize().unwrap())
            .display()
            .to_string()
            .replace('\\', "/");
        let uri = format!("file:///{root}/src/main.rs");

        let (tx, rx) = std::sync::mpsc::channel();
        let id = start(language_server(dir.path(), "rs").unwrap(), move |e| {
            tx.send(e).unwrap()
        })
        .unwrap();
        let msg = |v: serde_json::Value| send(id, &v.to_string()).unwrap();
        let wait = |want: &dyn Fn(&serde_json::Value) -> bool| -> serde_json::Value {
            loop {
                match rx
                    .recv_timeout(std::time::Duration::from_secs(180))
                    .expect("rust-analyzer answered")
                {
                    BridgeEvent::Message { text } => {
                        let v: serde_json::Value = serde_json::from_str(&text).unwrap();
                        if want(&v) {
                            return v;
                        }
                    }
                    BridgeEvent::Exit { stderr, .. } => panic!("rust-analyzer exited: {stderr}"),
                }
            }
        };
        msg(
            serde_json::json!({"jsonrpc": "2.0", "id": 1, "method": "initialize",
            "params": {"processId": null, "rootUri": format!("file:///{root}"), "capabilities": {}}}),
        );
        wait(&|v| v["id"] == 1);
        msg(serde_json::json!({"jsonrpc": "2.0", "method": "initialized", "params": {}}));
        msg(
            serde_json::json!({"jsonrpc": "2.0", "method": "textDocument/didOpen",
            "params": {"textDocument": {"uri": uri, "languageId": "rust", "version": 1, "text": text}}}),
        );
        let diags = wait(&|v| {
            v["method"] == "textDocument/publishDiagnostics"
                && v["params"]["diagnostics"]
                    .as_array()
                    .is_some_and(|d| !d.is_empty())
        });
        assert_eq!(
            diags["params"]["diagnostics"][0]["range"]["start"]["line"], 2,
            "{diags}"
        );
        // Hover over `n` on line 2 (0-based 1), character 8.
        msg(
            serde_json::json!({"jsonrpc": "2.0", "id": 2, "method": "textDocument/hover",
            "params": {"textDocument": {"uri": uri}, "position": {"line": 1, "character": 8}}}),
        );
        let hover = wait(&|v| v["id"] == 2);
        assert!(hover.to_string().contains("u32"), "{hover}");
        stop(id);
    }

    #[test]
    fn every_installed_language_server_answers_initialize() {
        let dir = tempfile::tempdir().unwrap();
        let root = plain(&dir.path().canonicalize().unwrap())
            .display()
            .to_string()
            .replace('\\', "/");
        for ext in ["rs", "c", "py", "ts", "go"] {
            let Ok(cmd) = language_server(dir.path(), ext) else {
                continue;
            };
            let (tx, rx) = std::sync::mpsc::channel();
            let id = start(cmd, move |e| {
                let _ = tx.send(e);
            })
            .unwrap();
            send(id, &serde_json::json!({"jsonrpc": "2.0", "id": 1, "method": "initialize",
                "params": {"processId": null, "rootUri": format!("file:///{root}"), "capabilities": {}}}).to_string()).unwrap();
            let answer = loop {
                match rx.recv_timeout(std::time::Duration::from_secs(60)) {
                    Ok(BridgeEvent::Message { text }) if text.contains("\"id\":1") => break text,
                    Ok(BridgeEvent::Message { .. }) => {}
                    // The rustup shim is on PATH even without its component.
                    Ok(BridgeEvent::Exit { stderr, .. })
                        if ext == "rs"
                            && (stderr.contains("rustup") || stderr.contains("Unknown binary")) =>
                    {
                        break String::new();
                    }
                    other => panic!(".{ext}: no answer to initialize: {other:?}"),
                }
            };
            assert!(
                answer.is_empty() || answer.contains("capabilities"),
                ".{ext}: {answer}"
            );
            stop(id);
        }
    }
}
