//! Where things are defined in a project: functions, types, classes and
//! constants, found line by line with a pattern per language. Fast and local;
//! files are only reread when they change.
//!
//! shortcut: patterns, not a parser, so an unusual declaration can be missed;
//! upgrade to tree-sitter if testing shows real gaps.

use regex::Regex;
use serde::Serialize;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{LazyLock, Mutex};
use std::time::SystemTime;

/// Folders no one wants indexed or searched: version control, dependencies,
/// build output and virtual environments.
pub(crate) const SKIP_DIRS: &[&str] = &[
    ".git",
    "node_modules",
    "target",
    ".venv",
    "venv",
    "__pycache__",
    ".next",
];

/// Bigger files are generated or data, not code worth mapping.
const MAX_FILE_BYTES: u64 = 1024 * 1024;

/// shortcut: stops at this many files, raise it if a real repo hits it.
const MAX_FILES: usize = 20_000;

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Def {
    pub name: String,
    /// `fn`, `struct`, `class`, `const`…
    pub kind: String,
    /// Relative to the project, with `/` separators.
    pub path: String,
    /// 1-based.
    pub line: usize,
}

/// Every file in the project, in name order, skipping `SKIP_DIRS`, links
/// (they can lead out of the project or round in a loop) and big files.
#[must_use]
pub fn project_files(start: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let mut stack = vec![start.to_path_buf()];
    while let Some(path) = stack.pop() {
        if out.len() >= MAX_FILES {
            break;
        }
        if path.is_dir() {
            let skip = path
                .file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|n| SKIP_DIRS.contains(&n));
            if skip {
                continue;
            }
            if let Ok(entries) = std::fs::read_dir(&path) {
                let mut children: Vec<_> = entries
                    .flatten()
                    .filter(|e| e.file_type().is_ok_and(|t| !t.is_symlink()))
                    .map(|e| e.path())
                    .collect();
                // Reversed, so the stack visits them in name order.
                children.sort();
                children.reverse();
                stack.extend(children);
            }
        } else if std::fs::metadata(&path).is_ok_and(|m| m.len() <= MAX_FILE_BYTES) {
            out.push(path);
        }
    }
    out
}

/// The patterns for one file type: each captures (kind, name).
fn patterns(ext: &str) -> &'static [Regex] {
    static RUST: LazyLock<Vec<Regex>> = LazyLock::new(|| {
        vec![
            re(
                r#"^\s*(?:pub(?:\([^)]*\))?\s+)?(?:(?:async|const|unsafe|extern\s+"[^"]*")\s+)*(fn|struct|enum|trait|type|mod|const|static|union)\s+([A-Za-z_]\w*)"#,
            ),
            re(r"^\s*macro_rules!\s+()([A-Za-z_]\w*)"),
        ]
    });
    static PY: LazyLock<Vec<Regex>> =
        LazyLock::new(|| vec![re(r"^\s*(?:async\s+)?(def|class)\s+([A-Za-z_]\w*)")]);
    // Top level only for variables, or every local would be listed.
    static TS: LazyLock<Vec<Regex>> = LazyLock::new(|| {
        vec![
            re(
                r"^(?:export\s+(?:default\s+)?)?(?:declare\s+)?(?:abstract\s+)?(?:async\s+)?(function\*?|class|interface|type|enum|const|let|var)\s+([A-Za-z_$][\w$]*)",
            ),
            // Methods in a class body.
            re(
                r"^\s+(?:(?:public|private|protected|static|async|readonly|override)\s+)*()([A-Za-z_$][\w$]*)\s*\([^)]*\)\s*(?::[^{=]+)?\{\s*$",
            ),
        ]
    });
    static GO: LazyLock<Vec<Regex>> = LazyLock::new(|| {
        vec![
            re(r"^(func)\s+(?:\([^)]*\)\s*)?([A-Za-z_]\w*)"),
            re(r"^(type|const|var)\s+([A-Za-z_]\w*)"),
        ]
    });
    static C: LazyLock<Vec<Regex>> = LazyLock::new(|| {
        vec![
            re(r"^(?:typedef\s+)?(struct|class|enum|union|namespace)\s+([A-Za-z_]\w*)"),
            re(r"^#\s*(define)\s+([A-Za-z_]\w*)"),
            // A function: a type, then a name and an opening bracket, at the
            // start of a line, and not a call ending in `;`.
            re(r"^(?:[A-Za-z_][\w:<>,\*&\s]*?[\s\*&])()([A-Za-z_][\w:~]*)\s*\([^;]*$"),
        ]
    });
    match ext {
        "rs" => &RUST,
        "py" | "pyi" => &PY,
        "ts" | "tsx" | "mts" | "cts" | "js" | "jsx" | "mjs" | "cjs" => &TS,
        "go" => &GO,
        "c" | "h" | "cc" | "cpp" | "cxx" | "hpp" | "hh" | "hxx" => &C,
        _ => &[],
    }
}

