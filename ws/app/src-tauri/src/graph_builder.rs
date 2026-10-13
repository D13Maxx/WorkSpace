//! Workspace's own knowledge graph: free, with no model and no Node.
//!
//! Reads the project's source with simple per-language patterns (not a
//! parser): its files, the functions and types they define, and which files
//! import which. Writes the result in Understand-Anything's format to
//! `.ua/knowledge-graph.json`, so UA's dashboard shows it. Summaries come
//! from each file's leading comment, or describe what it contains.
//!
//! shortcut: line patterns miss unusual formatting and macros; UA's /understand gives the full AI graph.

use serde::Serialize;
use serde_json::{Value, json};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::{Path, PathBuf};

/// A bigger project is graphed from its first files in path order.
const MAX_FILES: usize = 5000;
const MAX_FILE_BYTES: u64 = 1024 * 1024;
/// Per file, so one generated file cannot swamp the graph.
const MAX_SYMBOLS: usize = 40;
const MAX_LAYERS: usize = 12;
/// Marks a graph this module wrote, which it may replace without asking.
const MARKER: &str = "Built by Workspace's free graph builder";
/// What MARKER said when the app was called FRIDAY: graphs built then are ours too.
const OLD_MARKER: &str = "Built by FRIDAY's free graph builder";

const SKIP_DIRS: &[&str] = &[
    ".git",
    "node_modules",
    "target",
    "dist",
    "build",
    "out",
    ".ua",
    ".understand-anything",
    "__pycache__",
    ".venv",
    "venv",
    ".next",
    "vendor",
    "bin",
    "obj",
    ".idea",
    ".vscode",
];

#[derive(Clone, Copy, PartialEq, Eq)]
enum Kind {
    Code,
    Config,
    Doc,
}

fn classify(name: &str) -> Option<(&'static str, Kind)> {
    let lower = name.to_ascii_lowercase();
    if lower == "dockerfile" {
        return Some(("dockerfile", Kind::Config));
    }
    let ext = lower.rsplit_once('.')?.1;
    Some(match ext {
        "rs" => ("rust", Kind::Code),
        "py" => ("python", Kind::Code),
        "js" | "jsx" | "mjs" | "cjs" => ("javascript", Kind::Code),
        "ts" | "tsx" | "mts" | "cts" => ("typescript", Kind::Code),
        "c" | "h" => ("c", Kind::Code),
        "cpp" | "cc" | "cxx" | "hpp" | "hh" => ("cpp", Kind::Code),
        "java" => ("java", Kind::Code),
        "kt" => ("kotlin", Kind::Code),
        "cs" => ("csharp", Kind::Code),
        "go" => ("go", Kind::Code),
        "rb" => ("ruby", Kind::Code),
        "php" => ("php", Kind::Code),
        "swift" => ("swift", Kind::Code),
        "scala" => ("scala", Kind::Code),
        "html" | "htm" => ("html", Kind::Code),
        "css" | "scss" => ("css", Kind::Code),
        "sh" | "bash" | "ps1" => ("shell", Kind::Code),
        "json" | "toml" | "yaml" | "yml" | "ini" | "xml" => (
            match ext {
                "yml" => "yaml",
                e => leak(e),
            },
            Kind::Config,
        ),
        "md" | "txt" | "rst" | "tex" => {
            (if ext == "md" { "markdown" } else { leak(ext) }, Kind::Doc)
        }
        _ => return None,
    })
}

/// The few extension names used as language ids, as `'static`.
fn leak(ext: &str) -> &'static str {
    match ext {
        "json" => "json",
        "toml" => "toml",
        "yaml" => "yaml",
        "ini" => "ini",
        "xml" => "xml",
        "txt" => "text",
        "rst" => "rst",
        "tex" => "latex",
        _ => "text",
    }
}

struct Symbol {
    kind: &'static str,
    name: String,
    line: usize,
}

struct FileInfo {
    rel: String,
    language: &'static str,
    kind: Kind,
    lines: usize,
    doc: Option<String>,
    symbols: Vec<Symbol>,
    /// Raw import specifiers, resolved to project files later.
    imports: Vec<String>,
}

fn ident(s: &str) -> Option<String> {
    let name: String = s
        .chars()
        .take_while(|c| c.is_alphanumeric() || *c == '_' || *c == '$')
        .collect();
    (!name.is_empty() && !name.starts_with(|c: char| c.is_ascii_digit())).then_some(name)
}

/// Drop any of `words` from the front of `line`, repeatedly.
fn strip_words<'a>(mut line: &'a str, words: &[&str]) -> &'a str {
    loop {
        let before = line;
        for w in words {
            if let Some(rest) = line.strip_prefix(w) {
                line = rest.trim_start();
            }
        }
        if let Some(rest) = line.strip_prefix("pub(") {
            if let Some((_, after)) = rest.split_once(')') {
                line = after.trim_start();
            }
        }
        if line == before {
            return line;
        }
    }
}

const NOT_FUNCTIONS: &[&str] = &[
    "if",
    "for",
    "while",
    "switch",
    "catch",
    "return",
    "else",
    "do",
    "sizeof",
    "new",
    "throw",
    "elif",
    "with",
    "match",
    "case",
    "typedef",
    "using",
    "namespace",
    "template",
];

