//! Plugin marketplaces: git repositories whose top-level folders are
//! scripts, laid out as `<config>/scripts/` is — see `crate::scripts`.
//!
//! A marketplace is fetched by the **user's `git`**, on the network queue:
//! its credential helpers and SSH keys are what reach a private repository,
//! and on Windows the worker in WSL clones it as it fetches everything else.
//! What comes back over the wire is the scripts' files as text, never a
//! path: the interface writes them under its own configuration.
//!
//! Three places, none of them run before the user says so:
//!
//! - the **catalog**, `<config>/plugin-markets/<slug>/`: the last fetch of a
//!   marketplace, whole, rewritten by each fetch that brings a new commit;
//! - the **installed** copies, `<config>/scripts-market/<id>/`, each pinned
//!   to the commit it came from (`INSTALLED`), and brought up to the
//!   catalog's only by a gesture;
//! - nothing else: the catalog is read, never mounted.
//!
//! **An installed plugin's id is `<owner>.<repo>.<folder>`**, lower-cased.
//! Its data, secrets and settings are filed by that id, so a plugin can
//! never take the place of a builtin — `sentry` would inherit its token —
//! nor of another owner's: a GitHub owner has no `.` in its name.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::scripts::{self, MANIFEST};

/// A marketplace as the user names it, read.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Source {
    /// What `git` clones: the address as given, or GitHub's for `owner/repo`.
    pub url: String,
    /// The branch fetched; the repository's default one when `None`.
    pub branch: Option<String>,
    pub owner: String,
    pub repo: String,
}

impl Source {
    /// The marketplace's name on disk, and the head of its plugins' ids.
    pub fn slug(&self) -> String {
        format!("{}.{}", self.owner, self.repo).to_lowercase()
    }
}

/// Reads a marketplace as the user names it: `owner/repo`, a GitHub address
/// over HTTPS or SSH, each with an optional `#branch`.
pub fn parse_source(text: &str) -> Result<Source, String> {
    let text = text.trim();
    let (address, branch) = match text.split_once('#') {
        Some((address, branch)) => (address.trim(), Some(branch.trim())),
        None => (text, None),
    };
    if branch.is_some_and(|branch| {
        branch.is_empty() || branch.starts_with('-') || branch.contains(char::is_whitespace)
    }) {
        return Err(format!("`{text}`: the branch after `#` is not one"));
    }
    let path = [
        "https://github.com/",
        "ssh://git@github.com/",
        "git@github.com:",
    ]
    .iter()
    .find_map(|prefix| address.strip_prefix(prefix))
    .unwrap_or(address);
    let path = path.trim_end_matches('/');
    let path = path.strip_suffix(".git").unwrap_or(path);
    let mut parts = path.split('/');
    let (Some(owner), Some(repo), None) = (parts.next(), parts.next(), parts.next()) else {
        return Err(format!(
            "`{text}` is not a GitHub repository: `owner/repo`, or its address"
        ));
    };
    let owner_ok = !owner.is_empty()
        && !owner.starts_with('-')
        && owner.chars().all(|c| c.is_ascii_alphanumeric() || c == '-');
    let repo_ok = !repo.is_empty()
        && !repo.starts_with('.')
        && repo
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'));
    if !owner_ok || !repo_ok {
        return Err(format!(
            "`{text}` is not a GitHub repository: `owner/repo`, or its address"
        ));
    }
    let url = if address.contains(':') {
        address.to_string()
    } else {
        format!("https://github.com/{owner}/{repo}.git")
    };
    Ok(Source {
        url,
        branch: branch.map(str::to_string),
        owner: owner.to_string(),
        repo: repo.to_string(),
    })
}

/// The id a plugin of marketplace `slug`, in folder `folder`, is installed
/// under.
pub fn plugin_id(slug: &str, folder: &str) -> String {
    format!("{slug}.{}", folder.to_lowercase())
}

/// A marketplace's scripts at one commit: `<folder>/<path>` and the text.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Snapshot {
    pub commit: String,
    pub files: Vec<(String, String)>,
}

/// What a fetch found.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Fetched {
    /// The commit the catalog has is still the branch's.
    Unchanged,
    Snapshot(Snapshot),
    /// Why it could not be read — in git's words, often an authentication.
    Failed(String),
}

