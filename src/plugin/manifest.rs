//! What a plugin declares about itself, and how it is found.
//!
//! A plugin is a **directory**: a `plugin.toml` that describes it, a `main.rn`
//! that is its code. A directory and not a single file because a plugin will
//! want its own strings beside its code — it cannot go through `tr!`, whose
//! catalogues are compiled and whose keys a test compares.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use anyhow::{bail, Context as _, Result};
use serde::Deserialize;

/// The file that makes a directory a plugin.
pub const MANIFEST: &str = "plugin.toml";

/// What every plugin's panel name begins with.
///
/// It is what lets a saved layout be read without the plugin: a name under this
/// prefix that no manifest claims is a panel nobody can build any more, and it
/// is pruned rather than left to come back as an empty frame.
pub const PANEL_PREFIX: &str = "ClaudhubPlugin:";

/// The screens a plugin may put its panel on.
///
/// Named by the same keys the layout is saved under (`workspace::key`), so a
/// manifest and a `layout.json` speak the same language — which is also why the
/// first is still `review` after its screen was renamed "Git". The settings are
/// not among them: that screen holds the form and nothing else.
pub const SCREENS: [&str; 5] = ["review", "files", "search", "db", "sentry"];

/// What a plugin says it needs.
///
/// Declared and not inferred, so that reading a `plugin.toml` is enough to know
/// what it may reach — and so an unknown name is refused at load rather than
/// discovered when a capability first fails.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Capability {
    Http,
    Shell,
}

impl Capability {
    pub fn name(self) -> &'static str {
        match self {
            Self::Http => "http",
            Self::Shell => "shell",
        }
    }
}

/// A plugin's declaration, as read from disk.
#[derive(Debug, Clone, Deserialize)]
pub struct Declaration {
    /// The name shown on the tab. Not a key: a plugin's strings are its own.
    pub title: String,
    /// The screen whose dock carries the panel.
    #[serde(default = "default_screen")]
    pub screen: String,
    /// The tab's icon, a Lucide name.
    #[serde(default)]
    pub icon: Option<String>,
    #[serde(default)]
    pub capabilities: Vec<Capability>,
    /// Free-form settings the script reads with `setting(name)`. The plugin
    /// decides what they mean; Claudhub only carries them.
    #[serde(default)]
    pub settings: BTreeMap<String, String>,
    /// The names of the secrets the script may ask for. The **values** live in
    /// the settings file, never here: a manifest is a file one copies around.
    #[serde(default)]
    pub secrets: Vec<String>,
    /// The settings and secrets without which the plugin cannot work.
    ///
    /// Declared and not guessed, for the same reason the capabilities are: only
    /// the author knows that a Sentry plugin without an organisation and a
    /// token has nothing to show. A plugin missing one of these is **not
    /// switched on** — its panel is not there, and neither is the screen that
    /// carried only it. Otherwise one lands on an empty view and has to work
    /// out that the fault is a field left blank in another page.
    ///
    /// Names, matched against both settings and secrets: from here they are the
    /// same thing, something the user has to say.
    #[serde(default)]
    pub required: Vec<String>,
}

fn default_screen() -> String {
    "review".into()
}

/// A plugin found on disk: its declaration, plus what only discovery knows.
#[derive(Debug, Clone)]
pub struct Manifest {
    /// The directory's name. It keys everything: the panel, the settings, the
    /// state.
    pub id: String,
    pub dir: PathBuf,
    pub declaration: Declaration,
    /// The panel's name in the dock's registry, leaked once.
    ///
    /// `BasePanel::panel_name` returns a `&'static str` and a plugin's id is
    /// only known at run time. Leaking is what bridges the two, and it is safe
    /// here for one reason worth stating: discovery happens once per session,
    /// over a bounded directory, and these strings live as long as the window
    /// does anyway.
    pub panel: &'static str,
}

impl Manifest {
    pub fn title(&self) -> &str {
        &self.declaration.title
    }

    pub fn icon(&self) -> &str {
        self.declaration.icon.as_deref().unwrap_or("puzzle")
    }

    /// The script's path.
    pub fn entry(&self) -> PathBuf {
        self.dir.join("main.rn")
    }

    pub fn allows(&self, capability: Capability) -> bool {
        self.declaration.capabilities.contains(&capability)
    }

    /// What the user still has to say before this plugin can work.
    ///
    /// `have` is what is actually set — the manifest's defaults laid over by
    /// the settings, secrets included — and a value that is only whitespace
    /// counts as unsaid, a field one has cleared being a field one has emptied.
    pub fn missing<'a>(&'a self, have: &dyn Fn(&str) -> Option<String>) -> Vec<&'a str> {
        self.declaration
            .required
            .iter()
            .filter(|name| {
                have(name)
                    .map(|value| value.trim().is_empty())
                    .unwrap_or(true)
            })
            .map(String::as_str)
            .collect()
    }
}