/// A function or type defined on this line, if the line starts one.
fn symbol_on(language: &str, raw: &str) -> Option<(&'static str, String)> {
    let indent = raw.len() - raw.trim_start().len();
    let line = raw.trim();
    // Top level, or one level in (methods).
    if indent > 4 && !matches!(language, "java" | "csharp" | "kotlin" | "scala") {
        return None;
    }
    let after = |prefix: &str| {
        line.strip_prefix(prefix)
            .and_then(|r| ident(r.trim_start()))
    };
    match language {
        "rust" => {
            let l = strip_words(
                line,
                &[
                    "pub ",
                    "async ",
                    "unsafe ",
                    "const ",
                    "extern \"C\" ",
                    "default ",
                ],
            );
            for (kw, kind) in [
                ("fn ", "function"),
                ("struct ", "class"),
                ("enum ", "class"),
                ("trait ", "class"),
                ("union ", "class"),
            ] {
                if let Some(rest) = l.strip_prefix(kw) {
                    return ident(rest).map(|n| (kind, n));
                }
            }
            None
        }
        "python" => after("def ")
            .or_else(|| after("async def "))
            .map(|n| ("function", n))
            .or_else(|| after("class ").map(|n| ("class", n))),
        "javascript" | "typescript" => {
            let l = strip_words(
                line,
                &["export ", "default ", "async ", "declare ", "abstract "],
            );
            if let Some(rest) = l.strip_prefix("function") {
                return ident(rest.trim_start_matches('*').trim_start()).map(|n| ("function", n));
            }
            for kw in ["class ", "interface "] {
                if let Some(rest) = l.strip_prefix(kw) {
                    return ident(rest).map(|n| ("class", n));
                }
            }
            for kw in ["const ", "let "] {
                if let Some(rest) = l.strip_prefix(kw) {
                    let is_fn = rest.contains("=>") || rest.contains("function");
                    return ident(rest)
                        .filter(|_| is_fn && indent == 0)
                        .map(|n| ("function", n));
                }
            }
            None
        }
        "go" => {
            if let Some(rest) = line.strip_prefix("func ") {
                // A method: `func (r *T) Name(`.
                let rest = if rest.starts_with('(') {
                    rest.split_once(')').map_or(rest, |(_, r)| r.trim_start())
                } else {
                    rest
                };
                return ident(rest).map(|n| ("function", n));
            }
            line.strip_prefix("type ")
                .and_then(ident)
                .filter(|_| line.contains("struct") || line.contains("interface"))
                .map(|n| ("class", n))
        }
        "java" | "csharp" | "kotlin" | "scala" | "c" | "cpp" | "swift" | "php" => {
            let l = strip_words(
                line,
                &[
                    "public ",
                    "private ",
                    "protected ",
                    "internal ",
                    "static ",
                    "final ",
                    "abstract ",
                    "sealed ",
                    "partial ",
                    "virtual ",
                    "override ",
                    "inline ",
                    "extern ",
                    "export ",
                    "open ",
                    "data ",
                    "async ",
                    "synchronized ",
                ],
            );
            for kw in [
                "class ",
                "interface ",
                "enum ",
                "struct ",
                "record ",
                "object ",
                "trait ",
            ] {
                if let Some(rest) = l.strip_prefix(kw) {
                    return ident(rest)
                        .filter(|_| !line.ends_with(';'))
                        .map(|n| ("class", n));
                }
            }
            for kw in ["fun ", "def ", "func ", "function "] {
                if let Some(rest) = l.strip_prefix(kw) {
                    return ident(rest).map(|n| ("function", n));
                }
            }
            // `type name(args) {` or `type name(args)` with the brace below.
            let open = l.find('(')?;
            if line.ends_with(';')
                || line.starts_with('#')
                || line.starts_with("//")
                || l.contains('=')
            {
                return None;
            }
            let head = l[..open].trim_end();
            let name = head
                .rsplit(|c: char| c.is_whitespace() || c == '*' || c == '&')
                .next()?;
            let name = name.rsplit("::").next().unwrap_or(name);
            let has_type = head.len() > name.len();
            (has_type && !NOT_FUNCTIONS.contains(&name))
                .then(|| ident(name))
                .flatten()
                .filter(|n| n.len() == name.len())
                .map(|n| ("function", n))
        }
        _ => None,
    }
}