/// Past these, a marketplace is refused rather than cut: a plugin with half
/// its files would load as something else.
const MAX_FILE: u64 = 1 << 20;
const MAX_TOTAL: u64 = 16 << 20;
const MAX_FILES: usize = 4000;
const MAX_DEPTH: usize = 8;

/// The scripts of a checkout: each top-level folder holding a manifest, its
/// files but the hidden ones and `node_modules`. A file that is not UTF-8
/// text is passed by — a script is JavaScript and JSON.
pub fn read_snapshot(dir: &Path) -> Result<Vec<(String, String)>, String> {
    let mut files = Vec::new();
    let mut total = 0u64;
    let entries = std::fs::read_dir(dir).map_err(|error| error.to_string())?;
    let mut folders: Vec<String> = entries
        .flatten()
        .filter(|entry| entry.file_type().is_ok_and(|kind| kind.is_dir()))
        .filter_map(|entry| entry.file_name().into_string().ok())
        .filter(|name| scripts::valid_id(name) && dir.join(name).join(MANIFEST).is_file())
        .collect();
    folders.sort();
    for folder in folders {
        let mut pending = vec![(PathBuf::from(&folder), 0usize)];
        while let Some((relative, depth)) = pending.pop() {
            let entries = std::fs::read_dir(dir.join(&relative)).map_err(|e| e.to_string())?;
            for entry in entries.flatten() {
                let Ok(name) = entry.file_name().into_string() else {
                    continue;
                };
                if name.starts_with('.') || name == "node_modules" {
                    continue;
                }
                let Ok(kind) = entry.file_type() else {
                    continue;
                };
                let path = relative.join(&name);
                if kind.is_dir() {
                    if depth < MAX_DEPTH {
                        pending.push((path, depth + 1));
                    }
                    continue;
                }
                if !kind.is_file() {
                    continue;
                }
                let size = entry.metadata().map(|meta| meta.len()).unwrap_or(0);
                if size > MAX_FILE {
                    return Err(format!("{} is over a megabyte", path.display()));
                }
                let Ok(text) = std::fs::read_to_string(entry.path()) else {
                    continue;
                };
                total += size;
                if total > MAX_TOTAL || files.len() >= MAX_FILES {
                    return Err(format!(
                        "the marketplace is over {MAX_FILES} files or 16 megabytes"
                    ));
                }
                let path = path.to_string_lossy().replace('\\', "/");
                files.push((path, text));
            }
        }
    }
    files.sort();
    Ok(files)
}

/// Fetches marketplace `url` — its `branch`, or its default one — unless
/// its head is still `known`. The worker's half; never the interface's.
pub fn fetch(url: &str, branch: Option<&str>, known: Option<&str>) -> Fetched {
    match fetch_or_fail(url, branch, known) {
        Ok(fetched) => fetched,
        Err(error) => Fetched::Failed(format!("{error:#}")),
    }
}

fn fetch_or_fail(url: &str, branch: Option<&str>, known: Option<&str>) -> anyhow::Result<Fetched> {
    let scratch = std::env::temp_dir();
    let head = crate::git::git(
        &scratch,
        &["ls-remote", "--", url, branch.unwrap_or("HEAD")],
    )?;
    let head = head.split_whitespace().next().unwrap_or_default();
    if head.is_empty() {
        anyhow::bail!("{url} has no branch {}", branch.unwrap_or("HEAD"));
    }
    if known == Some(head) {
        return Ok(Fetched::Unchanged);
    }
    let into = scratch.join(format!(
        "claudhub-market-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |since| since.as_nanos())
    ));
    let into_text = into.to_string_lossy().into_owned();
    let mut args = vec!["clone", "--quiet", "--depth", "1", "--no-tags"];
    if let Some(branch) = branch {
        args.extend(["--branch", branch]);
    }
    args.extend(["--", url, into_text.as_str()]);
    let read = crate::git::git(&scratch, &args).and_then(|_| {
        let commit = crate::git::git(&into, &["rev-parse", "HEAD"])?;
        let files = read_snapshot(&into).map_err(anyhow::Error::msg)?;
        Ok(Snapshot { commit, files })
    });
    let _ = std::fs::remove_dir_all(&into);
    Ok(Fetched::Snapshot(read?))
}