/// Reads one directory. `Ok(None)` when it simply is not a plugin.
pub fn read(dir: &Path) -> Result<Option<Manifest>> {
    let file = dir.join(MANIFEST);
    if !file.is_file() {
        return Ok(None);
    }
    let id = dir
        .file_name()
        .and_then(|name| name.to_str())
        .context("a plugin directory with an unreadable name")?
        .to_string();
    let text =
        std::fs::read_to_string(&file).with_context(|| format!("reading {}", file.display()))?;
    let declaration: Declaration =
        toml::from_str(&text).with_context(|| format!("reading {}", file.display()))?;
    if !SCREENS.contains(&declaration.screen.as_str()) {
        bail!(
            "{id}: unknown screen `{}` (expected one of {})",
            declaration.screen,
            SCREENS.join(", ")
        );
    }
    if !dir.join("main.rn").is_file() {
        bail!("{id}: no main.rn beside its {MANIFEST}");
    }
    let panel: &'static str = format!("{PANEL_PREFIX}{id}").leak();
    Ok(Some(Manifest {
        id,
        dir: dir.to_path_buf(),
        declaration,
        panel,
    }))
}

/// Every plugin under `root`, in a stable order.
///
/// Sorted by id: the panels are registered in this order and appear in the
/// menus in it, and a directory listing's order is the filesystem's, which
/// changes for no reason anyone can see.
///
/// A directory that does not read is **skipped and logged**, never fatal: one
/// broken plugin must not take the four others with it.
pub fn discover(root: &Path) -> Vec<Manifest> {
    let Ok(entries) = std::fs::read_dir(root) else {
        return Vec::new();
    };
    let mut found = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }
        match read(&path) {
            Ok(Some(manifest)) => found.push(manifest),
            Ok(None) => {}
            Err(e) => log::warn!("plugin in {}: {e:#}", path.display()),
        }
    }
    found.sort_by(|a, b| a.id.cmp(&b.id));
    found
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write(dir: &Path, name: &str, manifest: &str, script: Option<&str>) {
        let dir = dir.join(name);
        std::fs::create_dir_all(&dir).expect("mkdir");
        std::fs::write(dir.join(MANIFEST), manifest).expect("write manifest");
        if let Some(script) = script {
            std::fs::write(dir.join("main.rn"), script).expect("write script");
        }
    }

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("claudhub-plugin-{name}"));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("mkdir");
        dir
    }

    /// A plugin says what it cannot work without, and a blank counts as unsaid.
    #[test]
    fn a_plugin_says_what_it_still_needs() {
        let root = scratch("required");
        write(
            &root,
            "sentry",
            "title = \"Sentry\"\ncapabilities = [\"http\"]\nsecrets = [\"token\"]\nrequired = [\"org\", \"token\"]\n[settings]\norg = \"\"\n",
            Some("pub fn view(s){}"),
        );
        let found = discover(&root);
        let manifest = &found[0];

        // Nothing said yet: both are missing, in the order the manifest lists
        // them — what one reads is a checklist, not a set.
        assert_eq!(manifest.missing(&|_| None), vec!["org", "token"]);
        // A field cleared is a field emptied.
        assert_eq!(
            manifest.missing(&|name| Some(if name == "org" {
                "  ".into()
            } else {
                "t".into()
            })),
            vec!["org"]
        );
        assert!(manifest
            .missing(&|name| Some(format!("{name}-value")))
            .is_empty());
    }

    /// A manifest that requires nothing is configured from the start: that is
    /// the CI plugin's case, and most plugins'.
    #[test]
    fn a_plugin_that_asks_for_nothing_is_ready() {
        let root = scratch("nothing-required");
        write(&root, "ci", "title = \"CI\"\n", Some("pub fn view(s){}"));
        assert!(discover(&root)[0].missing(&|_| None).is_empty());
    }

    #[test]
    fn a_manifest_names_its_panel_after_its_directory() {
        let root = scratch("named");
        write(
            &root,
            "ci",
            "title = \"CI\"\nscreen = \"review\"\ncapabilities = [\"shell\"]\n",
            Some("pub fn view(s) { }"),
        );
        let found = discover(&root);
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].id, "ci");
        assert_eq!(found[0].panel, "ClaudhubPlugin:ci");
        assert!(found[0].allows(Capability::Shell));
        assert!(!found[0].allows(Capability::Http));
    }

    /// One broken plugin must not take the others with it: a directory that
    /// does not read is skipped, and the window still opens.
    #[test]
    fn a_broken_plugin_does_not_hide_a_working_one() {
        let root = scratch("broken");
        write(
            &root,
            "aaa",
            "this is not toml at all {{{",
            Some("pub fn view(s){}"),
        );
        // A screen that does not exist: the panel would be registered against a
        // dock nobody builds, and the plugin would simply never appear.
        write(
            &root,
            "bbb",
            "title = \"X\"\nscreen = \"nowhere\"\n",
            Some("pub fn view(s){}"),
        );
        // A manifest with no script beside it.
        write(&root, "ccc", "title = \"Y\"\n", None);
        write(&root, "ddd", "title = \"Good\"\n", Some("pub fn view(s){}"));
        let found = discover(&root);
        assert_eq!(found.len(), 1, "{found:?}");
        assert_eq!(found[0].id, "ddd");
        // The default screen, when the manifest says nothing.
        assert_eq!(found[0].declaration.screen, "review");
    }

    #[test]
    fn discovery_is_sorted_and_not_the_filesystems_order() {
        let root = scratch("sorted");
        for id in ["zeta", "alpha", "mu"] {
            write(&root, id, "title = \"T\"\n", Some("pub fn view(s){}"));
        }
        let ids: Vec<_> = discover(&root).into_iter().map(|m| m.id).collect();
        assert_eq!(ids, vec!["alpha", "mu", "zeta"]);
    }
}