/// Import specifiers on this line.
fn imports_on(language: &str, line: &str) -> Vec<String> {
    let line = line.trim();
    let quoted = |s: &str| -> Option<String> {
        let start = s.find(['"', '\'', '`'])?;
        let q = s[start..].chars().next()?;
        let rest = &s[start + 1..];
        Some(rest[..rest.find(q)?].to_owned())
    };
    match language {
        "rust" => {
            let l = strip_words(line, &["pub "]);
            if let Some(m) = l
                .strip_prefix("mod ")
                .filter(|r| r.trim_end().ends_with(';'))
            {
                return ident(m)
                    .map(|n| vec![format!("mod:{n}")])
                    .unwrap_or_default();
            }
            l.strip_prefix("use ")
                .filter(|r| r.starts_with("crate::") || r.starts_with("super::"))
                .map(|r| {
                    let path: String = r
                        .chars()
                        .take_while(|c| c.is_alphanumeric() || *c == '_' || *c == ':')
                        .collect();
                    vec![format!("use:{}", path.trim_end_matches(':'))]
                })
                .unwrap_or_default()
        }
        "python" => {
            if let Some(rest) = line.strip_prefix("from ") {
                let module = rest.split_whitespace().next().unwrap_or("");
                let names = rest.split_once(" import ").map_or("", |(_, n)| n);
                if module.chars().all(|c| c == '.') {
                    // `from . import a, b`: each name is a sibling module.
                    return names
                        .split(',')
                        .filter_map(|n| ident(n.trim()))
                        .map(|n| format!("py:{module}{n}"))
                        .collect();
                }
                return vec![format!("py:{module}")];
            }
            line.strip_prefix("import ")
                .map(|rest| {
                    rest.split(',')
                        .filter_map(|m| m.split_whitespace().next())
                        .map(|m| format!("py:{m}"))
                        .collect()
                })
                .unwrap_or_default()
        }
        "javascript" | "typescript" => {
            let is_import = (line.starts_with("import ") || line.starts_with("export "))
                && (line.contains(" from ")
                    || line.starts_with("import \"")
                    || line.starts_with("import '"));
            let spec = if is_import {
                line.rsplit_once(" from ").map_or(line, |(_, s)| s)
            } else if let Some(i) = line.find("require(").or_else(|| line.find("import(")) {
                &line[i..]
            } else {
                return Vec::new();
            };
            quoted(spec)
                .filter(|s| s.starts_with('.'))
                .map(|s| format!("js:{s}"))
                .into_iter()
                .collect()
        }
        "c" | "cpp" => line
            .strip_prefix("#include")
            .map(str::trim)
            .filter(|r| r.starts_with('"'))
            .and_then(quoted)
            .map(|s| vec![format!("inc:{s}")])
            .unwrap_or_default(),
        "java" | "kotlin" | "scala" => line
            .strip_prefix("import ")
            .map(|r| strip_words(r, &["static "]))
            .map(|r| r.trim_end_matches(';').trim())
            .filter(|r| !r.ends_with('*'))
            .map(|r| vec![format!("jvm:{r}")])
            .unwrap_or_default(),
        _ => Vec::new(),
    }
}

/// The first line of the file's leading comment or docstring, skipping
/// licence headers and tool directives.
fn leading_doc(text: &str) -> Option<String> {
    let mut in_docstring = false;
    for raw in text.lines().take(40) {
        let line = raw.trim();
        if line.is_empty() || line.starts_with("#!") || line.contains("coding:") {
            continue;
        }
        let stripped = if in_docstring {
            line
        } else if let Some(rest) = line
            .strip_prefix("\"\"\"")
            .or_else(|| line.strip_prefix("'''"))
        {
            in_docstring = true;
            rest
        } else {
            // None: code before any comment, so there is no leading doc.
            ["//!", "///", "//", "/**", "/*", "*", "#", "--", "<!--", "%"]
                .iter()
                .find_map(|m| line.strip_prefix(m))?
        };
        let text = stripped
            .trim_end_matches("*/")
            .trim_end_matches("-->")
            .trim_end_matches("\"\"\"")
            .trim_matches(|c: char| c == '*' || c == '!' || c == '/' || c.is_whitespace());
        let lower = text.to_ascii_lowercase();
        let noise = [
            "copyright",
            "spdx",
            "license",
            "eslint",
            "@ts-",
            "prettier",
            "use strict",
            "jshint",
        ];
        if text.len() < 3 || noise.iter().any(|n| lower.contains(n)) || text.starts_with('-') {
            continue;
        }
        let sentence = text.split_inclusive(". ").next().unwrap_or(text).trim();
        return Some(sentence.chars().take(200).collect());
    }
    None
}

fn analyse(rel: String, language: &'static str, kind: Kind, text: &str) -> FileInfo {
    let mut symbols = Vec::new();
    let mut imports = Vec::new();
    if kind == Kind::Code {
        for (i, line) in text.lines().enumerate() {
            if symbols.len() < MAX_SYMBOLS {
                if let Some((kind, name)) = symbol_on(language, line) {
                    symbols.push(Symbol {
                        kind,
                        name,
                        line: i + 1,
                    });
                }
            }
            imports.extend(imports_on(language, line));
        }
    }
    FileInfo {
        rel,
        language,
        kind,
        lines: text.lines().count(),
        doc: leading_doc(text),
        symbols,
        imports,
    }
}

fn walk(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    let mut entries: Vec<_> = entries.flatten().collect();
    entries.sort_by_key(std::fs::DirEntry::file_name);
    for e in entries {
        if out.len() >= MAX_FILES {
            return;
        }
        let name = e.file_name().to_string_lossy().into_owned();
        let Ok(kind) = e.file_type() else { continue };
        if kind.is_dir() {
            if !SKIP_DIRS.contains(&name.as_str()) && !name.starts_with('.') {
                walk(&e.path(), out);
            }
        } else if kind.is_file() && classify(&name).is_some() {
            out.push(e.path());
        }
    }
}