#[allow(clippy::expect_used)] // The patterns are constants; a bad one fails the first test.
fn re(pattern: &str) -> Regex {
    Regex::new(pattern).expect("a valid pattern")
}

/// Words that open a statement, which the loose C pattern would otherwise
/// take for a function's name.
const NOT_NAMES: &[&str] = &[
    "if", "for", "while", "switch", "return", "else", "catch", "sizeof", "new", "delete",
];

/// The definitions in one file's `text`, as (kind, name, 1-based line).
#[must_use]
pub fn definitions(ext: &str, text: &str) -> Vec<(String, String, usize)> {
    let pats = patterns(ext);
    if pats.is_empty() {
        return Vec::new();
    }
    let mut out = Vec::new();
    for (i, line) in text.lines().enumerate() {
        for p in pats {
            let Some(c) = p.captures(line) else { continue };
            let name = c.get(2).map_or("", |m| m.as_str());
            if name.is_empty() || NOT_NAMES.contains(&name) {
                continue;
            }
            let kind = match c.get(1).map_or("", |m| m.as_str()) {
                "" if ext == "rs" => "macro",
                "" => "fn",
                "def" | "function" | "function*" | "func" => "fn",
                k => k,
            };
            out.push((kind.to_owned(), name.to_owned(), i + 1));
            break;
        }
    }
    out
}

/// One file's definitions as (kind, name, line), and when it was read.
type Cached = (SystemTime, Vec<(String, String, usize)>);

/// Definitions per file, kept between scans and reread only when a file's
/// modified time changes.
#[derive(Debug, Default)]
pub struct Index {
    files: Mutex<HashMap<PathBuf, Cached>>,
}

impl Index {
    /// Every definition under `root`, files in name order.
    #[must_use]
    pub fn scan(&self, root: &Path) -> Vec<Def> {
        let files = project_files(root);
        let mut cache = self
            .files
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let mut out = Vec::new();
        for path in &files {
            let ext = path
                .extension()
                .and_then(|e| e.to_str())
                .unwrap_or("")
                .to_ascii_lowercase();
            if patterns(&ext).is_empty() {
                continue;
            }
            let Ok(modified) = std::fs::metadata(path).and_then(|m| m.modified()) else {
                continue;
            };
            let fresh = cache.get(path).is_some_and(|(at, _)| *at == modified);
            if !fresh {
                let defs = std::fs::read(path)
                    .map(|b| definitions(&ext, &String::from_utf8_lossy(&b)))
                    .unwrap_or_default();
                cache.insert(path.clone(), (modified, defs));
            }
            let rel = path
                .strip_prefix(root)
                .unwrap_or(path)
                .to_string_lossy()
                .replace('\\', "/");
            if let Some((_, defs)) = cache.get(path) {
                out.extend(defs.iter().map(|(kind, name, line)| Def {
                    name: name.clone(),
                    kind: kind.clone(),
                    path: rel.clone(),
                    line: *line,
                }));
            }
        }
        out
    }
}

/// Names per file in the map; the rest are counted.
const MAP_NAMES_PER_FILE: usize = 15;

/// Worth a place in the outline: not `let`/`var`, and a `const` only when it
/// is named like a constant or component, not a script's local value.
fn mapped(d: &Def) -> bool {
    match d.kind.as_str() {
        "let" | "var" => false,
        "const" => d.name.starts_with(|c: char| c.is_ascii_uppercase()),
        _ => true,
    }
}