/// The file, at a catalog's root, saying where it comes from.
pub const MARKET: &str = ".market.json";

/// The file, in an installed plugin's folder, saying where it came from.
pub const INSTALLED: &str = ".installed.json";

/// What a catalog says of itself.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct CatalogMeta {
    /// The source as the user wrote it.
    pub source: String,
    pub commit: String,
    /// When it was fetched, in seconds since the epoch.
    pub fetched: u64,
}

/// What an installed plugin says of where it came from.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Provenance {
    /// The marketplace's slug, and the plugin's folder in it.
    pub market: String,
    pub folder: String,
    pub commit: String,
    /// The fingerprint of the files installed: what says the catalog holds
    /// another version of them.
    pub print: String,
}

/// Rewrites the catalog of marketplace `slug` under `root` with `snapshot`:
/// written beside, then put in the old one's place.
pub fn write_catalog(
    root: &Path,
    slug: &str,
    source: &str,
    snapshot: &Snapshot,
    fetched: u64,
) -> std::io::Result<()> {
    let target = root.join(slug);
    let fresh = root.join(format!(".{slug}.new"));
    if fresh.exists() {
        std::fs::remove_dir_all(&fresh)?;
    }
    std::fs::create_dir_all(&fresh)?;
    for (path, text) in &snapshot.files {
        if !inside(path) {
            continue;
        }
        let file = fresh.join(path);
        if let Some(parent) = file.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(file, text)?;
    }
    let meta = CatalogMeta {
        source: source.to_string(),
        commit: snapshot.commit.clone(),
        fetched,
    };
    std::fs::write(
        fresh.join(MARKET),
        serde_json::to_string_pretty(&meta).unwrap_or_default(),
    )?;
    if target.exists() {
        std::fs::remove_dir_all(&target)?;
    }
    std::fs::rename(fresh, target)
}

/// A relative path with nothing but names: what the wire brings is written
/// nowhere else than where it says.
fn inside(path: &str) -> bool {
    let path = Path::new(path);
    !path.as_os_str().is_empty()
        && path
            .components()
            .all(|part| matches!(part, std::path::Component::Normal(_)))
}

/// What a catalog says of itself, if it reads.
pub fn catalog_meta(dir: &Path) -> Option<CatalogMeta> {
    let text = std::fs::read_to_string(dir.join(MARKET)).ok()?;
    serde_json::from_str(&text).ok()
}

/// Where an installed plugin came from, if it says.
pub fn provenance(dir: &Path) -> Option<Provenance> {
    let text = std::fs::read_to_string(dir.join(INSTALLED)).ok()?;
    serde_json::from_str(&text).ok()
}

/// The fingerprint of a plugin's folder: its files but the hidden ones and
/// what the runtime writes, by path.
pub fn print_of(dir: &Path) -> String {
    let mut files: Vec<(String, String)> = Vec::new();
    let mut pending = vec![(PathBuf::new(), 0usize)];
    while let Some((relative, depth)) = pending.pop() {
        let Ok(entries) = std::fs::read_dir(dir.join(&relative)) else {
            continue;
        };
        for entry in entries.flatten() {
            let Ok(name) = entry.file_name().into_string() else {
                continue;
            };
            if name.starts_with('.')
                || name == "node_modules"
                || (depth == 0 && scripts::GENERATED.contains(&name.as_str()))
            {
                continue;
            }
            let path = relative.join(&name);
            match entry.file_type() {
                Ok(kind) if kind.is_dir() && depth < MAX_DEPTH => {
                    pending.push((path, depth + 1));
                }
                Ok(kind) if kind.is_file() => {
                    if let Ok(text) = std::fs::read_to_string(entry.path()) {
                        files.push((path.to_string_lossy().replace('\\', "/"), text));
                    }
                }
                _ => {}
            }
        }
    }
    files.sort();
    scripts::fingerprint(
        files
            .iter()
            .map(|(path, text)| (path.as_str(), Some(text.as_str()))),
    )
}