/// Resolve one import specifier from `from` to a project file.
fn resolve(
    spec: &str,
    from: &str,
    files: &HashSet<String>,
    by_name: &HashMap<String, Vec<String>>,
) -> Option<String> {
    let dir = from.rsplit_once('/').map_or("", |(d, _)| d);
    let join = |base: &str, rel: &str| -> String {
        let mut parts: Vec<&str> = base.split('/').filter(|p| !p.is_empty()).collect();
        for p in rel.split('/') {
            match p {
                "" | "." => {}
                ".." => {
                    parts.pop();
                }
                p => parts.push(p),
            }
        }
        parts.join("/")
    };
    let first = |cands: Vec<String>| cands.into_iter().find(|c| files.contains(c));
    let (kind, value) = spec.split_once(':')?;
    match kind {
        "mod" => {
            let stem = from.rsplit('/').next()?.trim_end_matches(".rs");
            let base = if matches!(stem, "main" | "lib" | "mod") {
                dir.to_owned()
            } else {
                join(dir, stem)
            };
            first(vec![
                join(&base, &format!("{value}.rs")),
                join(&base, &format!("{value}/mod.rs")),
            ])
        }
        "use" => {
            let mut segs: Vec<&str> = value.split("::").collect();
            let mut base = match segs.first()? {
                &"crate" => {
                    // The crate root: the nearest folder above holding main.rs or lib.rs.
                    let mut d = dir.to_owned();
                    loop {
                        if files.contains(&join(&d, "lib.rs"))
                            || files.contains(&join(&d, "main.rs"))
                        {
                            break d;
                        }
                        let up = d.rsplit_once('/').map(|(u, _)| u.to_owned());
                        match up {
                            Some(u) => d = u,
                            None if d.is_empty() => break String::new(),
                            None => d = String::new(),
                        }
                    }
                }
                _ => join(dir, ".."),
            };
            segs.remove(0);
            // The longest prefix of the path that is a module file.
            let mut found = None;
            for seg in segs {
                base = join(&base, seg);
                if let Some(f) = first(vec![format!("{base}.rs"), format!("{base}/mod.rs")]) {
                    found = Some(f);
                }
            }
            found
        }
        "py" => {
            let dots = value.chars().take_while(|c| *c == '.').count();
            let module = value[dots..].replace('.', "/");
            let base = if dots > 0 {
                (1..dots).fold(dir.to_owned(), |b, _| join(&b, ".."))
            } else {
                String::new()
            };
            let roots = if dots > 0 {
                vec![base]
            } else {
                vec![String::new(), "src".to_owned(), dir.to_owned()]
            };
            roots.iter().find_map(|r| {
                let p = join(r, &module);
                first(vec![format!("{p}.py"), format!("{p}/__init__.py")])
            })
        }
        "inc" => first(vec![join(dir, value)]).or_else(|| {
            let name = value.rsplit('/').next()?;
            match by_name.get(name).map(Vec::as_slice) {
                Some([only]) => Some(only.clone()),
                _ => None,
            }
        }),
        "jvm" => {
            let tail = format!("/{}", value.replace('.', "/"));
            files
                .iter()
                .filter(|f| {
                    let stem = f.rsplit_once('.').map_or(f.as_str(), |(s, _)| s);
                    format!("/{stem}").ends_with(&tail)
                })
                .min()
                .cloned()
        }
        _ => {
            // "js": a relative path, with or without its extension.
            let p = join(dir, value);
            let exts = ["", ".ts", ".tsx", ".js", ".jsx", ".mjs", ".cjs", ".d.ts"];
            first(exts.iter().map(|e| format!("{p}{e}")).collect())
                .or_else(|| first(exts[1..].iter().map(|e| format!("{p}/index{e}")).collect()))
        }
    }
    .filter(|f| f != from)
}

fn complexity(lines: usize, symbols: usize) -> &'static str {
    if lines > 400 || symbols > 20 {
        "complex"
    } else if lines > 100 || symbols > 6 {
        "moderate"
    } else {
        "simple"
    }
}

fn git_head(root: &Path) -> String {
    let git = root.join(".git");
    let head = std::fs::read_to_string(git.join("HEAD")).unwrap_or_default();
    let Some(reference) = head.trim().strip_prefix("ref: ") else {
        return head.trim().to_owned();
    };
    std::fs::read_to_string(git.join(reference))
        .ok()
        .or_else(|| {
            std::fs::read_to_string(git.join("packed-refs"))
                .ok()?
                .lines()
                .find_map(|l| l.strip_suffix(reference).map(|h| h.trim().to_owned()))
        })
        .map(|h| h.trim().to_owned())
        .unwrap_or_default()
}

