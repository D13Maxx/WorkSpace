//! Language packs: what each language needs, whether it is on this computer,
//! and installing what is missing with winget. Settings → Languages and the
//! installer's chooser window (`--choose-languages`) both use these.
//!
//! A program installed here is on the PATH that new programs get, not on the
//! one Workspace started with: the packs re-read PATH as they go, and the
//! rest of the app sees the new tools after a restart.

use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::atomic::{AtomicBool, Ordering};

use serde::Serialize;
use tauri::{AppHandle, Emitter as _, Manager as _};
use tokio::io::{AsyncBufReadExt as _, BufReader};

/// The flag the installer starts Workspace with to show only the chooser.
pub const CHOOSE_FLAG: &str = "--choose-languages";

/// How to tell whether a part is here.
enum Probe {
    /// Any of these programs.
    Program(&'static [&'static str]),
    /// A module the found Python can import.
    PythonModule(&'static str),
    /// A component rustup lists as installed (its proxy is always on PATH).
    RustupComponent(&'static str),
    /// Microsoft's C++ build tools, which Rust's default toolchain links with.
    MsvcTools,
    /// A gdb new enough to debug with.
    Gdb,
}

/// One install step: a program and its arguments. `winget` and `npm` and the
/// like are found on the refreshed PATH when the step runs.
type Step = (&'static str, &'static [&'static str]);

struct Part {
    name: &'static str,
    probe: Probe,
    steps: &'static [Step],
}

struct Pack {
    id: &'static str,
    name: &'static str,
    about: &'static str,
    parts: &'static [Part],
}

/// What `winget!` passes before the package id; the test holds every step to it.
#[cfg(test)]
const WINGET: &[&str] = &[
    "install",
    "--exact",
    "--silent",
    "--accept-package-agreements",
    "--accept-source-agreements",
    "--disable-interactivity",
    "--id",
];

macro_rules! winget {
    ($id:literal) => {
        (
            "winget",
            &[
                "install",
                "--exact",
                "--silent",
                "--accept-package-agreements",
                "--accept-source-agreements",
                "--disable-interactivity",
                "--id",
                $id,
            ],
        )
    };
}

const PACKS: &[Pack] = &[
    Pack {
        id: "core",
        name: "Core",
        about: "Git for agent teams' copies and the Changes panel; Node.js for Reverse Engineer Anything, knowledge graphs and running JavaScript.",
        parts: &[
            Part {
                name: "Git",
                probe: Probe::Program(&["git"]),
                steps: &[winget!("Git.Git")],
            },
            Part {
                name: "Node.js",
                probe: Probe::Program(&["node"]),
                steps: &[winget!("OpenJS.NodeJS.LTS")],
            },
        ],
    },
    Pack {
        id: "python",
        name: "Python",
        about: "Run, debug and edit Python with a language server.",
        parts: &[
            Part {
                name: "Python",
                probe: Probe::Program(&["python", "py"]),
                steps: &[winget!("Python.Python.3.14")],
            },
            Part {
                name: "debugpy (debugger)",
                probe: Probe::PythonModule("debugpy"),
                steps: &[("python", &["-m", "pip", "install", "--user", "debugpy"])],
            },
            Part {
                name: "Pyright (language server)",
                probe: Probe::Program(&["pyright-langserver"]),
                steps: &[("npm", &["install", "--global", "pyright"])],
            },
        ],
    },
    Pack {
        id: "typescript",
        name: "JavaScript and TypeScript",
        about: "A language server for .js and .ts files. Running them needs only Core's Node.js.",
        parts: &[Part {
            name: "TypeScript language server",
            probe: Probe::Program(&["typescript-language-server"]),
            steps: &[(
                "npm",
                &[
                    "install",
                    "--global",
                    "typescript",
                    "typescript-language-server",
                ],
            )],
        }],
    },
    Pack {
        id: "c",
        name: "C and C++",
        about: "Compile, run and debug C and C++ with GCC and GDB; clangd for the editor.",
        parts: &[
            Part {
                name: "GCC and GDB",
                probe: Probe::Gdb,
                steps: &[winget!("BrechtSanders.WinLibs.POSIX.UCRT")],
            },
            Part {
                name: "clangd (LLVM)",
                probe: Probe::Program(&["clangd"]),
                steps: &[winget!("LLVM.LLVM")],
            },
        ],
    },
    Pack {
        id: "rust",
        name: "Rust",
        about: "Build, run and debug Rust. Microsoft's C++ build tools are a large download and ask for admin rights.",
        parts: &[
            Part {
                name: "C++ build tools (Microsoft)",
                probe: Probe::MsvcTools,
                steps: &[(
                    "winget",
                    &[
                        "install",
                        "--exact",
                        "--silent",
                        "--accept-package-agreements",
                        "--accept-source-agreements",
                        "--disable-interactivity",
                        "--id",
                        "Microsoft.VisualStudio.2022.BuildTools",
                        "--override",
                        "--wait --passive --add Microsoft.VisualStudio.Workload.VCTools --includeRecommended",
                    ],
                )],
            },
            Part {
                name: "Rust (rustup)",
                probe: Probe::Program(&["cargo"]),
                steps: &[
                    winget!("Rustlang.Rustup"),
                    ("rustup", &["default", "stable"]),
                ],
            },
            Part {
                name: "rust-analyzer",
                probe: Probe::RustupComponent("rust-analyzer"),
                steps: &[("rustup", &["component", "add", "rust-analyzer"])],
            },
            Part {
                name: "lldb-dap (LLVM debugger)",
                probe: Probe::Program(&["lldb-dap"]),
                steps: &[winget!("LLVM.LLVM")],
            },
        ],
    },
    Pack {
        id: "go",
        name: "Go",
        about: "Go and gopls, for editing Go files.",
        parts: &[
            Part {
                name: "Go",
                probe: Probe::Program(&["go"]),
                steps: &[winget!("GoLang.Go")],
            },
            Part {
                name: "gopls (language server)",
                probe: Probe::Program(&["gopls"]),
                steps: &[("go", &["install", "golang.org/x/tools/gopls@latest"])],
            },
        ],
    },
    Pack {
        id: "latex",
        name: "LaTeX",
        about: "Build .tex files into PDFs with MiKTeX.",
        parts: &[Part {
            name: "MiKTeX",
            probe: Probe::Program(&["pdflatex", "tectonic"]),
            steps: &[winget!("MiKTeX.MiKTeX")],
        }],
    },
];

/// Where to look for programs: PATH as the system has it now, then the
/// folders installers use without always adding them to PATH.
#[derive(Debug, Clone)]
struct Search {
    path: String,
}

impl Search {
    /// PATH as it is now, read from the registry on Windows so tools installed
    /// since Workspace started are found; the process's own elsewhere.
    async fn now() -> Self {
        let own = std::env::var("PATH").unwrap_or_default();
        if !cfg!(windows) {
            return Self { path: own };
        }
        let read = no_window(Path::new("powershell"))
            .args([
                "-NoProfile",
                "-Command",
                "[Environment]::GetEnvironmentVariable('Path','Machine') + ';' + [Environment]::GetEnvironmentVariable('Path','User')",
            ])
            .stdin(Stdio::null())
            .output()
            .await;
        let fresh = match read {
            Ok(out) if out.status.success() => {
                String::from_utf8_lossy(&out.stdout).trim().to_owned()
            }
            _ => String::new(),
        };
        let mut dirs: Vec<PathBuf> = std::env::split_paths(&fresh)
            .chain(std::env::split_paths(&own))
            .chain(extra_dirs())
            .filter(|d| !d.as_os_str().is_empty())
            // The Store's stand-ins for python open the Store instead of running.
            .filter(|d| {
                !d.to_string_lossy()
                    .to_lowercase()
                    .contains(r"microsoft\windowsapps")
            })
            .collect();
        dirs.dedup();
        Self {
            path: std::env::join_paths(dirs)
                .map(|p| p.to_string_lossy().into_owned())
                .unwrap_or(own),
        }
    }

    fn find(&self, names: &[&str]) -> Option<PathBuf> {
        let exts: &[&str] = if cfg!(windows) {
            &[".exe", ".cmd", ".bat"]
        } else {
            &[""]
        };
        names.iter().find_map(|name| {
            std::env::split_paths(&self.path).find_map(|dir| {
                exts.iter()
                    .map(|ext| dir.join(format!("{name}{ext}")))
                    .find(|p| p.is_file())
            })
        })
    }

    fn command(&self, program: &Path) -> tokio::process::Command {
        let mut cmd = no_window(program);
        cmd.env("PATH", &self.path)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        cmd
    }

    async fn quietly_succeeds(&self, program: &Path, args: &[&str]) -> Option<String> {
        let out = self.command(program).args(args).output().await.ok()?;
        out.status
            .success()
            .then(|| String::from_utf8_lossy(&out.stdout).into_owned())
    }

    async fn has(&self, probe: &Probe) -> bool {
        match probe {
            Probe::Program(names) => self.find(names).is_some(),
            Probe::Gdb => crate::bridge::newest_gdb(std::ffi::OsStr::new(&self.path)).is_some(),
            Probe::PythonModule(module) => match self.find(&["python", "py"]) {
                Some(python) => self
                    .quietly_succeeds(&python, &["-c", &format!("import {module}")])
                    .await
                    .is_some(),
                None => false,
            },
            Probe::RustupComponent(component) => match self.find(&["rustup"]) {
                Some(rustup) => self
                    .quietly_succeeds(&rustup, &["component", "list", "--installed"])
                    .await
                    .is_some_and(|list| list.lines().any(|l| l.starts_with(component))),
                None => false,
            },
            Probe::MsvcTools => {
                let Some(base) = std::env::var_os("ProgramFiles(x86)") else {
                    return false;
                };
                let vswhere =
                    Path::new(&base).join(r"Microsoft Visual Studio\Installer\vswhere.exe");
                vswhere.is_file()
                    && self
                        .quietly_succeeds(
                            &vswhere,
                            &[
                                "-products",
                                "*",
                                "-requires",
                                "Microsoft.VisualStudio.Component.VC.Tools.x86.x64",
                                "-property",
                                "installationPath",
                            ],
                        )
                        .await
                        .is_some_and(|p| !p.trim().is_empty())
            }
        }
    }
}

/// A command that opens no console window of its own, which a GUI app's
/// children otherwise each flash up on Windows.
fn no_window(program: &Path) -> tokio::process::Command {
    #[allow(unused_mut)]
    let mut cmd = tokio::process::Command::new(program);
    #[cfg(windows)]
    cmd.creation_flags(0x0800_0000);
    cmd
}

/// Folders tools install into without always adding them to PATH.
fn extra_dirs() -> Vec<PathBuf> {
    let env = |k: &str| std::env::var_os(k).map(PathBuf::from);
    let mut dirs = Vec::new();
    if let Some(pf) = env("ProgramFiles") {
        for d in [r"LLVM\bin", r"Git\cmd", "nodejs", r"Go\bin"] {
            dirs.push(pf.join(d));
        }
    }
    if let Some(home) = env("USERPROFILE") {
        dirs.push(home.join(r".cargo\bin"));
        dirs.push(home.join(r"go\bin"));
    }
    if let Some(roaming) = env("APPDATA") {
        dirs.push(roaming.join("npm"));
    }
    if let Some(local) = env("LOCALAPPDATA") {
        dirs.push(local.join(r"Programs\MiKTeX\miktex\bin\x64"));
    }
    dirs
}

#[derive(Debug, Serialize)]
pub struct PartStatus {
    pub name: String,
    pub present: bool,
}

#[derive(Debug, Serialize)]
pub struct PackStatus {
    pub id: String,
    pub name: String,
    pub about: String,
    pub parts: Vec<PartStatus>,
}

#[derive(Debug, Serialize)]
pub struct Packs {
    pub packs: Vec<PackStatus>,
    /// Whether Workspace can install them here (winget, on Windows).
    pub can_install: bool,
}

/// Every pack, and which of its parts are here.
#[tauri::command]
pub async fn language_packs() -> Packs {
    let search = Search::now().await;
    let mut packs = Vec::with_capacity(PACKS.len());
    for pack in PACKS {
        let mut parts = Vec::with_capacity(pack.parts.len());
        for part in pack.parts {
            parts.push(PartStatus {
                name: part.name.to_owned(),
                present: search.has(&part.probe).await,
            });
        }
        packs.push(PackStatus {
            id: pack.id.to_owned(),
            name: pack.name.to_owned(),
            about: pack.about.to_owned(),
            parts,
        });
    }
    Packs {
        packs,
        can_install: cfg!(windows) && search.find(&["winget"]).is_some(),
    }
}

/// A line of an install's output, for the UI.
#[derive(Debug, Clone, Serialize)]
struct Progress {
    pack: String,
    line: String,
}

/// winget redraws progress bars with carriage returns: keep what reads as text.
fn readable(raw: &str) -> Option<String> {
    let last = raw.rsplit('\r').find(|s| !s.trim().is_empty())?.trim();
    let text = last.chars().filter(|c| c.is_alphanumeric()).count();
    (text >= 3).then(|| last.to_owned())
}

/// Install a pack's missing parts in order, telling the UI as it goes
/// (`language-pack-progress`). Parts already here are skipped.
///
/// # Errors
/// Installing is not possible here, the pack is unknown, or a step fails.
#[tauri::command]
pub async fn install_language_pack(app: AppHandle, id: String) -> Result<(), String> {
    if !cfg!(windows) {
        return Err(
            "Workspace installs languages with winget, on Windows; use your package manager here"
                .to_owned(),
        );
    }
    let pack = PACKS
        .iter()
        .find(|p| p.id == id)
        .ok_or("no such language pack")?;
    let say = |line: String| {
        let _ = app.emit(
            "language-pack-progress",
            Progress {
                pack: id.clone(),
                line,
            },
        );
    };
    for part in pack.parts {
        // Read again each time: the last step may have put this part's tools on PATH.
        let search = Search::now().await;
        if search.has(&part.probe).await {
            continue;
        }
        say(format!("Installing {}…", part.name));
        for (program, args) in part.steps {
            let python = *program == "python";
            let found = if python {
                search.find(&["python", "py"])
            } else {
                search.find(&[program])
            };
            let Some(exe) = found else {
                return Err(format!(
                    "{} needs {program}, which is not installed: install the pack it belongs to first",
                    part.name
                ));
            };
            let mut child = search
                .command(&exe)
                .args(*args)
                .spawn()
                .map_err(|e| format!("could not start {program}: {e}"))?;
            let (Some(out), Some(err)) = (child.stdout.take(), child.stderr.take()) else {
                return Err(format!("{program} gave no output to read"));
            };
            let mut out = BufReader::new(out).lines();
            let mut err = BufReader::new(err).lines();
            let (mut out_done, mut err_done) = (false, false);
            while !(out_done && err_done) {
                tokio::select! {
                    line = out.next_line(), if !out_done => match line {
                        Ok(Some(l)) => { if let Some(l) = readable(&l) { say(l); } }
                        _ => out_done = true,
                    },
                    line = err.next_line(), if !err_done => match line {
                        Ok(Some(l)) => { if let Some(l) = readable(&l) { say(l); } }
                        _ => err_done = true,
                    },
                }
            }
            let status = child.wait().await.map_err(|e| e.to_string())?;
            // winget's "already installed" and "no newer version" are fine.
            let fine = status.success()
                || (*program == "winget"
                    && matches!(status.code(), Some(-1_978_335_189 | -1_978_335_135)));
            if !fine {
                return Err(format!(
                    "installing {} failed: {program} exited with {}",
                    part.name,
                    status
                        .code()
                        .map_or_else(|| "no code".to_owned(), |c| format!("code {c}"))
                ));
            }
        }
        say(format!("{} installed.", part.name));
    }
    Ok(())
}

/// Set when this run of Workspace is only the installer's chooser.
static CHOOSER_ONLY: AtomicBool = AtomicBool::new(false);

/// Whether closing the chooser should quit: it was all this run showed.
pub fn chooser_only() -> bool {
    CHOOSER_ONLY.load(Ordering::Relaxed)
}

/// The window the installer opens. `alone` when this run is only the
/// chooser; otherwise setup found Workspace open and it shows there too.
pub fn open_chooser(app: &AppHandle, alone: bool) -> tauri::Result<()> {
    if let Some(window) = app.get_webview_window("languages") {
        return window.set_focus();
    }
    if alone {
        CHOOSER_ONLY.store(true, Ordering::Relaxed);
    }
    tauri::WebviewWindowBuilder::new(
        app,
        "languages",
        tauri::WebviewUrl::App("index.html".into()),
    )
    .title("Choose languages — Workspace setup")
    .inner_size(720.0, 760.0)
    .min_inner_size(560.0, 480.0)
    .initialization_script("window.__WORKSPACE_LANGUAGES__ = true;")
    .build()
    .map(|_| ())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_pack_has_a_unique_id_and_parts_that_install() {
        let mut ids: Vec<_> = PACKS.iter().map(|p| p.id).collect();
        ids.sort_unstable();
        ids.dedup();
        assert_eq!(ids.len(), PACKS.len());
        for part in PACKS.iter().flat_map(|p| p.parts) {
            assert!(
                !part.steps.is_empty(),
                "{} has no way to install",
                part.name
            );
            for (program, args) in part.steps {
                if *program == "winget" {
                    assert!(args.starts_with(WINGET), "{}: winget flags", part.name);
                    assert!(args.len() > WINGET.len(), "{}: a package id", part.name);
                }
            }
        }
        assert_eq!(
            PACKS[0].id, "core",
            "core comes first: the others use its Node.js"
        );
    }

    #[test]
    fn progress_bars_are_dropped_and_text_is_kept() {
        assert_eq!(
            readable("  ██████▒▒▒  12 MB / 30 MB"),
            Some("██████▒▒▒  12 MB / 30 MB".to_owned())
        );
        assert_eq!(readable("\r   - \r   \\ "), None);
        assert_eq!(
            readable("Found Git [Git.Git] Version 2.56.1"),
            Some("Found Git [Git.Git] Version 2.56.1".to_owned())
        );
        assert_eq!(
            readable("old\rSuccessfully installed"),
            Some("Successfully installed".to_owned())
        );
    }
}
