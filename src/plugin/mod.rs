//! Plugins: a panel whose content is a script, reloaded while the window runs.
//!
//! The reason this exists is a count. Sentry, undressed, holds almost nothing
//! that speaks of Sentry: fetch JSON behind a token, remember one setting per
//! repository, paint a master/detail list with a code excerpt, compose a prompt
//! and hand it to the agent. Those are five generic capabilities, and they are
//! exactly what a GitHub issue list, a CI board or a log stream would ask for.
//!
//! The split is the repository's own, pushed one notch: `notes.rs` in front of
//! `notes_view.rs`, `sql_history.rs` in front of `sql_history_view.rs`. A
//! script produces a **tree of data** ([`view::Node`]); `ui::plugin_view`
//! paints it. Nothing here knows gpui.
//!
//! Three layers, and each is where it is for a reason:
//!
//! - [`view`] and [`manifest`] are plain data. No gpui, no Rune.
//! - [`caps`] is what a plugin may do to the outside world. Data too, executed
//!   by a worker — which may be a worker in the WSL server. **That is why it
//!   carries no Rune**: the headless binary must be able to run a plugin's
//!   requests without a scripting engine in it.
//! - [`host`] is the Rune machine, behind the `plugins` feature, which `ui`
//!   turns on. The script runs on the interface's side; only its input and
//!   output cross the wire.

use std::path::{Path, PathBuf};

pub mod caps;
pub mod install;
pub mod manifest;
pub mod view;

#[cfg(feature = "plugins")]
pub mod host;

#[cfg(feature = "plugins")]
mod loaded;
#[cfg(feature = "plugins")]
pub use loaded::{Plugin, SHOW_AFTER};

/// A path a script named, read as a path **inside the worktree**.
///
/// Everywhere else in Claudhub a file is called by its path within the
/// worktree, and each command joins the root at the last moment. A script has
/// no reason to know that, and what it hands over comes from wherever it
/// fetched: Sentry reports a frame as `/vendor/laravel/framework/…`, meaning
/// "under the deployed application's root" and not "at the root of this
/// machine". Joining that as it stands **replaces** the worktree — `Path::join`
/// with an absolute path drops what it was joined to — and the reader answers
/// `No such file or directory` about a path nobody wrote.
///
/// So the leading separator is dropped, and a `..` is **refused**: a plugin
/// reaching outside the worktree is a plugin doing what its manifest does not
/// say, and there is no reading of it that is what its author meant. `None`
/// also for a path that says nothing at all.
pub fn in_worktree(said: &str) -> Option<PathBuf> {
    use std::path::Component;
    let mut out = PathBuf::new();
    for component in Path::new(said.trim()).components() {
        match component {
            Component::Normal(part) => out.push(part),
            // A root or a `.` says where to start, and here that is settled.
            Component::RootDir | Component::CurDir => {}
            Component::ParentDir | Component::Prefix(_) => return None,
        }
    }
    (!out.as_os_str().is_empty()).then_some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// What Sentry hands over, and what nobody must be able to hand over.
    #[test]
    fn a_path_a_script_named_lands_inside_the_worktree() {
        // The case that sent the panel looking at the root of the machine.
        assert_eq!(
            in_worktree("/vendor/laravel/framework/src/Illuminate/Pipeline/Pipeline.php"),
            Some(PathBuf::from(
                "vendor/laravel/framework/src/Illuminate/Pipeline/Pipeline.php"
            ))
        );
        assert_eq!(
            in_worktree("app/Models/User.php"),
            Some(PathBuf::from("app/Models/User.php"))
        );
        assert_eq!(
            in_worktree("  ./app/Http/Kernel.php  "),
            Some(PathBuf::from("app/Http/Kernel.php"))
        );
        // Nothing said is nothing to open, rather than the worktree itself.
        assert_eq!(in_worktree(""), None);
        assert_eq!(in_worktree("/"), None);
        // And there is no reading of these that is what the author meant.
        assert_eq!(in_worktree("../../etc/passwd"), None);
        assert_eq!(in_worktree("/app/../../etc/passwd"), None);
    }
}