/// Installs — or brings up to date — plugin `folder` of the catalog `from`
/// (`<plugin-markets>/<slug>`) as `id` under `root`, pinned to `commit`.
/// Written beside, then put in the place of the version there.
pub fn install(
    from: &Path,
    slug: &str,
    folder: &str,
    commit: &str,
    root: &Path,
    id: &str,
) -> std::io::Result<PathBuf> {
    let source = from.join(folder);
    if !source.join(MANIFEST).is_file() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::NotFound,
            format!("`{folder}` is no longer in the marketplace"),
        ));
    }
    let target = root.join(id);
    let fresh = root.join(format!(".{id}.new"));
    if fresh.exists() {
        std::fs::remove_dir_all(&fresh)?;
    }
    copy_dir(&source, &fresh, 0)?;
    let provenance = Provenance {
        market: slug.to_string(),
        folder: folder.to_string(),
        commit: commit.to_string(),
        print: print_of(&fresh),
    };
    std::fs::write(
        fresh.join(INSTALLED),
        serde_json::to_string_pretty(&provenance).unwrap_or_default(),
    )?;
    if target.exists() {
        std::fs::remove_dir_all(&target)?;
    }
    std::fs::rename(&fresh, &target)?;
    Ok(target)
}

/// Copies a folder, the hidden entries left behind.
pub fn copy_dir(from: &Path, to: &Path, depth: usize) -> std::io::Result<()> {
    std::fs::create_dir_all(to)?;
    for entry in std::fs::read_dir(from)?.flatten() {
        let name = entry.file_name();
        if name.to_string_lossy().starts_with('.') {
            continue;
        }
        let kind = entry.file_type()?;
        if kind.is_dir() && depth < MAX_DEPTH {
            copy_dir(&entry.path(), &to.join(&name), depth + 1)?;
        } else if kind.is_file() {
            std::fs::copy(entry.path(), to.join(&name))?;
        }
    }
    Ok(())
}

