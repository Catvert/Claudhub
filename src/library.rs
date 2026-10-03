//! The agent library: every skill an agent started in a worktree can read —
//! a folder holding a `SKILL.md` —, found where Claude Code and Codex look
//! for them, and what the library plugin lists, reads and launches.
//!
//! **Three scopes**, and only the first one travels: the **project**'s
//! (`.claude/skills/`, `.agents/skills/`, `.codex/skills/` of the checkout —
//! committed, so a teammate who pulls has them), the **user**'s (under the
//! home, for every repository) and those an installed Claude Code **plugin**
//! brings (`installed_plugins.json`, user-wide or for this project). Sharing
//! a skill is copying its folder into the project's ([`share`]): the next
//! commit hands it over.
//!
//! **Which agent reads it of itself** is said by where it lies — Claude's
//! folders or Codex's —, and is a hint, not a fence: the plugin's prompt
//! names the `SKILL.md`, which any agent can open.
//!
//! **A skill has its companion prompt in the project** — what this team
//! asks of it, written once: `.claudhub/prompts/<name>.md`, versioned beside
//! the notes, and keyed by the skill's name, so it follows the skill whatever
//! folder it is read from ([`save_prompt`]).
//!
//! A `SKILL.md` is read leniently ([`parse`]): its front matter is a few
//! `key: value` lines, and a missing name or description falls back on the
//! folder's name and the first line of prose.
//!
//! Pure but for [`discover`], [`share`] and [`save_prompt`], which the worker
//! calls.

use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};

/// The file a skill's folder is recognised by.
pub const FILE: &str = "SKILL.md";

/// How much of a `SKILL.md` travels with the listing: enough to read it in
/// the library, not a manual pasted whole.
const BODY_MAX: usize = 24 * 1024;

/// How deep a folder of skills is walked: `synced/<bucket>/<skill>` is
/// Claude's deepest.
const DEPTH: usize = 3;

/// Where the project keeps the skills' companion prompts, one file each.
pub const PROMPTS: &str = ".claudhub/prompts";

/// What a copy into the project may weigh: a skill is instructions and a
/// few scripts, and a folder ten times that is a mistake to stop.
const SHARE_FILES: usize = 500;
const SHARE_BYTES: u64 = 16 * 1024 * 1024;

/// Where a skill comes from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum Scope {
    /// The checkout's own: versioned, shared by a commit.
    Project,
    /// The user's, for every repository.
    Personal,
    /// Brought by an installed Claude Code plugin.
    Plugin,
}

/// The agent that finds a skill of itself where it lies.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum Reader {
    Claude,
    Codex,
}

impl Reader {
    /// The project folder a skill of this reader is shared into.
    pub fn project_folder(self) -> &'static str {
        match self {
            Self::Claude => ".claude/skills",
            Self::Codex => ".agents/skills",
        }
    }
}

/// A skill found.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Skill {
    pub name: String,
    pub description: String,
    /// What it takes after its name, as its front matter says
    /// (`argument-hint`); empty when it says nothing.
    pub argument_hint: String,
    /// Its folder.
    pub dir: PathBuf,
    pub scope: Scope,
    pub reader: Reader,
    /// The plugin it comes with, by name.
    pub plugin: Option<String>,
    /// The `SKILL.md` past its front matter, cut at [`BODY_MAX`].
    pub body: String,
    /// The project's companion prompt for it; empty when it has none.
    pub prompt: String,
}

impl Skill {
    /// Its `SKILL.md`.
    pub fn file(&self) -> PathBuf {
        self.dir.join(FILE)
    }
}

/// A `SKILL.md` read: its front matter's fields, and the rest.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Header {
    pub name: Option<String>,
    pub description: Option<String>,
    pub argument_hint: Option<String>,
    pub body: String,
}

