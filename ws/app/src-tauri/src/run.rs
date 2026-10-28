//! Compile and run one file: C, C++, Python, Rust or LaTeX.
//!
//! Each run gets a pseudo-terminal (ConPTY on Windows) rather than pipes, so
//! a program that prints a prompt and reads input behaves as it would in a
//! console: C's stdout is not held in a buffer until exit, and `input()` and
//! `scanf` get what the user types.
//!
//! Compilers are found on PATH when the run starts. Workspace ships none.

use portable_pty::{ChildKiller, CommandBuilder, MasterPty, PtySize, native_pty_system};
use serde::Serialize;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use tauri::ipc::Channel;

#[derive(Debug, Clone, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum RunEvent {
    /// Terminal output, escape codes included.
    Output { data: String },
    /// The run is over. `code` is the last step's exit code, if it ran.
    Exit { code: Option<u32>, stopped: bool },
}

/// One program to start, in order. A run stops at the first that fails.
#[derive(Debug, PartialEq, Eq)]
pub struct Step {
    pub program: PathBuf,
    pub args: Vec<String>,
}

#[derive(Debug, PartialEq, Eq)]
pub struct Plan {
    pub cwd: PathBuf,
    pub steps: Vec<Step>,
}

const NO_C: &str = "No C compiler found. Install one, for example\r\n  winget install -e --id BrechtSanders.WinLibs.POSIX.UCRT\r\nthen restart Workspace.";
const NO_CPP: &str = "No C++ compiler found. Install one, for example\r\n  winget install -e --id BrechtSanders.WinLibs.POSIX.UCRT\r\nthen restart Workspace.";
const NO_TEX: &str = "No LaTeX found. Install one, for example\r\n  winget install -e --id MiKTeX.MiKTeX\r\nthen restart Workspace.";
const NO_PYTHON: &str = "No Python found. Install it from python.org, then restart Workspace.";
const NO_RUST: &str = "No Rust found. Install it from rustup.rs, then restart Workspace.";
const NO_NODE: &str = "No Node.js found. Install it from nodejs.org, then restart Workspace.";