fn frameworks(root: &Path) -> Vec<String> {
    let read = |f: &str| {
        std::fs::read_to_string(root.join(f))
            .unwrap_or_default()
            .to_lowercase()
    };
    let (pkg, cargo, py) = (
        read("package.json"),
        read("Cargo.toml"),
        read("requirements.txt") + &read("pyproject.toml"),
    );
    let checks: [(&str, bool); 12] = [
        ("React", pkg.contains("\"react\"")),
        ("Vue", pkg.contains("\"vue\"")),
        ("Next.js", pkg.contains("\"next\"")),
        ("Express", pkg.contains("\"express\"")),
        ("Vite", pkg.contains("\"vite\"")),
        (
            "Tauri",
            cargo.contains("tauri") || pkg.contains("@tauri-apps"),
        ),
        ("Tokio", cargo.contains("tokio")),
        ("Django", py.contains("django")),
        ("Flask", py.contains("flask")),
        ("FastAPI", py.contains("fastapi")),
        (
            "Spring",
            root.join("pom.xml").is_file() && read("pom.xml").contains("spring"),
        ),
        ("Cargo", !cargo.is_empty()),
    ];
    checks
        .iter()
        .filter(|(_, on)| *on)
        .map(|(n, _)| (*n).to_owned())
        .collect()
}

/// The folder each file is grouped under: the first one below what every
/// file shares, so a project that is all `src/` still gets several layers.
fn group_of(rel: &str, shared: usize) -> String {
    let parts: Vec<&str> = rel.split('/').collect();
    if parts.len() <= shared + 1 {
        if shared == 0 {
            "(project root)".to_owned()
        } else {
            parts[..shared].join("/")
        }
    } else {
        parts[..=shared].join("/")
    }
}

#[derive(Debug, Serialize, PartialEq, Eq)]
pub struct Built {
    pub files: usize,
    pub nodes: usize,
    pub edges: usize,
    pub layers: usize,
    /// Where the graph was written.
    pub path: String,
}