/// Reads a `SKILL.md`. The front matter is YAML in principle and a handful
/// of `key: value` lines in practice: plain, quoted, or a block (`>`, `|`)
/// whose indented lines follow — what the skills in the wild are written
/// with. A file without front matter is all body.
pub fn parse(text: &str) -> Header {
    let text = text.strip_prefix('\u{feff}').unwrap_or(text);
    let mut lines = text.split_inclusive('\n');
    let first = lines.clone().next().unwrap_or_default();
    if first.trim_end() != "---" {
        return fallback(Header {
            body: text.to_string(),
            ..Header::default()
        });
    }
    lines.next();
    let mut fields: Vec<(String, String)> = Vec::new();
    let mut consumed = first.len();
    let mut closed = false;
    // The block being read: its key, and whether its lines keep their breaks.
    let mut block: Option<(String, bool)> = None;
    for line in lines {
        consumed += line.len();
        let bare = line.trim_end_matches(['\n', '\r']);
        if bare.trim_end() == "---" {
            closed = true;
            break;
        }
        let indented = bare.starts_with(' ') || bare.starts_with('\t');
        if indented || (bare.trim().is_empty() && block.is_some()) {
            // A continuation: of a block, or of a plain value folded over lines.
            let piece = bare.trim();
            if let Some((_, value)) = fields.last_mut() {
                let literal = block.as_ref().is_some_and(|(_, literal)| *literal);
                if !value.is_empty() {
                    value.push(if literal { '\n' } else { ' ' });
                }
                value.push_str(piece);
            }
            continue;
        }
        block = None;
        let Some((key, value)) = bare.split_once(':') else {
            continue;
        };
        let (key, value) = (key.trim().to_string(), value.trim());
        if let Some(style) = value.strip_prefix(['>', '|']) {
            let indicator = |c: char| c == '-' || c == '+' || c.is_ascii_digit();
            if style.trim_start_matches(indicator).is_empty() {
                block = Some((key.clone(), value.starts_with('|')));
                fields.push((key, String::new()));
                continue;
            }
        }
        fields.push((key, unquote(value)));
    }
    if !closed {
        return fallback(Header {
            body: text.to_string(),
            ..Header::default()
        });
    }
    let field = |name: &str| {
        fields
            .iter()
            .find(|(key, _)| key == name)
            .map(|(_, value)| value.trim().to_string())
            .filter(|value| !value.is_empty())
    };
    fallback(Header {
        name: field("name"),
        description: field("description"),
        argument_hint: field("argument-hint"),
        body: text[consumed.min(text.len())..]
            .trim_start_matches(['\n', '\r'])
            .to_string(),
    })
}

/// A description the front matter does not give: the first line of prose.
fn fallback(mut header: Header) -> Header {
    if header.description.is_none() {
        let mut fenced = false;
        header.description = header
            .body
            .lines()
            .map(str::trim)
            .find(|line| {
                if line.starts_with("```") {
                    fenced = !fenced;
                    return false;
                }
                !fenced && !line.is_empty() && !line.starts_with('#')
            })
            .map(|line| crate::text::ellipsized(line, 200));
    }
    header
}

/// A value without the quotes YAML wraps it in.
fn unquote(value: &str) -> String {
    for quote in ['"', '\''] {
        if let Some(inner) = value
            .strip_prefix(quote)
            .and_then(|rest| rest.strip_suffix(quote))
        {
            return match quote {
                '"' => inner.replace("\\\"", "\"").replace("\\n", "\n"),
                _ => inner.replace("''", "'"),
            };
        }
    }
    value.to_string()
}

/// A folder of skills, and what the skills under it are.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Root {
    pub dir: PathBuf,
    pub scope: Scope,
    pub reader: Reader,
    pub plugin: Option<String>,
}

impl Root {
    fn new(dir: PathBuf, scope: Scope, reader: Reader) -> Self {
        Self {
            dir,
            scope,
            reader,
            plugin: None,
        }
    }
}