/// The manifest fields a catalog entry shows before it is installed, read
/// as `scripts::parse` reads them — what it would refuse is refused here.
pub fn read_entry(dir: &Path, folder: &str, language: &str) -> Result<scripts::Script, String> {
    let manifest = std::fs::read_to_string(dir.join(folder).join(MANIFEST))
        .map_err(|error| format!("{MANIFEST}: {error}"))?;
    scripts::parse(folder, &dir.join(folder), &manifest, language)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_source_is_read_from_each_way_of_naming_it() {
        let shorthand = parse_source(" Acme/Claudhub-Plugins ").unwrap();
        assert_eq!(
            shorthand.url,
            "https://github.com/Acme/Claudhub-Plugins.git"
        );
        assert_eq!(shorthand.branch, None);
        assert_eq!(shorthand.slug(), "acme.claudhub-plugins");

        let https = parse_source("https://github.com/acme/plugins.git#dev").unwrap();
        assert_eq!(https.url, "https://github.com/acme/plugins.git");
        assert_eq!(https.branch.as_deref(), Some("dev"));

        let ssh = parse_source("git@github.com:acme/my.plugins.git").unwrap();
        assert_eq!(ssh.url, "git@github.com:acme/my.plugins.git");
        assert_eq!(ssh.slug(), "acme.my.plugins");
        assert_eq!(
            parse_source("https://github.com/acme/plugins/")
                .unwrap()
                .slug(),
            "acme.plugins"
        );
    }

    #[test]
    fn what_is_no_repository_is_refused() {
        for text in [
            "",
            "acme",
            "acme/plugins/extra",
            "a.b/plugins",
            "-acme/plugins",
            "acme/.hidden",
            "acme/plugins#",
            "acme/plugins#--upload-pack=x",
            "https://gitlab.com/acme/plugins",
        ] {
            assert!(parse_source(text).is_err(), "{text}");
        }
    }

    /// No owner has a `.`: a plugin of one owner never wears the id of
    /// another's, nor of a builtin.
    #[test]
    fn an_installed_id_carries_its_owner_and_repository() {
        let source = parse_source("acme/plugins").unwrap();
        assert_eq!(plugin_id(&source.slug(), "Jira"), "acme.plugins.jira");
        assert!(scripts::valid_id(&plugin_id(&source.slug(), "jira")));
    }

    fn sh(root: &Path, args: &[&str]) -> String {
        let out = std::process::Command::new("git")
            .arg("-C")
            .arg(root)
            .args(args)
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "git {args:?}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8_lossy(&out.stdout).trim().to_string()
    }

    /// A real repository as a marketplace: fetched by the user's `git`,
    /// written as a catalog, installed, and an update seen by fingerprint.
    #[test]
    fn a_marketplace_is_fetched_written_and_installed() {
        let root =
            std::env::temp_dir().join(format!("claudhub-market-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let repo = root.join("repo");
        for (path, text) in [
            (
                "jira/claudhub.json",
                r#"{"title": "Jira", "version": "1.0.0"}"#,
            ),
            ("jira/main.js", "export default 1;"),
            ("jira/lib/api.js", "export const api = 1;"),
            ("jira/node_modules/x.js", "nope"),
            ("docs/README.md", "not a plugin: no manifest"),
            (".github/ci.yml", "hidden"),
        ] {
            std::fs::create_dir_all(repo.join(path).parent().unwrap()).unwrap();
            std::fs::write(repo.join(path), text).unwrap();
        }
        sh(&repo, &["init", "-q", "-b", "main"]);
        sh(&repo, &["config", "user.email", "t@example.com"]);
        sh(&repo, &["config", "user.name", "T"]);
        sh(&repo, &["add", "-A"]);
        sh(&repo, &["commit", "-q", "-m", "one"]);
        let head = sh(&repo, &["rev-parse", "HEAD"]);
        let url = repo.to_string_lossy().into_owned();

        let Fetched::Snapshot(snapshot) = fetch(&url, None, None) else {
            panic!("no snapshot");
        };
        assert_eq!(snapshot.commit, head);
        let paths: Vec<&str> = snapshot
            .files
            .iter()
            .map(|(path, _)| path.as_str())
            .collect();
        assert_eq!(
            paths,
            ["jira/claudhub.json", "jira/lib/api.js", "jira/main.js"]
        );
        assert_eq!(fetch(&url, None, Some(&head)), Fetched::Unchanged);
        assert_eq!(fetch(&url, Some("main"), Some(&head)), Fetched::Unchanged);
        assert!(matches!(
            fetch(&url, Some("nowhere"), None),
            Fetched::Failed(_)
        ));

        let catalogs = root.join("plugin-markets");
        write_catalog(&catalogs, "acme.tools", "acme/tools", &snapshot, 7).unwrap();
        let catalog = catalogs.join("acme.tools");
        assert_eq!(catalog_meta(&catalog).unwrap().commit, head);
        let entry = read_entry(&catalog, "jira", "en").unwrap();
        assert_eq!(entry.version.as_deref(), Some("1.0.0"));

        let installed = root.join("scripts-market");
        let id = plugin_id("acme.tools", "jira");
        let dir = install(&catalog, "acme.tools", "jira", &head, &installed, &id).unwrap();
        assert!(dir.join("lib/api.js").is_file());
        let pinned = provenance(&dir).unwrap();
        assert_eq!(pinned.commit, head);
        assert_eq!(pinned.print, print_of(&catalog.join("jira")));
        // What the runtime writes beside the entry is no change of version.
        std::fs::write(dir.join("gpui-kit.d.ts"), "// generated").unwrap();
        assert_eq!(print_of(&dir), pinned.print);

        // A new commit: the catalog holds another version, the installed
        // one stays where it was.
        std::fs::write(repo.join("jira/main.js"), "export default 2;").unwrap();
        sh(&repo, &["commit", "-q", "-am", "two"]);
        let Fetched::Snapshot(newer) = fetch(&url, None, Some(&head)) else {
            panic!("no newer snapshot");
        };
        write_catalog(&catalogs, "acme.tools", "acme/tools", &newer, 8).unwrap();
        assert_ne!(print_of(&catalog.join("jira")), pinned.print);
        assert_eq!(
            std::fs::read_to_string(dir.join("main.js")).unwrap(),
            "export default 1;"
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn only_plain_relative_paths_are_written() {
        assert!(inside("jira/main.js"));
        for path in ["", "../x", "/etc/x", "jira/../../x"] {
            assert!(!inside(path), "{path}");
        }
    }
}