/// The first of `names` on PATH.
pub(crate) fn find(names: &[&str]) -> Option<PathBuf> {
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

fn step(program: PathBuf, args: &[&str]) -> Step {
    Step {
        program,
        args: args.iter().map(|a| (*a).to_owned()).collect(),
    }
}

/// The folder of the nearest `Cargo.toml` above `file`, without leaving `root`.
fn cargo_dir(root: &Path, file: &Path) -> Option<PathBuf> {
    file.ancestors()
        .skip(1)
        .take_while(|d| d.starts_with(root))
        .find(|d| d.join("Cargo.toml").is_file())
        .map(Path::to_path_buf)
}

/// How to build and run `file` (canonical, inside the canonical `root`).
///
/// # Errors
/// The file type has no runner, or its compiler is not installed.
pub fn plan(root: &Path, file: &Path) -> Result<Plan, String> {
    plan_with(root, file, find)
}

/// `C:\x` for `\\?\C:\x`. Canonical paths carry that prefix on Windows, and
/// MinGW's gcc and TeX cannot open files named with it.
fn plain(path: &Path) -> PathBuf {
    let text = path.to_string_lossy();
    match text.strip_prefix(r"\\?\") {
        Some(rest) if rest.as_bytes().get(1) == Some(&b':') => PathBuf::from(rest),
        _ => path.to_path_buf(),
    }
}

fn plan_with(
    root: &Path,
    file: &Path,
    find: impl Fn(&[&str]) -> Option<PathBuf>,
) -> Result<Plan, String> {
    let (root, file) = (&plain(root), &plain(file));
    let ext = file
        .extension()
        .and_then(|e| e.to_str())
        .map(str::to_ascii_lowercase)
        .unwrap_or_default();
    let cwd = file.parent().unwrap_or(root).to_path_buf();
    let src = file.display().to_string();
    // Built programs go beside their source, as most IDEs leave them.
    let exe = file.with_extension(std::env::consts::EXE_EXTENSION);
    let out = exe.display().to_string();
    let name = file
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();

    let steps = match ext.as_str() {
        "py" => vec![step(
            find(&["python", "python3", "py"]).ok_or(NO_PYTHON)?,
            &[&src],
        )],
        // shortcut: Node strips TypeScript types itself but rejects enums and namespaces; add a compiler step if those are needed.
        "js" | "mjs" | "cjs" | "ts" | "mts" | "cts" => {
            vec![step(find(&["node"]).ok_or(NO_NODE)?, &[&src])]
        }
        "c" => vec![
            step(
                find(&["gcc", "clang"]).ok_or(NO_C)?,
                &[&src, "-Wall", "-o", &out],
            ),
            step(exe, &[]),
        ],
        "cpp" | "cc" | "cxx" => vec![
            step(
                find(&["g++", "clang++"]).ok_or(NO_CPP)?,
                &[&src, "-Wall", "-o", &out],
            ),
            step(exe, &[]),
        ],
        "rs" => {
            let cargo = find(&["cargo"]).ok_or(NO_RUST)?;
            if let Some(dir) = cargo_dir(root, file) {
                return Ok(Plan {
                    cwd: dir,
                    steps: vec![step(cargo, &["run"])],
                });
            }
            vec![
                step(
                    find(&["rustc"]).ok_or(NO_RUST)?,
                    &["--edition", "2021", &src, "-o", &out],
                ),
                step(exe, &[]),
            ]
        }
        "tex" => {
            if let Some(tectonic) = find(&["tectonic"]) {
                vec![step(tectonic, &[&name])]
            } else {
                let pdflatex = find(&["pdflatex"]).ok_or(NO_TEX)?;
                let args = ["-interaction=nonstopmode", "-halt-on-error", name.as_str()];
                // The second pass fills in references and the contents page.
                vec![step(pdflatex.clone(), &args), step(pdflatex, &args)]
            }
        }
        _ => {
            return Err(format!(
                "Workspace can run .c, .cpp, .py, .rs, .js, .ts and .tex files, not .{ext} files."
            ));
        }
    };
    Ok(Plan { cwd, steps })
}

/// The user's shell, interactive, in `root`.
///
/// # Errors
/// No shell was found.
pub fn terminal(root: &Path) -> Result<Plan, String> {
    let shell = if cfg!(windows) {
        find(&["pwsh", "powershell", "cmd"])
    } else {
        find(&["bash", "sh"])
    }
    .ok_or("No shell found.")?;
    let args: &[&str] = if shell.file_stem().is_some_and(|s| s != "cmd") && cfg!(windows) {
        &["-NoLogo"]
    } else {
        &[]
    };
    Ok(Plan {
        cwd: plain(root),
        steps: vec![step(shell, args)],
    })
}

/// A package name as pip accepts one, optionally with extras and a version:
/// `numpy`, `requests[socks]`, `pandas==2.2.3`. Nothing that a shell, or pip,
/// would read as another option or a path.
fn is_package(spec: &str) -> bool {
    !spec.is_empty()
        && spec.len() <= 100
        && spec
            .chars()
            .next()
            .is_some_and(|c| c.is_ascii_alphanumeric())
        && spec
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || "._-[],=<>!~".contains(c))
}

/// `python -m pip install <package>`, with the Python that runs `.py` files.
///
/// # Errors
/// The name is not a plain package spec, or Python is not installed.
pub fn pip_install(root: &Path, package: &str) -> Result<Plan, String> {
    let package = package.trim();
    if !is_package(package) {
        return Err(format!("{package} is not a package name"));
    }
    let python = find(&["python", "python3", "py"]).ok_or(NO_PYTHON)?;
    Ok(Plan {
        cwd: plain(root),
        steps: vec![step(python, &["-m", "pip", "install", package])],
    })
}

/// The longest valid UTF-8 prefix of `buf`, leaving a character cut off by
/// the end of a read for the next one.
fn take_utf8(buf: &mut Vec<u8>) -> String {
    let valid = match std::str::from_utf8(buf) {
        Ok(_) => buf.len(),
        Err(e) if e.error_len().is_none() => e.valid_up_to(),
        Err(_) => buf.len(),
    };
    let rest = buf.split_off(valid);
    let text = String::from_utf8_lossy(buf).into_owned();
    *buf = rest;
    text
}

struct Active {
    id: u64,
    master: Box<dyn MasterPty + Send>,
    writer: Box<dyn Write + Send>,
    killer: Arc<Mutex<Option<Box<dyn ChildKiller + Send + Sync>>>>,
    stopped: Arc<AtomicBool>,
}

