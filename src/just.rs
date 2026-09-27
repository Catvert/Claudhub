//! What a project's `justfile` adds to Claudhub: its recipes, and a button
//! that runs one.
//!
//! Same bargain as `wt.toml`, one floor lower: a project declares its commands
//! in a file it already has, and Claudhub shows them **without knowing what
//! they do**. Where `wt.toml` is Claudhub's own extension point, a `justfile`
//! is the one most repositories already carry — this project's own `just ci`
//! among them — and reading it costs one subprocess per worktree.
//!
//! **The recipes come from `just` itself** (`--dump --dump-format json`), not
//! from a parser of ours. It is the `--porcelain` of this tool: a machine
//! format, and the only reading that stays right when a justfile uses
//! attributes, aliases, imports or `mod`. Re-implementing that grammar would
//! be re-implementing it wrongly, and the failures would be silent — a recipe
//! missing from a menu says nothing. The price is that `just` must be on the
//! `PATH`; without it there is no button, which is the honest answer, since
//! without it there is nothing to run either.
//!
//! **The justfile is found by us, and named to `just` explicitly.** Left to
//! search, `just` walks up to the parent directories: a worktree with no
//! justfile would have shown the recipes of the checkout above it, and run them
//! there.
//!
//! Nothing here may be called from the interface thread: it launches a process.
//! It goes through a worker, on the background queue.
//!
//! Only the top-level recipes are listed. A `mod` declares a submodule with a
//! namespace of its own (`just front::build`); nothing needs them yet, and the
//! dump carries them under `modules` when something does.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use anyhow::{Context, Result};

/// Beyond this, `just` is killed. Reading a justfile is instant; this ceiling
/// exists for the one that never answers — a `[[shell]]` backtick in an
/// assignment waiting on a network mount, the same case as git's timeout.
const TIMEOUT: Duration = Duration::from_secs(10);

/// What a worktree's justfile offers, cut down to what a menu shows.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Snapshot {
    /// Public recipes, in alphabetical order — `just --list`'s order, and the
    /// one the dump gives.
    pub recipes: Vec<Recipe>,
    /// The recipe a bare `just` runs: the first one declared. `None` when the
    /// justfile has no recipe at all, in which case the button is not painted.
    pub default: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Recipe {
    pub name: String,
    /// The doc comment `just --list` shows, which is the **last** comment line
    /// above the recipe. That is `just`'s own rule, and matching it is what
    /// makes the menu say what `just --list` says.
    pub doc: String,
    /// The parameters it declares, written as `just --list` writes them
    /// (`env="prod"`, `*extra`). Shown, never filled in: the button runs the
    /// recipe bare, and a recipe wanting an argument says so in the terminal,
    /// with its own message.
    pub params: Vec<String>,
}

impl Recipe {
    /// The recipe as one reads it in `just --list`: its name, then what it
    /// takes.
    pub fn signature(&self) -> String {
        if self.params.is_empty() {
            return self.name.clone();
        }
        format!("{} {}", self.name, self.params.join(" "))
    }
}

/// What a worktree's justfile declares, or `None`.
///
/// `None` is the ordinary case and never an error: most checkouts have no
/// justfile, and the button simply does not appear. A `just` that is missing or
/// that refuses the file — a syntax error while it is being written — is
/// journalled and answers the same way.
pub fn snapshot(worktree: &Path) -> Option<Snapshot> {
    let file = justfile_in(worktree)?;
    match dump(worktree, &file) {
        Ok(json) => match parse(&json) {
            Ok(snapshot) => Some(snapshot),
            Err(e) => {
                log::warn!("reading {}: {e:#}", file.display());
                None
            }
        },
        Err(e) => {
            // `debug` and not `warn`: a justfile being edited is invalid for a
            // few keystrokes at a time, and the watcher re-reads it on each
            // save. Warning there would fill the journal with what fixes
            // itself.
            log::debug!("just --dump in {}: {e:#}", worktree.display());
            None
        }
    }
}

/// The justfile of a directory, without searching the parents.
///
/// The names are `just`'s own, matched without case: `justfile` and
/// `.justfile`, which on a case-insensitive filesystem also covers `Justfile`
/// and `JUSTFILE`. The directory is read rather than four paths probed, so that
/// the same rule holds on every filesystem.
pub fn justfile_in(dir: &Path) -> Option<PathBuf> {
    let mut found: Option<PathBuf> = None;
    for entry in std::fs::read_dir(dir).ok()?.flatten() {
        let name = entry.file_name();
        let Some(name) = name.to_str() else { continue };
        if !is_justfile(name) {
            continue;
        }
        // `justfile` wins over `.justfile`, as it does for `just`, and the
        // reading order of a directory is not one to depend on.
        let path = entry.path();
        if !name.starts_with('.') {
            return Some(path);
        }
        found = Some(path);
    }
    found
}

/// Is this file name a justfile? The predicate the watcher asks too, which is
/// why it is here and pure: a justfile that changes has to reload the menu, and
/// that decision is made on a path, far from any directory listing.
pub fn is_justfile(name: &str) -> bool {
    let name = name.to_ascii_lowercase();
    name == "justfile" || name == ".justfile"
}