/// The graph for `root`, in Understand-Anything's format.
#[allow(
    clippy::too_many_lines,
    reason = "one pass from files to graph reads best in order"
)]
fn build(root: &Path) -> Value {
    let mut paths = Vec::new();
    walk(root, &mut paths);
    let infos: Vec<FileInfo> = paths
        .iter()
        .filter_map(|p| {
            if std::fs::metadata(p).ok()?.len() > MAX_FILE_BYTES {
                return None;
            }
            let rel = p
                .strip_prefix(root)
                .ok()?
                .to_string_lossy()
                .replace('\\', "/");
            let (language, kind) = classify(rel.rsplit('/').next()?)?;
            let bytes = std::fs::read(p).ok()?;
            if bytes.iter().take(8192).any(|b| *b == 0) {
                return None;
            }
            Some(analyse(
                rel,
                language,
                kind,
                &String::from_utf8_lossy(&bytes),
            ))
        })
        .collect();

    let files: HashSet<String> = infos.iter().map(|f| f.rel.clone()).collect();
    let mut by_name: HashMap<String, Vec<String>> = HashMap::new();
    for f in &infos {
        by_name
            .entry(f.rel.rsplit('/').next().unwrap_or(&f.rel).to_owned())
            .or_default()
            .push(f.rel.clone());
    }
    let file_id = |f: &FileInfo| {
        let prefix = match f.kind {
            Kind::Code => "file",
            Kind::Config => "config",
            Kind::Doc => "document",
        };
        format!("{prefix}:{}", f.rel)
    };
    let id_of: HashMap<&str, String> = infos.iter().map(|f| (f.rel.as_str(), file_id(f))).collect();

    // Imports first: summaries mention who uses a file.
    let mut import_edges: Vec<(String, String)> = Vec::new();
    for f in &infos {
        let mut seen = HashSet::new();
        for spec in &f.imports {
            if let Some(target) = resolve(spec, &f.rel, &files, &by_name) {
                if seen.insert(target.clone()) {
                    import_edges.push((f.rel.clone(), target));
                }
            }
        }
    }
    let mut used_by: HashMap<&str, usize> = HashMap::new();
    for (_, t) in &import_edges {
        *used_by.entry(t.as_str()).or_default() += 1;
    }

    let mut nodes = Vec::new();
    let mut edges = Vec::new();
    let mut ids = HashSet::new();
    for f in &infos {
        let fid = file_id(f);
        let funcs = f.symbols.iter().filter(|s| s.kind == "function").count();
        let types = f.symbols.len() - funcs;
        let users = used_by.get(f.rel.as_str()).copied().unwrap_or(0);
        let summary = f.doc.clone().unwrap_or_else(|| {
            let mut parts = vec![format!("{} file, {} lines", f.language, f.lines)];
            if funcs + types > 0 {
                parts.push(format!("defines {funcs} functions and {types} types"));
            }
            parts.join("; ") + "."
        });
        let summary = if users > 0 {
            format!(
                "{summary} Imported by {users} file{}.",
                if users == 1 { "" } else { "s" }
            )
        } else {
            summary
        };
        let top = f.rel.split_once('/').map_or("root", |(t, _)| t);
        nodes.push(json!({
            "id": fid,
            "type": match f.kind { Kind::Code => "file", Kind::Config => "config", Kind::Doc => "document" },
            "name": f.rel.rsplit('/').next().unwrap_or(&f.rel),
            "filePath": f.rel,
            "lineRange": [1, f.lines.max(1)],
            "summary": summary,
            "tags": [f.language, top],
            "complexity": complexity(f.lines, f.symbols.len()),
        }));
        ids.insert(fid.clone());
        for (i, s) in f.symbols.iter().enumerate() {
            let mut sid = format!("{}:{}:{}", s.kind, f.rel, s.name);
            if !ids.insert(sid.clone()) {
                sid = format!("{sid}#{}", s.line);
                ids.insert(sid.clone());
            }
            let end = f
                .symbols
                .get(i + 1)
                .map_or(f.lines, |n| n.line.saturating_sub(1))
                .max(s.line);
            let what = if s.kind == "function" {
                "Function"
            } else {
                "Type"
            };
            nodes.push(json!({
                "id": sid,
                "type": s.kind,
                "name": s.name,
                "filePath": f.rel,
                "lineRange": [s.line, end],
                "summary": format!("{what} `{}` in {}, from line {}.", s.name, f.rel, s.line),
                "tags": [f.language, s.kind],
                "complexity": complexity(end - s.line, 0),
            }));
            edges.push(json!({"source": fid, "target": sid, "type": "contains", "direction": "forward", "weight": 1.0}));
        }
    }
    for (s, t) in &import_edges {
        edges.push(json!({"source": id_of[s.as_str()], "target": id_of[t.as_str()], "type": "imports", "direction": "forward", "weight": 0.7}));
    }

    // Layers: one per folder, at the first level where folders differ.
    let shared = {
        let dirs: Vec<Vec<&str>> = infos
            .iter()
            .map(|f| f.rel.split('/').collect::<Vec<_>>())
            .map(|mut p| {
                p.pop();
                p
            })
            .collect();
        let mut depth = 0;
        while dirs.iter().all(|d| d.len() > depth)
            && dirs.windows(2).all(|w| w[0][depth] == w[1][depth])
            && !dirs.is_empty()
        {
            depth += 1;
        }
        depth
    };
    let mut groups: BTreeMap<String, Vec<&FileInfo>> = BTreeMap::new();
    for f in &infos {
        groups.entry(group_of(&f.rel, shared)).or_default().push(f);
    }
    // A folder holding a large share of the project (`crates/`, `packages/`)
    // says little as one layer; split it one level further.
    let big = (infos.len() * 2 / 5).max(20);
    let mut split: BTreeMap<String, Vec<&FileInfo>> = BTreeMap::new();
    for (key, members) in groups {
        let depth = key.split('/').count();
        if members.len() > big && !key.starts_with('(') {
            for f in members {
                split.entry(group_of(&f.rel, depth)).or_default().push(f);
            }
        } else {
            split.entry(key).or_default().extend(members);
        }
    }
    let mut groups: Vec<(String, Vec<&FileInfo>)> = split.into_iter().collect();
    groups.sort_by(|a, b| b.1.len().cmp(&a.1.len()).then(a.0.cmp(&b.0)));
    if groups.len() > MAX_LAYERS {
        let rest: Vec<&FileInfo> = groups
            .drain(MAX_LAYERS - 1..)
            .flat_map(|(_, f)| f)
            .collect();
        groups.push(("(other folders)".to_owned(), rest));
    }
    let mut layers = Vec::new();
    for (name, members) in &groups {
        let mut langs: Vec<&str> = members.iter().map(|f| f.language).collect();
        langs.sort_unstable();
        langs.dedup();
        layers.push(json!({
            "id": format!("layer:{name}"),
            "name": name,
            "description": format!("{} files under {name} ({}).", members.len(), langs.join(", ")),
            "nodeIds": members.iter().map(|f| file_id(f)).collect::<Vec<_>>(),
        }));
    }

    // Tour: where to start, then each layer's most-used files.
    let most_used = |members: &[&FileInfo], n: usize| -> Vec<String> {
        let mut m: Vec<&&FileInfo> = members.iter().collect();
        m.sort_by(|a, b| {
            let ua = used_by.get(a.rel.as_str()).unwrap_or(&0);
            let ub = used_by.get(b.rel.as_str()).unwrap_or(&0);
            ub.cmp(ua).then(a.rel.cmp(&b.rel))
        });
        m.into_iter().take(n).map(|f| file_id(f)).collect()
    };
    let entry_names = [
        "readme.md",
        "main.rs",
        "lib.rs",
        "main.py",
        "__main__.py",
        "app.py",
        "manage.py",
        "index.ts",
        "index.js",
        "main.ts",
        "main.js",
        "main.tsx",
        "app.tsx",
        "main.c",
        "main.cpp",
        "main.java",
        "main.go",
        "program.cs",
    ];
    let mut start: Vec<&FileInfo> = infos
        .iter()
        .filter(|f| {
            entry_names.contains(
                &f.rel
                    .rsplit('/')
                    .next()
                    .unwrap_or("")
                    .to_ascii_lowercase()
                    .as_str(),
            )
        })
        .collect();
    start.sort_by_key(|f| (f.rel.matches('/').count(), f.rel.clone()));
    let mut tour = Vec::new();
    let mut step = |title: String, description: String, node_ids: Vec<String>| {
        if !node_ids.is_empty() {
            tour.push(json!({"order": tour.len() + 1, "title": title, "description": description, "nodeIds": node_ids}));
        }
    };
    step(
        "Start here".to_owned(),
        "The project's readme and entry points: where reading usually begins.".to_owned(),
        start.iter().take(4).map(|f| file_id(f)).collect(),
    );
    for (name, members) in groups.iter().take(8) {
        step(
            format!("Inside {name}"),
            format!(
                "{} files. These are the ones the rest of the project uses most.",
                members.len()
            ),
            most_used(members, 3),
        );
    }
    let everything: Vec<&FileInfo> = infos.iter().collect();
    step(
        "Most connected files".to_owned(),
        "The files imported by the most others: change these with care.".to_owned(),
        most_used(&everything, 5)
            .into_iter()
            .filter(|id| {
                id_of
                    .iter()
                    .any(|(rel, i)| i == id && used_by.contains_key(rel))
            })
            .collect(),
    );

    let mut language_counts: BTreeMap<&str, usize> = BTreeMap::new();
    for f in infos.iter().filter(|f| f.kind == Kind::Code) {
        *language_counts.entry(f.language).or_default() += 1;
    }
    let mut languages: Vec<(&str, usize)> = language_counts.into_iter().collect();
    languages.sort_by_key(|l| std::cmp::Reverse(l.1));
    let capped = if paths.len() >= MAX_FILES {
        format!(" (the first {MAX_FILES})")
    } else {
        String::new()
    };

    json!({
        "version": "1.0.0",
        "kind": "codebase",
        "project": {
            "name": root.file_name().map_or_else(|| "project".to_owned(), |n| n.to_string_lossy().into_owned()),
            "languages": languages.iter().map(|(l, _)| *l).collect::<Vec<_>>(),
            "frameworks": frameworks(root),
            "description": format!("{MARKER} from {} files{capped}: structure only, no AI summaries. Run /understand in Workspace's chat for the full AI graph.", infos.len()),
            "analyzedAt": chrono::Utc::now().to_rfc3339(),
            "gitCommitHash": git_head(root),
        },
        "nodes": nodes,
        "edges": edges,
        "layers": layers,
        "tour": tour,
    })
}