impl Active {
    fn stop(&self) {
        self.stopped.store(true, Ordering::SeqCst);
        if let Ok(mut k) = self.killer.lock() {
            if let Some(k) = k.as_mut() {
                let _ = k.kill();
            }
        }
    }
}

/// The one program running in the output panel. Starting another stops it.
#[derive(Default)]
pub struct Runner {
    current: Arc<Mutex<Option<Active>>>,
    next_id: AtomicU64,
}

impl std::fmt::Debug for Runner {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Runner").finish_non_exhaustive()
    }
}

fn shown(step: &Step) -> String {
    let name = step
        .program
        .file_stem()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    std::iter::once(name)
        .chain(step.args.iter().cloned())
        .collect::<Vec<_>>()
        .join(" ")
}

impl Runner {
    /// Start `plan`, streaming the terminal to `on_event`.
    ///
    /// # Errors
    /// The pseudo-terminal could not be opened.
    pub fn start(&self, plan: Plan, on_event: Channel<RunEvent>) -> Result<(), String> {
        let pair = native_pty_system()
            .openpty(PtySize {
                rows: 24,
                cols: 100,
                pixel_width: 0,
                pixel_height: 0,
            })
            .map_err(|e| e.to_string())?;
        let mut reader = pair.master.try_clone_reader().map_err(|e| e.to_string())?;
        let writer = pair.master.take_writer().map_err(|e| e.to_string())?;
        let id = self.next_id.fetch_add(1, Ordering::SeqCst);
        let killer = Arc::new(Mutex::new(None));
        let stopped = Arc::new(AtomicBool::new(false));
        {
            let mut current = self.current.lock().map_err(|_| "runner unavailable")?;
            if let Some(old) = current.take() {
                old.stop();
            }
            *current = Some(Active {
                id,
                master: pair.master,
                writer,
                killer: killer.clone(),
                stopped: stopped.clone(),
            });
        }

        let out = on_event.clone();
        let read = std::thread::spawn(move || {
            let mut buf = [0u8; 8192];
            let mut pending = Vec::new();
            while let Ok(n) = reader.read(&mut buf) {
                if n == 0 {
                    break;
                }
                pending.extend_from_slice(&buf[..n]);
                let data = take_utf8(&mut pending);
                if !data.is_empty() && out.send(RunEvent::Output { data }).is_err() {
                    break;
                }
            }
        });

        let slave = pair.slave;
        let current = self.current.clone();
        std::thread::spawn(move || {
            let mut code = None;
            for step in &plan.steps {
                if stopped.load(Ordering::SeqCst) {
                    break;
                }
                let _ = on_event.send(RunEvent::Output {
                    data: format!("\x1b[2m> {}\x1b[0m\r\n", shown(step)),
                });
                let mut cmd = CommandBuilder::new(&step.program);
                cmd.args(&step.args);
                cmd.cwd(&plan.cwd);
                match slave.spawn_command(cmd) {
                    Ok(mut child) => {
                        if let Ok(mut k) = killer.lock() {
                            *k = Some(child.clone_killer());
                        }
                        // Stop was pressed while it was starting.
                        if stopped.load(Ordering::SeqCst) {
                            let _ = child.kill();
                        }
                        code = child.wait().ok().map(|s| s.exit_code());
                        if let Ok(mut k) = killer.lock() {
                            k.take();
                        }
                        if code != Some(0) {
                            break;
                        }
                    }
                    Err(e) => {
                        let _ = on_event.send(RunEvent::Output {
                            data: format!("could not start {}: {e}\r\n", step.program.display()),
                        });
                        code = None;
                        break;
                    }
                }
            }
            // Let the console flush, then close it so the reader sees the end.
            std::thread::sleep(std::time::Duration::from_millis(150));
            drop(slave);
            if let Ok(mut c) = current.lock() {
                if c.as_ref().is_some_and(|a| a.id == id) {
                    c.take();
                }
            }
            let _ = read.join();
            let _ = on_event.send(RunEvent::Exit {
                code,
                stopped: stopped.load(Ordering::SeqCst),
            });
        });
        Ok(())
    }

    /// Keystrokes from the output panel.
    pub fn input(&self, data: &str) {
        if let Ok(mut c) = self.current.lock() {
            if let Some(a) = c.as_mut() {
                let _ = a.writer.write_all(data.as_bytes());
                let _ = a.writer.flush();
            }
        }
    }