/// The folders where the agents look, the project's first: what is found
/// twice — a symbolic link, a skill installed both ways — is listed where it
/// is found first.
///
/// `claude` is Claude Code's configuration (`~/.claude`), `codex` Codex's
/// (`$CODEX_HOME`, `~/.codex`); `plugins` is Claude's
/// `installed_plugins.json`, read.
pub fn roots(
    worktree: &Path,
    home: Option<&Path>,
    claude: Option<&Path>,
    codex: Option<&Path>,
    plugins: Option<&str>,
) -> Vec<Root> {
    let mut roots = vec![
        Root::new(
            worktree.join(".claude/skills"),
            Scope::Project,
            Reader::Claude,
        ),
        Root::new(
            worktree.join(".agents/skills"),
            Scope::Project,
            Reader::Codex,
        ),
        Root::new(
            worktree.join(".codex/skills"),
            Scope::Project,
            Reader::Codex,
        ),
    ];
    if let Some(claude) = claude {
        roots.push(Root::new(
            claude.join("skills"),
            Scope::Personal,
            Reader::Claude,
        ));
    }
    if let Some(home) = home {
        roots.push(Root::new(
            home.join(".agents/skills"),
            Scope::Personal,
            Reader::Codex,
        ));
    }
    if let Some(codex) = codex {
        roots.push(Root::new(
            codex.join("skills"),
            Scope::Personal,
            Reader::Codex,
        ));
    }
    for (name, dir) in plugins.map_or_else(Vec::new, |text| plugin_dirs(text, worktree)) {
        roots.push(Root {
            dir: dir.join("skills"),
            scope: Scope::Plugin,
            reader: Reader::Claude,
            plugin: Some(name),
        });
    }
    roots
}

/// The plugins Claude Code has installed that apply to `worktree` — for the
/// user, or for a project the worktree is in —, by name and folder.
///
/// `installed_plugins.json` keys each plugin `name@marketplace` and lists
/// its installations; a plugin installed twice is one folder for each.
pub fn plugin_dirs(text: &str, worktree: &Path) -> Vec<(String, PathBuf)> {
    let Ok(value) = serde_json::from_str::<serde_json::Value>(text) else {
        return Vec::new();
    };
    let Some(plugins) = value.get("plugins").and_then(|plugins| plugins.as_object()) else {
        return Vec::new();
    };
    let mut found: Vec<(String, PathBuf)> = Vec::new();
    for (key, installs) in plugins {
        let name = key.split('@').next().unwrap_or(key).to_string();
        let installs = installs.as_array().map(Vec::as_slice).unwrap_or_default();
        for install in installs {
            let applies = match crate::json::string(install, "scope") {
                Some("user") | None => true,
                Some(_) => crate::json::string(install, "projectPath")
                    .is_some_and(|project| worktree.starts_with(project)),
            };
            let Some(dir) = crate::json::string(install, "installPath") else {
                continue;
            };
            let dir = PathBuf::from(dir);
            if applies && !found.iter().any(|(_, known)| *known == dir) {
                found.push((name.clone(), dir));
            }
        }
    }
    found.sort();
    found
}

/// The skills of every root, in the roots' order, each root's by name.
pub fn collect(roots: &[Root]) -> Vec<Skill> {
    let mut skills: Vec<Skill> = Vec::new();
    let mut seen: Vec<PathBuf> = Vec::new();
    for root in roots {
        let mut dirs = Vec::new();
        walk(&root.dir, DEPTH, &mut dirs);
        let mut found: Vec<Skill> = dirs
            .into_iter()
            .filter_map(|dir| {
                let real = std::fs::canonicalize(&dir).unwrap_or_else(|_| dir.clone());
                if seen.contains(&real) {
                    return None;
                }
                seen.push(real);
                let text = std::fs::read_to_string(dir.join(FILE)).ok()?;
                Some(skill(&dir, &text, root))
            })
            .collect();
        found.sort_by_key(|skill| skill.name.to_lowercase());
        skills.extend(found);
    }
    skills
}

/// A skill of `root`, from its folder and its `SKILL.md`.
fn skill(dir: &Path, text: &str, root: &Root) -> Skill {
    let header = parse(text);
    let folder = dir
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default();
    Skill {
        name: header.name.unwrap_or(folder),
        description: header.description.unwrap_or_default(),
        argument_hint: header.argument_hint.unwrap_or_default(),
        dir: dir.to_path_buf(),
        scope: root.scope,
        reader: root.reader,
        plugin: root.plugin.clone(),
        body: crate::text::head_bytes(&header.body, BODY_MAX).to_string(),
        prompt: String::new(),
    }
}

