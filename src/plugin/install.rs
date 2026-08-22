//! Installing, updating and removing a plugin — that is, `git clone`, `git
//! pull`, and a directory that goes away.
//!
//! **The `git` binary and not a copy of an archive.** A plugin is a directory
//! of text files, which is exactly what git is for: the update is a `pull`, one
//! sees which revision one is on, and the author publishes by pushing. It is
//! the same reasoning that made Claudhub shell out to `git` rather than link
//! libgit2 — the user's credential helpers, their `includeIf`, their SSH keys
//! all work here without us knowing they exist.
//!
//! **No plugin registry, and that is a decision.** A registry means a server, a
//! namespace, a moderation policy and a trust model, to install what a URL
//! already names. `wt.toml`'s reasoning, one floor up: the cheapest level that
//! suffices.

use std::path::{Path, PathBuf};

use anyhow::{bail, Context as _, Result};

/// What one asks of a plugin's directory.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub enum Manage {
    /// Clone a repository into it.
    Install { url: String },
    /// Fast-forward it onto its upstream.
    Update,
    /// Remove it from the disk.
    Remove,
}

impl Manage {
    pub fn name(&self) -> &'static str {
        match self {
            Self::Install { .. } => "Install",
            Self::Update => "Update",
            Self::Remove => "Remove",
        }
    }

    /// Does it talk to a remote.
    ///
    /// Read off the operation the way a plugin's capability is read off its
    /// own: a clone and a pull are the network's business, seconds against a
    /// socket; removing a directory is milliseconds and belongs with the reads.
    pub fn is_network(&self) -> bool {
        matches!(self, Self::Install { .. } | Self::Update)
    }

    pub fn run(self, dir: &Path) -> Result<String> {
        match self {
            Self::Install { url } => install(dir, &url),
            Self::Update => update(dir),
            Self::Remove => remove(dir),
        }
    }
}

/// The directory name a repository URL suggests.
///
/// It is only a **suggestion**: the field stays editable, because two plugins
/// could well be published from repositories called `claudhub-plugin` by two
/// different people. What it strips is what nobody wants in a directory name —
/// the `.git` suffix, the trailing slash, the whole of the host and the path.
pub fn id_from_url(url: &str) -> Option<String> {
    let trimmed = url.trim().trim_end_matches('/');
    let last = trimmed.rsplit(['/', ':']).next()?;
    let last = last.strip_suffix(".git").unwrap_or(last);
    let cleaned: String = last
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '-'
            }
        })
        .collect();
    let cleaned = cleaned.trim_matches('-').to_ascii_lowercase();
    // A leading `claudhub-` says nothing here: everything in this directory is
    // a Claudhub plugin.
    let cleaned = cleaned
        .strip_prefix("claudhub-plugin-")
        .or_else(|| cleaned.strip_prefix("claudhub-"))
        .unwrap_or(&cleaned)
        .to_string();
    Some(cleaned).filter(|id| !id.is_empty())
}

fn install(dir: &Path, url: &str) -> Result<String> {
    if url.trim().is_empty() {
        bail!("no repository address");
    }
    if dir.exists() {
        bail!("{} already exists", dir.display());
    }
    let parent = dir.parent().context("a plugin directory has a parent")?;
    std::fs::create_dir_all(parent).with_context(|| format!("creating {}", parent.display()))?;
    // Cloned from the parent, with the target named: `git clone <url> <dir>`
    // run from inside a directory that does not exist yet would have nowhere to
    // start from.
    crate::git::git(parent, &["clone", url.trim(), &name_of(dir)?])?;
    // A repository that carries no manifest is not a plugin, and leaving it
    // there would be a directory nobody can explain. Removed at once, and the
    // reason said.
    if !dir.join(super::manifest::MANIFEST).is_file() {
        let _ = std::fs::remove_dir_all(dir);
        bail!("this repository carries no {}", super::manifest::MANIFEST);
    }
    Ok(revision(dir).unwrap_or_else(|| "installed".into()))
}

fn update(dir: &Path) -> Result<String> {
    if !dir.join(".git").exists() {
        bail!("{} was not installed from git", name_of(dir)?);
    }
    let before = revision(dir);
    // `--ff-only`: a plugin one has edited locally has commits of its own, and
    // a merge left half-done in a directory nobody thinks of as a repository is
    // a state impossible to get out of from here.
    crate::git::git(dir, &["pull", "--ff-only"])?;
    let after = revision(dir);
    Ok(match (before, after) {
        (Some(before), Some(after)) if before == after => "already up to date".into(),
        (_, Some(after)) => after,
        (_, None) => "updated".into(),
    })
}

