//! The Claude Code skill Claudhub ships: what tells an agent where the home
//! screen's nodes live and how they are written (`crate::canvas`).
//!
//! **Two places**, and the choice is the user's: the repository
//! (`.claude/skills/claudhub/`, committed — a teammate's Claude learns the
//! format too, Claudhub or not) or the user (`~/.claude/skills/claudhub/`,
//! for every repository, and the way round a `.claude/` one may not write).
//!
//! **A version in the text** (`claudhub-skill-version`), which is what says an
//! installed copy is behind the one this build carries. What we did not
//! write — no mark — is never overwritten nor removed.

use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};

/// The skill, as this build ships it.
pub const TEXT: &str = include_str!("../assets/skills/claudhub/SKILL.md");

const MARK: &str = "claudhub-skill-version:";

/// Where a skill is installed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum Scope {
    /// The repository's `.claude/skills/`, in this checkout.
    Repo,
    /// The user's `~/.claude/skills/`.
    User,
}

/// What is installed where: the version found, `Some(0)` for a file there
/// that is not ours.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Status {
    pub repo: Option<u32>,
    pub user: Option<u32>,
}

/// The version this build ships.
pub fn version() -> u32 {
    version_of(TEXT).unwrap_or(0)
}

/// The version a skill file says it is.
pub fn version_of(text: &str) -> Option<u32> {
    let at = text.find(MARK)? + MARK.len();
    text[at..]
        .split(|c: char| !c.is_ascii_digit() && c != ' ')
        .next()?
        .trim()
        .parse()
        .ok()
}

fn file(scope: Scope, worktree: &Path) -> Option<PathBuf> {
    let root = match scope {
        Scope::Repo => worktree.join(".claude"),
        Scope::User => std::env::var_os("CLAUDE_CONFIG_DIR")
            .map(PathBuf::from)
            .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".claude")))?,
    };
    Some(root.join("skills").join("claudhub").join("SKILL.md"))
}

fn installed(scope: Scope, worktree: &Path) -> Option<u32> {
    let text = std::fs::read_to_string(file(scope, worktree)?).ok()?;
    Some(version_of(&text).unwrap_or(0))
}

pub fn status(worktree: &Path) -> Status {
    Status {
        repo: installed(Scope::Repo, worktree),
        user: installed(Scope::User, worktree),
    }
}

/// Writes this build's skill, over an older copy of ours — never over a file
/// someone else put there.
pub fn install(scope: Scope, worktree: &Path) -> Result<()> {
    let path = file(scope, worktree).context("no home directory to install the skill in")?;
    if let Ok(current) = std::fs::read_to_string(&path) {
        if version_of(&current).is_none() {
            bail!(
                "{} exists and is not Claudhub's: left alone",
                path.display()
            );
        }
    }
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("creating {}", parent.display()))?;
    }
    std::fs::write(&path, TEXT).with_context(|| format!("writing {}", path.display()))
}

/// Removes our skill, and its folder when nothing else is in it.
pub fn remove(scope: Scope, worktree: &Path) -> Result<()> {
    let Some(path) = file(scope, worktree) else {
        return Ok(());
    };
    match std::fs::read_to_string(&path) {
        Ok(current) if version_of(&current).is_some() => {
            std::fs::remove_file(&path).with_context(|| format!("removing {}", path.display()))?;
            if let Some(dir) = path.parent() {
                let _ = std::fs::remove_dir(dir);
            }
            Ok(())
        }
        Ok(_) => bail!("{} is not Claudhub's: left alone", path.display()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e).with_context(|| format!("reading {}", path.display())),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_shipped_skill_says_its_version_and_its_name() {
        assert!(version() >= 1);
        assert!(TEXT.starts_with("---\nname: claudhub\n"));
        assert_eq!(version_of("<!-- claudhub-skill-version: 12 -->"), Some(12));
        assert_eq!(version_of("# someone else's"), None);
    }

    #[test]
    fn installing_in_the_repository_writes_ours_and_leaves_others_alone() {
        let root = std::env::temp_dir().join(format!("claudhub-skill-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        assert_eq!(status(&root).repo, None);
        install(Scope::Repo, &root).unwrap();
        assert_eq!(status(&root).repo, Some(version()));
        remove(Scope::Repo, &root).unwrap();
        assert_eq!(status(&root).repo, None);
        assert!(!root.join(".claude/skills/claudhub").exists());

        // A skill of that name that is not ours: neither replaced nor removed.
        let theirs = root.join(".claude/skills/claudhub/SKILL.md");
        std::fs::create_dir_all(theirs.parent().unwrap()).unwrap();
        std::fs::write(&theirs, "---\nname: claudhub\n---\nmine").unwrap();
        assert_eq!(status(&root).repo, Some(0));
        assert!(install(Scope::Repo, &root).is_err());
        assert!(remove(Scope::Repo, &root).is_err());
        assert_eq!(
            std::fs::read_to_string(&theirs).unwrap(),
            "---\nname: claudhub\n---\nmine"
        );
        let _ = std::fs::remove_dir_all(&root);
    }
}