/// The folders under `dir` holding a `SKILL.md`, `depth` levels down: a
/// skill's own folders are not looked into, nor the hidden ones but Codex's
/// `.system`, where its bundled skills are.
fn walk(dir: &Path, depth: usize, found: &mut Vec<PathBuf>) {
    if depth == 0 {
        return;
    }
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    let mut dirs: Vec<(String, PathBuf)> = entries
        .flatten()
        .filter(|entry| entry.path().is_dir())
        .filter_map(|entry| Some((entry.file_name().into_string().ok()?, entry.path())))
        .filter(|(name, _)| !name.starts_with('.') || name == ".system")
        .collect();
    dirs.sort();
    for (_, path) in dirs {
        if path.join(FILE).is_file() {
            found.push(path);
        } else {
            walk(&path, depth - 1, found);
        }
    }
}

/// Every skill an agent started in `worktree` can read, where this process
/// finds the user's folders.
pub fn discover(worktree: &Path) -> Vec<Skill> {
    let home = crate::home::home();
    let claude = crate::home::claude_config();
    let codex = std::env::var_os("CODEX_HOME")
        .filter(|dir| !dir.is_empty())
        .map(PathBuf::from)
        .or_else(|| home.as_ref().map(|home| home.join(".codex")));
    let plugins = claude
        .as_ref()
        .and_then(|dir| std::fs::read_to_string(dir.join("plugins/installed_plugins.json")).ok());
    let mut skills = collect(&roots(
        worktree,
        home.as_deref(),
        claude.as_deref(),
        codex.as_deref(),
        plugins.as_deref(),
    ));
    for skill in &mut skills {
        if let Some(file) = prompt_file(&skill.name) {
            skill.prompt = std::fs::read_to_string(worktree.join(file))
                .map(|text| text.trim_end().to_string())
                .unwrap_or_default();
        }
    }
    skills
}

/// The file of the project holding skill `name`'s companion prompt,
/// relative to the worktree: the name made a file name — what is neither a
/// letter, a digit, `-`, `_` nor `.` becomes `-` —, `None` when nothing is
/// left of it.
pub fn prompt_file(name: &str) -> Option<PathBuf> {
    let slug: String = name
        .trim()
        .chars()
        .map(|c| {
            if c.is_alphanumeric() || matches!(c, '-' | '_' | '.') {
                c
            } else {
                '-'
            }
        })
        .collect();
    let slug = slug.trim_matches(|c| c == '.' || c == '-');
    (!slug.is_empty()).then(|| Path::new(PROMPTS).join(format!("{slug}.md")))
}

/// Writes skill `name`'s companion prompt into the project — removes it
/// when `text` is blank. Answers the file, relative to the worktree.
pub fn save_prompt(worktree: &Path, name: &str, text: &str) -> Result<PathBuf> {
    let relative =
        prompt_file(name).with_context(|| format!("`{name}` cannot name a prompt's file"))?;
    let path = worktree.join(&relative);
    if text.trim().is_empty() {
        match std::fs::remove_file(&path) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => {
                return Err(error).with_context(|| format!("removing {}", relative.display()))
            }
        }
        return Ok(relative);
    }
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(&path, format!("{}\n", text.trim_end()))
        .with_context(|| format!("writing {}", relative.display()))?;
    Ok(relative)
}

/// Copies skill folder `dir` into `worktree`'s folder for `reader`, under
/// its own name: from there, a commit shares it. Refused when a skill of
/// that name is there already — nothing of the project's is overwritten —,
/// and when the folder weighs more than a skill does. Answers the copy's
/// path, relative to the worktree.
pub fn share(worktree: &Path, dir: &Path, reader: Reader) -> Result<PathBuf> {
    if !dir.join(FILE).is_file() {
        bail!("{} holds no {FILE}", dir.display());
    }
    if dir.starts_with(worktree) {
        bail!("{} is already the project's", dir.display());
    }
    let name = dir
        .file_name()
        .with_context(|| format!("{} has no name", dir.display()))?;
    let relative = Path::new(reader.project_folder()).join(name);
    let target = worktree.join(&relative);
    if target.exists() {
        bail!("the project has a skill at {} already", relative.display());
    }
    let mut files = Vec::new();
    let mut bytes = 0;
    listed(dir, Path::new(""), &mut files, &mut bytes, 0)?;
    let copied = files.iter().try_for_each(|file| {
        let to = target.join(file);
        if let Some(parent) = to.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::copy(dir.join(file), &to).map(|_| ())
    });
    if let Err(error) = copied {
        let _ = std::fs::remove_dir_all(&target);
        return Err(error).with_context(|| format!("copying into {}", relative.display()));
    }
    Ok(relative)
}