fn remove(dir: &Path) -> Result<String> {
    if !dir.is_dir() {
        bail!("{} is not there", dir.display());
    }
    let name = name_of(dir)?;
    std::fs::remove_dir_all(dir).with_context(|| format!("removing {}", dir.display()))?;
    Ok(name)
}

/// The revision a plugin sits on, short and with its subject.
///
/// `None` for a plugin that was not installed from git — a directory dropped in
/// by hand is perfectly legitimate, and the page says so rather than showing an
/// error where a version would be.
pub fn revision(dir: &Path) -> Option<String> {
    if !dir.join(".git").exists() {
        return None;
    }
    crate::git::git_opt(dir, &["log", "-1", "--format=%h %s"]).map(|line| line.trim().to_string())
}

/// Where a plugin's directory goes, refusing anything that leaves the root.
///
/// A `..` in an id typed by hand would otherwise reach outside the plugin
/// directory, and what follows is a `git clone` or a `remove_dir_all`. It is
/// the guard `files::inside` puts on the explorer's operations, and it is
/// wanted here for the same reason.
pub fn dir_of(root: &Path, id: &str) -> Result<PathBuf> {
    let id = id.trim();
    if id.is_empty() {
        bail!("a plugin needs a name");
    }
    if id.contains('/') || id.contains('\\') || id.contains("..") {
        bail!("`{id}` is not a directory name");
    }
    Ok(root.join(id))
}

fn name_of(dir: &Path) -> Result<String> {
    dir.file_name()
        .and_then(|n| n.to_str())
        .map(str::to_string)
        .context("a plugin directory with an unreadable name")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_url_suggests_a_directory_name() {
        for (url, expected) in [
            ("https://github.com/someone/claudhub-ci.git", "ci"),
            ("https://github.com/someone/claudhub-plugin-jira", "jira"),
            ("git@github.com:someone/gitlab-runs.git", "gitlab-runs"),
            ("https://example.com/a/b/Weather/", "weather"),
        ] {
            assert_eq!(id_from_url(url).as_deref(), Some(expected), "{url}");
        }
        assert_eq!(id_from_url("   "), None);
    }

    /// The name typed by hand ends in a `git clone` and, one day, in a
    /// `remove_dir_all`. A `..` there must never reach outside the root.
    #[test]
    fn a_name_can_never_leave_the_plugin_directory() {
        let root = Path::new("/home/someone/.config/claudhub/plugins");
        assert_eq!(dir_of(root, "ci").expect("a plain name"), root.join("ci"));
        for bad in ["../../etc", "a/b", "..", "a\\b", ""] {
            assert!(dir_of(root, bad).is_err(), "{bad} should be refused");
        }
    }

    #[test]
    fn talking_to_a_remote_is_read_off_the_operation() {
        assert!(Manage::Install {
            url: "https://example.test/x.git".into()
        }
        .is_network());
        assert!(Manage::Update.is_network());
        assert!(!Manage::Remove.is_network());
    }

    /// A repository without a manifest is not a plugin, and what is cloned by
    /// mistake must not stay behind as a directory nobody can explain.
    #[test]
    fn a_clone_without_a_manifest_is_undone() {
        let root = std::env::temp_dir().join("claudhub-install-nomanifest");
        let _ = std::fs::remove_dir_all(&root);
        let source = root.join("source");
        std::fs::create_dir_all(&source).expect("mkdir");
        crate::git::git(&source, &["init", "-q"]).expect("git init");
        std::fs::write(source.join("README.md"), "pas un plugin").expect("write");
        crate::git::git(&source, &["add", "-A"]).expect("git add");
        crate::git::git(
            &source,
            &[
                "-c",
                "user.email=t@t",
                "-c",
                "user.name=t",
                "commit",
                "-qm",
                "x",
            ],
        )
        .expect("git commit");

        let target = root.join("plugins").join("nope");
        let message = install(&target, source.to_str().expect("utf-8"))
            .expect_err("no manifest, no plugin")
            .to_string();
        assert!(message.contains("plugin.toml"), "{message}");
        assert!(!target.exists(), "the clone must not stay behind");
    }
}