    pub fn resize(&self, cols: u16, rows: u16) {
        if let Ok(c) = self.current.lock() {
            if let Some(a) = c.as_ref() {
                let _ = a.master.resize(PtySize {
                    rows: rows.max(2),
                    cols: cols.max(10),
                    pixel_width: 0,
                    pixel_height: 0,
                });
            }
        }
    }

    pub fn stop(&self) {
        if let Ok(c) = self.current.lock() {
            if let Some(a) = c.as_ref() {
                a.stop();
            }
        }
    }
}

#[cfg(test)]
mod tests {
    // Spec s14 permits unwrap/expect in tests.
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;

    fn installed(names: &[&str]) -> Option<PathBuf> {
        Some(PathBuf::from(format!("/bin/{}", names[0])))
    }

    #[test]
    fn each_language_gets_its_compiler_then_its_program() {
        let root = Path::new("/p");
        let c = plan_with(root, Path::new("/p/src/a.c"), installed).unwrap();
        assert_eq!(c.cwd, Path::new("/p/src"));
        assert_eq!(c.steps.len(), 2);
        assert_eq!(c.steps[0].program, Path::new("/bin/gcc"));
        assert_eq!(
            c.steps[1].program,
            Path::new("/p/src/a").with_extension(std::env::consts::EXE_EXTENSION)
        );
        let py = plan_with(root, Path::new("/p/a.py"), installed).unwrap();
        assert_eq!(py.steps, [step("/bin/python".into(), &["/p/a.py"])]);
        let ts = plan_with(root, Path::new("/p/a.ts"), installed).unwrap();
        assert_eq!(ts.steps, [step("/bin/node".into(), &["/p/a.ts"])]);
        let tex = plan_with(root, Path::new("/p/doc/x.tex"), installed).unwrap();
        assert_eq!(tex.steps[0].args, ["x.tex"], "LaTeX runs beside its file");
        assert!(plan_with(root, Path::new("/p/a.md"), installed).is_err());
    }

    #[test]
    fn a_missing_compiler_says_what_to_install() {
        let none = |_: &[&str]| None;
        let e = plan_with(Path::new("/p"), Path::new("/p/a.cpp"), none).unwrap_err();
        assert!(e.contains("winget install"), "{e}");
    }

    #[test]
    fn rust_inside_a_cargo_project_uses_cargo_run() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().canonicalize().unwrap();
        std::fs::create_dir_all(root.join("app/src")).unwrap();
        std::fs::write(root.join("app/Cargo.toml"), "").unwrap();
        let p = plan_with(&root, &root.join("app/src/main.rs"), installed).unwrap();
        assert_eq!(p.cwd, plain(&root.join("app")));
        assert_eq!(p.steps[0].args, ["run"]);
        let lone = plan_with(&root, &root.join("one.rs"), installed).unwrap();
        assert_eq!(lone.steps.len(), 2, "a lone file is compiled with rustc");
    }

    #[test]
    fn the_windows_verbatim_prefix_is_dropped() {
        assert_eq!(plain(Path::new(r"\\?\C:\a\b.c")), Path::new(r"C:\a\b.c"));
        assert_eq!(
            plain(Path::new(r"\\?\UNC\srv\x")),
            Path::new(r"\\?\UNC\srv\x")
        );
    }

    #[test]
    fn only_plain_package_names_reach_pip() {
        for ok in [
            "numpy",
            "pandas==2.2.3",
            "requests[socks]",
            "scikit-learn",
            "opencv_python",
        ] {
            assert!(is_package(ok), "{ok}");
        }
        for bad in [
            "",
            "-r requirements.txt",
            "--index-url=http://x",
            "../x",
            "a b",
            "x;rm",
            "C:\\x",
        ] {
            assert!(!is_package(bad), "{bad}");
        }
    }

    #[test]
    fn a_character_split_across_reads_is_kept_whole() {
        let mut buf = "é".as_bytes()[..1].to_vec();
        assert_eq!(take_utf8(&mut buf), "");
        buf.extend_from_slice(&"é".as_bytes()[1..]);
        assert_eq!(take_utf8(&mut buf), "é");
        assert!(buf.is_empty());
    }
}
