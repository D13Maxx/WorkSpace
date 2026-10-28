//! Skills: `SKILL.md` instruction packs in the Claude Code format, such as
//! Ponytail and Understand-Anything.
//!
//! A skill is markdown with `name` and `description` frontmatter. The model
//! sees the list and loads one with the `load_skill` tool; the user can also
//! type `/name request`, which works on every backend because it needs no
//! tool support.

use crate::tools::{ToolHost, ToolOutput};
use std::path::{Path, PathBuf};
use ws_context::{Part, ToolSpec};

pub const LOAD_SKILL: &str = "load_skill";

/// How deep discovery looks below the skills folder. A cloned repo keeps its
/// skills a level or two down (`ponytail/skills/ponytail-audit/SKILL.md`).
const MAX_DEPTH: usize = 6;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Skill {
    pub name: String,
    pub description: String,
    pub body: String,
    /// The folder holding `SKILL.md`, which a skill may refer to.
    pub dir: PathBuf,
}

#[derive(Debug, Default)]
pub struct Skills {
    list: Vec<Skill>,
}

impl Skills {
    /// Every `SKILL.md` below `root`, sorted by name. A missing folder is just
    /// no skills. If two share a name, the first in path order wins.
    #[must_use]
    pub fn load(root: &Path) -> Self {
        let mut found = Vec::new();
        walk(root, 0, &mut found);
        found.sort_by(|a, b| a.dir.cmp(&b.dir));
        let mut list: Vec<Skill> = Vec::new();
        for s in found {
            if list.iter().any(|k| k.name == s.name) {
                tracing::warn!(skill = %s.name, dir = %s.dir.display(), "duplicate skill name; skipped");
            } else {
                list.push(s);
            }
        }
        // Stable order keeps the tool description identical across turns.
        list.sort_by(|a, b| a.name.cmp(&b.name));
        Self { list }
    }

    /// Add the skills under another folder, such as one shipped beside the
    /// app. Ones already loaded keep their name.
    #[must_use]
    pub fn and_load(mut self, root: &Path) -> Self {
        let mut found = Vec::new();
        walk(root, 0, &mut found);
        found.sort_by(|a, b| a.dir.cmp(&b.dir));
        for s in found {
            if !self.list.iter().any(|k| k.name == s.name) {
                self.list.push(s);
            }
        }
        self.list.sort_by(|a, b| a.name.cmp(&b.name));
        self
    }

    /// Add skills shipped inside the app, as (folder label, `SKILL.md` text).
    /// One the user installed under the same name wins.
    #[must_use]
    pub fn with_builtin(mut self, builtin: &[(&str, &str)]) -> Self {
        for (label, text) in builtin {
            let Some((name, description, body)) = parse(text) else {
                tracing::warn!(skill = %label, "built-in SKILL.md without frontmatter; skipped");
                continue;
            };
            let dir = PathBuf::from(label);
            let name = name.unwrap_or_else(|| dir_name(&dir));
            if !self.list.iter().any(|s| s.name == name) {
                self.list.push(Skill {
                    name,
                    description,
                    body,
                    dir,
                });
            }
        }
        self.list.sort_by(|a, b| a.name.cmp(&b.name));
        self
    }

    #[must_use]
    pub fn list(&self) -> &[Skill] {
        &self.list
    }

    #[must_use]
    pub fn get(&self, name: &str) -> Option<&Skill> {
        self.list.iter().find(|s| s.name == name)
    }

    /// Turn a typed `/name request` into the message the model receives: the
    /// skill's instructions, then the request. `None` if it is not a skill
    /// invocation, so the text is sent as typed.
    #[must_use]
    pub fn expand(&self, typed: &str) -> Option<Vec<Part>> {
        let rest = typed.trim_start().strip_prefix('/')?;
        let (name, request) = rest.split_once(char::is_whitespace).unwrap_or((rest, ""));
        let skill = self.get(name)?;
        let mut parts = vec![Part::text(skill.wrapped(request))];
        if !request.trim().is_empty() {
            parts.push(Part::text(request.trim()));
        }
        Some(parts)
    }
}

impl Skill {
    /// The body in a tagged envelope, so the model can tell instructions from
    /// the request, and the UI can collapse them.
    fn wrapped(&self, args: &str) -> String {
        format!(
            "<skill name=\"{}\" dir=\"{}\">\n{}\n</skill>",
            self.name,
            self.dir.display(),
            self.body.replace("$ARGUMENTS", args.trim()).trim()
        )
    }
}

/// Split a message's first part back into (skill name, instructions), if it
/// is a skill envelope.
#[must_use]
pub fn unwrap_envelope(text: &str) -> Option<(&str, &str)> {
    let rest = text.strip_prefix("<skill name=\"")?;
    let (name, rest) = rest.split_once('"')?;
    let body = rest.split_once('\n')?.1.strip_suffix("</skill>")?;
    Some((name, body.trim_end()))
}