/// Asks `just` what the file declares.
///
/// `--working-directory` alongside `--justfile`: naming one without the other
/// makes `just` warn, and the pair is what pins both the file and the directory
/// its recipes would run in.
fn dump(dir: &Path, file: &Path) -> Result<String> {
    let mut cmd = Command::new("just");
    cmd.arg("--justfile")
        .arg(file)
        .arg("--working-directory")
        .arg(dir)
        .arg("--dump")
        .arg("--dump-format")
        .arg("json");
    // Closed stdin among the guards: an assignment evaluating a backtick
    // inherits it. English, because the complaint is quoted.
    crate::outside::Bounded::new(cmd, "just --dump", TIMEOUT)
        .in_english()
        .stdout()
}

/// Reads the dump: the public recipes, and the one a bare `just` runs.
///
/// Private recipes are dropped — those named with a leading underscore and
/// those carrying `[private]`, which the dump reports under the same flag —
/// exactly as `just --list` drops them: they are a justfile's internals, and
/// putting them in a menu would invite running them.
fn parse(json: &str) -> Result<Snapshot> {
    let root: serde_json::Value = serde_json::from_str(json).context("just's dump is not JSON")?;
    let recipes = root
        .get("recipes")
        .and_then(|value| value.as_object())
        .context("just's dump has no recipes")?;
    let recipes = recipes
        .values()
        .filter(|recipe| !crate::json::flag(recipe, "private"))
        .filter_map(|recipe| {
            Some(Recipe {
                name: crate::json::string(recipe, "name")?.to_string(),
                doc: crate::json::text(recipe, "doc"),
                params: crate::json::items(recipe, "parameters")
                    .iter()
                    .filter_map(parameter)
                    .collect(),
            })
        })
        .collect();
    Ok(Snapshot {
        recipes,
        // The first recipe declared, which is what `just` with no argument
        // runs. It may well be private — a justfile is free to start with an
        // underscore — and it stays the default all the same: it is what the
        // command does, and the button says what the command does.
        default: crate::json::string(&root, "first").map(str::to_string),
    })
}

/// One parameter, written as `just --list` writes it.
fn parameter(param: &serde_json::Value) -> Option<String> {
    let name = crate::json::string(param, "name")?;
    let prefix = match crate::json::string(param, "kind") {
        Some("star") => "*",
        Some("plus") => "+",
        _ => "",
    };
    Some(match crate::json::string(param, "default") {
        Some(default) => format!("{prefix}{name}=\"{default}\""),
        None => format!("{prefix}{name}"),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `just --dump --dump-format json` on a justfile carrying the four cases
    /// that matter: a default, a variadic, `[private]`, and a leading
    /// underscore. Trimmed of the fields nothing reads.
    const DUMP: &str = r#"{
      "first": "deploy",
      "aliases": { "d": { "name": "d", "target": "deploy" } },
      "recipes": {
        "_under":  { "doc": null, "name": "_under", "parameters": [], "private": true },
        "build":   { "doc": null, "name": "build",
                     "parameters": [ { "default": null, "kind": "singular", "name": "target" } ],
                     "private": false },
        "deploy":  { "doc": "to a place", "name": "deploy",
                     "parameters": [ { "default": "prod", "kind": "singular", "name": "env" },
                                     { "default": null, "kind": "star", "name": "extra" } ],
                     "private": false },
        "hidden":  { "doc": null, "name": "hidden", "parameters": [], "private": true }
      }
    }"#;

    #[test]
    fn the_dump_gives_the_public_recipes_and_the_default() {
        let snapshot = parse(DUMP).expect("the dump reads");
        let names: Vec<&str> = snapshot
            .recipes
            .iter()
            .map(|recipe| recipe.name.as_str())
            .collect();
        // `hidden` and `_under` are the justfile's internals, and `just --list`
        // does not show them either.
        assert_eq!(names, ["build", "deploy"]);
        assert_eq!(snapshot.default.as_deref(), Some("deploy"));
    }

    /// A recipe is written as `just --list` writes it: what it takes is part of
    /// what one reads before clicking.
    #[test]
    fn a_recipe_is_read_with_its_parameters() {
        let snapshot = parse(DUMP).expect("the dump reads");
        let deploy = &snapshot.recipes[1];
        assert_eq!(deploy.signature(), "deploy env=\"prod\" *extra");
        assert_eq!(deploy.doc, "to a place");
        assert_eq!(snapshot.recipes[0].signature(), "build target");
    }

    /// A justfile with nothing in it is not an error: the menu is empty and the
    /// button is not painted.
    #[test]
    fn an_empty_justfile_has_no_default() {
        let snapshot = parse(r#"{ "recipes": {}, "first": null }"#).expect("the dump reads");
        assert!(snapshot.recipes.is_empty());
        assert_eq!(snapshot.default, None);
    }

    #[test]
    fn what_is_not_json_is_an_error_and_not_an_empty_list() {
        assert!(parse("error: Justfile does not exist").is_err());
        assert!(parse("{}").is_err());
    }

    /// The name is matched without case, and only these two: a `justfile.md`
    /// saved next to it must not reload the menu.
    #[test]
    fn the_justfile_is_named_by_its_name() {
        assert!(is_justfile("justfile"));
        assert!(is_justfile("Justfile"));
        assert!(is_justfile("JUSTFILE"));
        assert!(is_justfile(".justfile"));
        assert!(!is_justfile("justfile.md"));
        assert!(!is_justfile("my.just"));
    }
}