/// The files under `dir`, relative to it, counted against the limits of a
/// copy. A link is followed: a skill linked in is shared by its content.
fn listed(
    root: &Path,
    at: &Path,
    files: &mut Vec<PathBuf>,
    bytes: &mut u64,
    depth: usize,
) -> Result<()> {
    if depth > 8 {
        bail!("{} is nested too deep for a skill", root.display());
    }
    for entry in std::fs::read_dir(root.join(at))? {
        let entry = entry?;
        let relative = at.join(entry.file_name());
        let meta = std::fs::metadata(entry.path())?;
        if meta.is_dir() {
            listed(root, &relative, files, bytes, depth + 1)?;
        } else if meta.is_file() {
            *bytes += meta.len();
            files.push(relative);
            if files.len() > SHARE_FILES || *bytes > SHARE_BYTES {
                bail!("{} is too large to be a skill", root.display());
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_front_matter_gives_its_fields_and_the_rest_is_body() {
        let header = parse(
            "---\nname: grill-me\ndescription: \"Interview the user: relentlessly\"\nargument-hint: '[plan]'\n---\n\n# Grill\n\nAsk.\n",
        );
        assert_eq!(header.name.as_deref(), Some("grill-me"));
        assert_eq!(
            header.description.as_deref(),
            Some("Interview the user: relentlessly")
        );
        assert_eq!(header.argument_hint.as_deref(), Some("[plan]"));
        assert_eq!(header.body, "# Grill\n\nAsk.\n");
    }

    /// The forms a description is written in across lines: a folded block,
    /// a literal one, a plain value carried on.
    #[test]
    fn a_description_over_several_lines_is_read_whole() {
        let folded =
            parse("---\nname: a\ndescription: >\n  Use when\n  testing.\nlicense: MIT\n---\nbody");
        assert_eq!(folded.description.as_deref(), Some("Use when testing."));
        let literal = parse("---\ndescription: |-\n  one\n  two\n---\n");
        assert_eq!(literal.description.as_deref(), Some("one\ntwo"));
        let plain = parse("---\ndescription: Use when\n  the plan\n---\n");
        assert_eq!(plain.description.as_deref(), Some("Use when the plan"));
    }

    /// No front matter, or one never closed: all body, the description the
    /// first line of prose.
    #[test]
    fn without_a_front_matter_the_prose_describes() {
        let header = parse("# Title\n\n```\ncode\n```\nDoes things.\n");
        assert_eq!(header.name, None);
        assert_eq!(header.description.as_deref(), Some("Does things."));
        let open = parse("---\nname: x\nNo end.\n");
        assert_eq!(open.name, None);
        assert!(open.body.starts_with("---"));
    }

    /// A companion prompt is filed by the skill's name, made a file name;
    /// blank, it is removed.
    #[test]
    fn a_companion_prompt_is_a_file_of_the_project_named_after_its_skill() {
        assert_eq!(
            prompt_file("grill-me"),
            Some(PathBuf::from(".claudhub/prompts/grill-me.md"))
        );
        assert_eq!(
            prompt_file("engineering:code review"),
            Some(PathBuf::from(
                ".claudhub/prompts/engineering-code-review.md"
            ))
        );
        assert_eq!(
            prompt_file("../x"),
            Some(PathBuf::from(".claudhub/prompts/x.md"))
        );
        assert_eq!(prompt_file(" ./ "), None);

        let worktree =
            std::env::temp_dir().join(format!("claudhub-prompts-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&worktree);
        let file = save_prompt(&worktree, "deploy", "Ship {{branch}}.\n\n").unwrap();
        assert_eq!(
            std::fs::read_to_string(worktree.join(&file)).unwrap(),
            "Ship {{branch}}.\n"
        );
        save_prompt(&worktree, "deploy", "  ").unwrap();
        assert!(!worktree.join(&file).exists());
        // Removing what is not there is no error.
        save_prompt(&worktree, "deploy", "").unwrap();
        let _ = std::fs::remove_dir_all(&worktree);
    }

    #[test]
    fn a_plugin_applies_to_the_user_or_to_its_project() {
        let text = r#"{"version": 2, "plugins": {
            "superpowers@market": [
                {"scope": "project", "projectPath": "/p/app", "installPath": "/c/superpowers/4"},
                {"scope": "project", "projectPath": "/p/other", "installPath": "/c/superpowers/3"}
            ],
            "sentry@official": [{"scope": "user", "installPath": "/c/sentry/1"}],
            "broken@x": [{"scope": "user"}]
        }}"#;
        assert_eq!(
            plugin_dirs(text, Path::new("/p/app/sub")),
            [
                ("sentry".to_string(), PathBuf::from("/c/sentry/1")),
                ("superpowers".to_string(), PathBuf::from("/c/superpowers/4")),
            ]
        );
        assert!(plugin_dirs("not json", Path::new("/p")).is_empty());
    }

    /// The project's first, a skill found twice listed once, the hidden
    /// folders passed but Codex's bundled ones; then a personal skill copied
    /// into the project, once.
    #[test]
    fn skills_are_found_where_the_agents_look_and_shared_into_the_project() {
        let base = std::env::temp_dir().join(format!("claudhub-library-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        let (worktree, home) = (base.join("w"), base.join("home"));
        let write = |path: PathBuf, text: &str| {
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, text).unwrap();
        };
        write(
            worktree.join(".claude/skills/deploy/SKILL.md"),
            "---\nname: deploy\ndescription: Ships it\n---\nSteps.",
        );
        write(
            home.join(".claude/skills/synced/bucket/pdf/SKILL.md"),
            "---\nname: pdf\ndescription: PDFs\n---\n",
        );
        write(home.join(".claude/skills/pdf/scripts/run.sh"), "echo");
        write(
            home.join(".claude/skills/pdf/SKILL.md"),
            "---\nname: pdf\ndescription: PDFs\n---\n",
        );
        write(home.join(".claude/skills/.staging/x/SKILL.md"), "hidden");
        write(
            home.join(".codex/skills/.system/creator/SKILL.md"),
            "---\nname: creator\n---\nMakes skills.",
        );
        let roots = roots(
            &worktree,
            Some(&home),
            Some(&home.join(".claude")),
            Some(&home.join(".codex")),
            None,
        );
        let found: Vec<(String, Scope, Reader)> = collect(&roots)
            .into_iter()
            .map(|skill| (skill.name, skill.scope, skill.reader))
            .collect();
        assert_eq!(
            found,
            [
                ("deploy".into(), Scope::Project, Reader::Claude),
                ("pdf".into(), Scope::Personal, Reader::Claude),
                ("pdf".into(), Scope::Personal, Reader::Claude),
                ("creator".into(), Scope::Personal, Reader::Codex),
            ]
        );

        let pdf = home.join(".claude/skills/pdf");
        let shared = share(&worktree, &pdf, Reader::Claude).unwrap();
        assert_eq!(shared, Path::new(".claude/skills/pdf"));
        assert!(worktree.join(".claude/skills/pdf/scripts/run.sh").is_file());
        // Twice is refused, and so is a project skill or a folder that is none.
        assert!(share(&worktree, &pdf, Reader::Claude).is_err());
        assert!(share(
            &worktree,
            &worktree.join(".claude/skills/deploy"),
            Reader::Codex
        )
        .is_err());
        assert!(share(&worktree, &home, Reader::Claude).is_err());
        let _ = std::fs::remove_dir_all(&base);
    }
}