fn walk(dir: &Path, depth: usize, out: &mut Vec<Skill>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if path.is_dir() {
            let skip = name.starts_with('.') || name == "node_modules" || name == "target";
            if !skip && depth < MAX_DEPTH {
                walk(&path, depth + 1, out);
            }
        } else if name == "SKILL.md" {
            match std::fs::read_to_string(&path).ok().and_then(|t| parse(&t)) {
                Some((n, description, body)) => out.push(Skill {
                    name: n.unwrap_or_else(|| dir_name(dir)),
                    description,
                    body,
                    dir: dir.to_path_buf(),
                }),
                None => {
                    tracing::warn!(file = %path.display(), "SKILL.md without frontmatter; skipped")
                }
            }
        }
    }
}

fn dir_name(dir: &Path) -> String {
    dir.file_name()
        .map_or_else(String::new, |n| n.to_string_lossy().into_owned())
}

/// Read `name` and `description` from YAML frontmatter, and the body after it.
///
/// shortcut: handles plain, quoted and `>`/`|` block values only, not full YAML; enough for Claude Code skills, swap in a YAML crate if one breaks.
pub(crate) fn parse(text: &str) -> Option<(Option<String>, String, String)> {
    let text = text.trim_start_matches('\u{feff}');
    let mut lines = text.lines();
    if lines.next()?.trim_end() != "---" {
        return None;
    }
    let mut front = Vec::new();
    for line in lines.by_ref() {
        if line.trim_end() == "---" {
            let body: Vec<&str> = lines.collect();
            let (name, description) = (field(&front, "name"), field(&front, "description"));
            return Some((name, description.unwrap_or_default(), body.join("\n")));
        }
        front.push(line);
    }
    None
}

fn field(front: &[&str], key: &str) -> Option<String> {
    let start = front.iter().position(|l| {
        l.strip_prefix(key)
            .is_some_and(|r| r.trim_start().starts_with(':'))
    })?;
    let value = front[start].split_once(':')?.1.trim();
    if let Some(style) = value.chars().next().filter(|c| *c == '>' || *c == '|') {
        // A block scalar: the indented lines that follow.
        let block: Vec<&str> = front[start + 1..]
            .iter()
            .take_while(|l| l.trim().is_empty() || l.starts_with([' ', '\t']))
            .map(|l| l.trim())
            .collect();
        let sep = if style == '>' { " " } else { "\n" };
        return Some(block.join(sep).trim().to_owned());
    }
    Some(value.trim_matches(|c| c == '"' || c == '\'').to_owned())
}

#[async_trait::async_trait]
impl ToolHost for Skills {
    fn specs(&self) -> Vec<ToolSpec> {
        if self.list.is_empty() {
            return Vec::new();
        }
        let menu: String = self
            .list
            .iter()
            .map(|s| format!("\n- {}: {}", s.name, s.description))
            .collect();
        vec![ToolSpec {
            name: LOAD_SKILL.to_owned(),
            description: format!(
                "Load a skill: detailed instructions for a kind of task. When the \
                 request matches one, load it before starting and follow it. \
                 Available skills:{menu}"
            ),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {"name": {"type": "string", "description": "The skill's name"}},
                "required": ["name"],
            }),
        }]
    }

    fn needs_approval(&self, _name: &str) -> bool {
        false // reading instructions changes nothing
    }

    async fn call(&self, _name: &str, args: serde_json::Value) -> ToolOutput {
        let name = args
            .get("name")
            .and_then(|v| v.as_str())
            .unwrap_or_default();
        match self.get(name) {
            Some(skill) => ToolOutput::ok(skill.wrapped("")),
            None => ToolOutput::error(format!("there is no skill called {name:?}")),
        }
    }
}

#[cfg(test)]
mod tests {
    // Spec s14 permits unwrap/expect in tests.
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;

    /// Ponytail's real frontmatter shape: a folded description and extras.
    const PONYTAIL: &str = "---\nname: ponytail\ndescription: >\n  Lazy senior dev mode: the smallest\n  change that works.\nargument-hint: \"[lite|full|ultra]\"\nlicense: MIT\n---\n\n# Ponytail\n\nBe lazy.\n";

    fn skills_in(files: &[(&str, &str)]) -> (tempfile::TempDir, Skills) {
        let dir = tempfile::tempdir().unwrap();
        for (path, text) in files {
            let p = dir.path().join(path);
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            std::fs::write(p, text).unwrap();
        }
        let skills = Skills::load(dir.path());
        (dir, skills)
    }