/// A short outline of the project for the agent: each file with the names it
/// defines, shallow files first, cut at about `budget` characters.
#[must_use]
pub fn repo_map(defs: &[Def], budget: usize) -> String {
    let mut by_file: Vec<(&str, Vec<String>)> = Vec::new();
    for d in defs.iter().filter(|d| mapped(d)) {
        let entry = format!("{} {}", d.kind, d.name);
        match by_file.last_mut() {
            Some((path, names)) if *path == d.path => names.push(entry),
            _ => by_file.push((&d.path, vec![entry])),
        }
    }
    by_file.sort_by_key(|(path, _)| (path.matches('/').count(), *path));
    let mut out = String::new();
    for (shown, (path, names)) in by_file.iter().enumerate() {
        let more = names.len().saturating_sub(MAP_NAMES_PER_FILE);
        let listed = names[..names.len() - more].join(", ");
        let line = if more > 0 {
            format!("{path}: {listed}, +{more} more\n")
        } else {
            format!("{path}: {listed}\n")
        };
        if out.len() + line.len() > budget {
            out.push_str(&format!(
                "… and {} more files with definitions; use find_symbol or list_dir.\n",
                by_file.len() - shown
            ));
            break;
        }
        out.push_str(&line);
    }
    out
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;

    fn names(ext: &str, text: &str) -> Vec<String> {
        definitions(ext, text)
            .into_iter()
            .map(|(k, n, l)| format!("{k} {n} {l}"))
            .collect()
    }

    #[test]
    fn rust_definitions_including_methods_and_qualified_ones() {
        let text = "pub struct Cube;\nimpl Cube {\n    pub async fn solve(&self) {}\n    const fn size() -> u8 { 3 }\n}\npub(crate) enum Face { U }\nconst MAX: u32 = 20;\nmacro_rules! twist { () => {} }\nlet x = 1;\n";
        assert_eq!(
            names("rs", text),
            [
                "struct Cube 1",
                "fn solve 3",
                "fn size 4",
                "enum Face 6",
                "const MAX 7",
                "macro twist 8"
            ]
        );
    }

    #[test]
    fn python_typescript_and_go() {
        assert_eq!(
            names(
                "py",
                "class Solver:\n    async def run(self):\n        x = 1\ndef main():\n"
            ),
            ["class Solver 1", "fn run 2", "fn main 4"]
        );
        assert_eq!(
            names(
                "ts",
                "export default function App() {}\nexport const api = {};\nconst local = 1;\ninterface Props {}\nclass A {\n  async load(id: string): Promise<void> {\n    if (x) {\n  }\n}\n"
            ),
            [
                "fn App 1",
                "const api 2",
                "const local 3",
                "interface Props 4",
                "class A 5",
                "fn load 6"
            ]
        );
        assert_eq!(
            names(
                "go",
                "func (s *Server) Start() error {\ntype Config struct {\nfunc main() {\n"
            ),
            ["fn Start 1", "type Config 2", "fn main 3"]
        );
    }

    #[test]
    fn c_functions_types_and_macros_but_not_calls_or_keywords() {
        let text = "#define SIZE 3\nstruct cube {\nstatic int solve(struct cube *c)\n{\n    if (x) {\n    solve(c);\nint main(void) {\nvoid Cube::turn(int face) {\n";
        assert_eq!(
            names("c", text),
            [
                "define SIZE 1",
                "struct cube 2",
                "fn solve 3",
                "fn main 7",
                "fn Cube::turn 8"
            ]
        );
    }

    #[test]
    fn a_scan_skips_dependencies_and_only_rereads_changed_files() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        std::fs::create_dir_all(root.join("src")).unwrap();
        std::fs::create_dir_all(root.join("node_modules/lib")).unwrap();
        std::fs::write(root.join("src/main.rs"), "fn main() {}\n").unwrap();
        std::fs::write(root.join("node_modules/lib/x.js"), "function hidden() {}\n").unwrap();
        std::fs::write(root.join("notes.txt"), "fn not_code() {}\n").unwrap();

        let index = Index::default();
        let defs = index.scan(root);
        assert_eq!(
            defs,
            [Def {
                name: "main".into(),
                kind: "fn".into(),
                path: "src/main.rs".into(),
                line: 1
            }]
        );

        // A cached file's stale copy would still say `main`.
        std::thread::sleep(std::time::Duration::from_millis(20));
        std::fs::write(root.join("src/main.rs"), "fn start() {}\nfn main() {}\n").unwrap();
        let names: Vec<_> = index.scan(root).into_iter().map(|d| d.name).collect();
        assert_eq!(names, ["start", "main"]);
    }

    #[test]
    fn the_map_lists_shallow_files_first_and_stops_at_the_budget() {
        let def = |path: &str, name: &str| Def {
            name: name.into(),
            kind: "fn".into(),
            path: path.into(),
            line: 1,
        };
        let defs = [
            def("src/deep/a.rs", "a"),
            def("main.rs", "main"),
            def("src/b.rs", "b"),
        ];
        assert_eq!(
            repo_map(&defs, 1000),
            "main.rs: fn main\nsrc/b.rs: fn b\nsrc/deep/a.rs: fn a\n"
        );
        let cut = repo_map(&defs, 30);
        assert!(cut.starts_with("main.rs: fn main\n"), "{cut}");
        assert!(cut.contains("2 more files"), "{cut}");

        let mut busy: Vec<Def> = (0..20).map(|i| def("a.rs", &format!("f{i}"))).collect();
        busy.push(Def {
            kind: "const".into(),
            ..def("s.mjs", "local")
        });
        busy.push(Def {
            kind: "const".into(),
            ..def("s.mjs", "MAX")
        });
        let map = repo_map(&busy, 10_000);
        assert!(map.contains("fn f14, +5 more\n"), "{map}");
        assert!(map.contains("s.mjs: const MAX\n"), "locals left out: {map}");
    }
}