/// Build `project`'s graph and write it. An existing graph that this module
/// did not write (an AI-built one) is replaced only when `replace` is set.
///
/// # Errors
/// The folder cannot be read, there is another graph and `replace` is
/// false, or the graph cannot be written.
pub fn write(project: &str, replace: bool) -> Result<Built, String> {
    let root = Path::new(project.trim())
        .canonicalize()
        .map_err(|e| format!("cannot open {}: {e}", project.trim()))?;
    let dir = if root.join(".understand-anything").is_dir() {
        root.join(".understand-anything")
    } else {
        root.join(".ua")
    };
    let file = dir.join("knowledge-graph.json");
    if file.is_file() && !replace {
        let existing: Value =
            serde_json::from_slice(&std::fs::read(&file).map_err(|e| e.to_string())?)
                .unwrap_or(Value::Null);
        let ours = existing["project"]["description"]
            .as_str()
            .is_some_and(|d| d.starts_with(MARKER) || d.starts_with(OLD_MARKER));
        if !ours {
            return Err(
                "EXISTS: this project already has a knowledge graph, perhaps an AI-built one"
                    .to_owned(),
            );
        }
    }
    let graph = build(&root);
    let count = |k: &str| graph[k].as_array().map_or(0, Vec::len);
    let built = Built {
        files: graph["layers"].as_array().map_or(0, |l| {
            l.iter()
                .map(|x| x["nodeIds"].as_array().map_or(0, Vec::len))
                .sum()
        }),
        nodes: count("nodes"),
        edges: count("edges"),
        layers: count("layers"),
        path: file
            .display()
            .to_string()
            .trim_start_matches(r"\\?\")
            .to_owned(),
    };
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let text = serde_json::to_vec_pretty(&graph).map_err(|e| e.to_string())?;
    ws_core::edits::restore(&file, Some(&text)).map_err(|e| e.to_string())?;
    Ok(built)
}

#[cfg(test)]
mod tests {
    // Spec s14 permits unwrap/expect in tests.
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;

    fn project(files: &[(&str, &str)]) -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        for (path, text) in files {
            let p = dir.path().join(path);
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            std::fs::write(p, text).unwrap();
        }
        dir
    }

    #[test]
    fn symbols_are_found_in_each_language() {
        let cases = [
            (
                "rust",
                "pub(crate) async fn load_all() {",
                Some(("function", "load_all")),
            ),
            ("rust", "pub struct Store {", Some(("class", "Store"))),
            (
                "python",
                "    def method(self):",
                Some(("function", "method")),
            ),
            ("python", "class Thing(Base):", Some(("class", "Thing"))),
            (
                "typescript",
                "export default function App() {",
                Some(("function", "App")),
            ),
            (
                "typescript",
                "export const useThing = () => {",
                Some(("function", "useThing")),
            ),
            ("typescript", "export const LIMIT = 5;", None),
            (
                "go",
                "func (s *Server) Serve(addr string) error {",
                Some(("function", "Serve")),
            ),
            (
                "java",
                "    public static void main(String[] args) {",
                Some(("function", "main")),
            ),
            ("java", "public class Hello {", Some(("class", "Hello"))),
            (
                "c",
                "static int helper(int x)",
                Some(("function", "helper")),
            ),
            ("c", "int x = compute(3);", None),
            ("c", "    if (ready) {", None),
            (
                "cpp",
                "void Widget::draw() const {",
                Some(("function", "draw")),
            ),
        ];
        for (lang, line, want) in cases {
            let got = symbol_on(lang, line);
            assert_eq!(
                got.as_ref().map(|(k, n)| (*k, n.as_str())),
                want,
                "{lang}: {line}"
            );
        }
    }

    #[test]
    fn imports_between_project_files_become_edges() {
        let dir = project(&[
            (
                "src/main.rs",
                "//! The app's entry point.\nmod store;\nuse crate::store::Store;\nfn main() {}\n",
            ),
            ("src/store.rs", "pub struct Store;\n"),
            (
                "web/app.ts",
                "import { x } from './util';\nimport React from 'react';\nexport function App() {}\n",
            ),
            ("web/util.ts", "export const x = () => 1;\n"),
            ("py/a.py", "from .b import thing\n\ndef run():\n    pass\n"),
            ("py/b.py", "thing = 1\n"),
            (
                "c/main.c",
                "#include \"util.h\"\n#include <stdio.h>\nint main(void) {\n}\n",
            ),
            ("c/util.h", "int util(void);\n"),
            ("README.md", "# Demo\n"),
            ("node_modules/x/index.js", "skipped\n"),
        ]);
        let g = build(dir.path());
        let imports: HashSet<(String, String)> = g["edges"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|e| e["type"] == "imports")
            .map(|e| {
                (
                    e["source"].as_str().unwrap().to_owned(),
                    e["target"].as_str().unwrap().to_owned(),
                )
            })
            .collect();
        for (s, t) in [
            ("file:src/main.rs", "file:src/store.rs"),
            ("file:web/app.ts", "file:web/util.ts"),
            ("file:py/a.py", "file:py/b.py"),
            ("file:c/main.c", "file:c/util.h"),
        ] {
            assert!(
                imports.contains(&(s.to_owned(), t.to_owned())),
                "{s} -> {t}: {imports:?}"
            );
        }
        assert_eq!(imports.len(), 4, "react and stdio are external");

        let nodes = g["nodes"].as_array().unwrap();
        let ids: HashSet<&str> = nodes.iter().map(|n| n["id"].as_str().unwrap()).collect();
        assert_eq!(ids.len(), nodes.len(), "ids are unique");
        assert!(!ids.iter().any(|i| i.contains("node_modules")));
        for e in g["edges"].as_array().unwrap() {
            assert!(
                ids.contains(e["source"].as_str().unwrap())
                    && ids.contains(e["target"].as_str().unwrap())
            );
        }
        let main = nodes
            .iter()
            .find(|n| n["id"] == "file:src/main.rs")
            .unwrap();
        assert_eq!(main["summary"], "The app's entry point.");
        let store = nodes
            .iter()
            .find(|n| n["id"] == "file:src/store.rs")
            .unwrap();
        assert!(
            store["summary"]
                .as_str()
                .unwrap()
                .ends_with("Imported by 1 file.")
        );

        // Every file is in exactly one layer, and the tour points at real nodes.
        let mut in_layers: Vec<&str> = g["layers"]
            .as_array()
            .unwrap()
            .iter()
            .flat_map(|l| l["nodeIds"].as_array().unwrap())
            .map(|i| i.as_str().unwrap())
            .collect();
        in_layers.sort_unstable();
        let mut file_ids: Vec<&str> = ids
            .iter()
            .copied()
            .filter(|i| !i.starts_with("function:") && !i.starts_with("class:"))
            .collect();
        file_ids.sort_unstable();
        assert_eq!(in_layers, file_ids);
        let tour = g["tour"].as_array().unwrap();
        assert_eq!(tour[0]["title"], "Start here");
        assert!(
            tour.iter()
                .flat_map(|s| s["nodeIds"].as_array().unwrap())
                .all(|i| ids.contains(i.as_str().unwrap()))
        );
    }

    #[test]
    fn an_ai_built_graph_is_not_replaced_without_asking() {
        let dir = project(&[("a.py", "def f():\n    pass\n")]);
        let root = dir.path().display().to_string();
        let first = write(&root, false).unwrap();
        assert_eq!((first.files, first.layers), (1, 1));
        write(&root, false).expect("its own graph is replaced freely");

        let file = dir.path().join(".ua/knowledge-graph.json");
        std::fs::write(&file, r#"{"project":{"description":"An AI graph"}}"#).unwrap();
        assert!(write(&root, false).unwrap_err().starts_with("EXISTS"));
        write(&root, true).unwrap();
        assert!(std::fs::read_to_string(&file).unwrap().contains(MARKER));
    }
}