    #[test]
    fn frontmatter_with_a_folded_description_is_read() {
        let (name, description, body) = parse(PONYTAIL).unwrap();
        assert_eq!(name.as_deref(), Some("ponytail"));
        assert_eq!(
            description,
            "Lazy senior dev mode: the smallest change that works."
        );
        assert!(body.contains("Be lazy."));
        assert!(
            !body.contains("license"),
            "frontmatter is not part of the body"
        );
    }

    #[test]
    fn quoted_and_crlf_frontmatter_is_read() {
        let text = "---\r\nname: \"x\"\r\ndescription: 'does x'\r\n---\r\nbody\r\n";
        let (name, description, body) = parse(text).unwrap();
        assert_eq!(
            (name.as_deref(), description.as_str(), body.as_str()),
            (Some("x"), "does x", "body")
        );
    }

    #[test]
    fn a_file_without_frontmatter_is_not_a_skill() {
        assert!(parse("# just markdown").is_none());
        assert!(parse("---\nname: x\nno closing fence").is_none());
    }

    #[test]
    fn skills_are_found_in_cloned_repos_and_hidden_folders_are_skipped() {
        let (_d, skills) = skills_in(&[
            ("ponytail/skills/ponytail/SKILL.md", PONYTAIL),
            (
                "ponytail/skills/audit/SKILL.md",
                "---\nname: ponytail-audit\ndescription: audit\n---\nAudit.",
            ),
            (
                "ponytail/.git/SKILL.md",
                "---\nname: hidden\ndescription: no\n---\n",
            ),
            (
                "other/SKILL.md",
                "---\ndescription: unnamed\n---\nUses the folder name.",
            ),
        ]);
        let names: Vec<_> = skills.list().iter().map(|s| s.name.as_str()).collect();
        assert_eq!(names, ["other", "ponytail", "ponytail-audit"]);
    }

    #[test]
    fn a_slash_command_puts_the_skill_before_the_request() {
        let (_d, skills) = skills_in(&[("p/SKILL.md", PONYTAIL)]);
        let parts = skills.expand("/ponytail fix the parser").unwrap();
        assert_eq!(parts.len(), 2);
        let first = parts[0].as_text().unwrap();
        let (name, body) = unwrap_envelope(first).unwrap();
        assert_eq!(name, "ponytail");
        assert!(body.contains("Be lazy."));
        assert_eq!(parts[1].as_text(), Some("fix the parser"));

        assert_eq!(
            skills.expand("/ponytail").unwrap().len(),
            1,
            "no request, no empty part"
        );
        assert!(
            skills.expand("/unknown thing").is_none(),
            "not a skill: sent as typed"
        );
        assert!(skills.expand("no slash").is_none());
    }

    #[tokio::test]
    async fn the_model_sees_the_menu_and_loads_a_skill_by_name() {
        let (_d, skills) = skills_in(&[("p/SKILL.md", PONYTAIL)]);
        let specs = skills.specs();
        assert_eq!(specs.len(), 1);
        assert!(
            specs[0]
                .description
                .contains("- ponytail: Lazy senior dev mode")
        );
        assert!(!skills.needs_approval(LOAD_SKILL));

        let out = skills
            .call(LOAD_SKILL, serde_json::json!({"name": "ponytail"}))
            .await;
        assert!(!out.is_error && out.text.contains("Be lazy."));
        let missing = skills
            .call(LOAD_SKILL, serde_json::json!({"name": "nope"}))
            .await;
        assert!(missing.is_error);
    }

    #[test]
    fn no_skills_means_no_tool() {
        assert!(Skills::default().specs().is_empty());
    }

    #[test]
    fn built_in_skills_join_the_list_but_the_users_own_win() {
        let mine = "---\nname: ponytail\ndescription: my edit\n---\nmine\n";
        let (_d, skills) = skills_in(&[("ponytail/SKILL.md", mine)]);
        let other = "---\nname: understand-chat\ndescription: ask the graph\n---\nask\n";
        let skills = skills.with_builtin(&[
            ("builtin/ponytail", PONYTAIL),
            ("builtin/understand-chat", other),
            ("builtin/broken", "no frontmatter"),
        ]);
        let names: Vec<_> = skills.list().iter().map(|s| s.name.as_str()).collect();
        assert_eq!(names, ["ponytail", "understand-chat"]);
        assert_eq!(skills.get("ponytail").unwrap().description, "my edit");

        let (shipped, _) = skills_in(&[(
            "skills/understand/SKILL.md",
            "---\nname: understand\ndescription: build a graph\n---\nbuild\n",
        )]);
        let skills = skills.and_load(shipped.path());
        let understand = skills.get("understand").unwrap();
        assert!(
            understand.dir.ends_with("skills/understand"),
            "a real folder"
        );
    }
}
