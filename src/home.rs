//! The user's own folders: the home directory, a path typed with `~/`, and
//! where Claude Code keeps its configuration.
//!
//! One answer for every caller. Three modules read Claude's folder and two
//! expanded `~/`, and they did not all find the same home: a `HOME` set but
//! empty — what a service manager can leave behind — was a path to one of
//! them and nothing to the others.

use std::path::PathBuf;

/// The home directory: `$HOME` when it says something, the system's answer
/// otherwise.
///
/// `$HOME` first because it is what a shell line reads: the agents' hooks
/// write under `$HOME/.claudhub`, and the reader must look where the writer
/// wrote. An empty one is no answer — `PathBuf::from("")` joined with a name
/// is a path relative to wherever the process happens to be.
pub fn home() -> Option<PathBuf> {
    std::env::var_os("HOME")
        .filter(|home| !home.is_empty())
        .map(PathBuf::from)
        .or_else(|| directories::UserDirs::new().map(|dirs| dirs.home_dir().to_path_buf()))
}

/// A path typed into a form, `~` expanded.
///
/// A path typed there is written `~/dev/base.sqlite` — that is how it is
/// given to a shell — and passing it as it is to `std::fs` would look for, or
/// create, a folder named `~` in the current directory. `None` only when it
/// names the home and there is none.
pub fn expand(path: &str) -> Option<PathBuf> {
    let path = path.trim();
    if path == "~" {
        return home();
    }
    match path.strip_prefix("~/") {
        Some(rest) => home().map(|home| home.join(rest)),
        None => Some(PathBuf::from(path)),
    }
}

/// Where Claude Code keeps its configuration: `$CLAUDE_CONFIG_DIR` when it is
/// set, as Claude reads it, `~/.claude` otherwise.
pub fn claude_config() -> Option<PathBuf> {
    std::env::var_os("CLAUDE_CONFIG_DIR")
        .filter(|dir| !dir.is_empty())
        .map(PathBuf::from)
        .or_else(|| home().map(|home| home.join(".claude")))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// What does not start with `~/` is left as it was typed, spaces around
    /// it aside.
    #[test]
    fn only_a_leading_tilde_is_expanded() {
        assert_eq!(
            expand(" /srv/db.sqlite "),
            Some(PathBuf::from("/srv/db.sqlite"))
        );
        assert_eq!(expand("data/~/x"), Some(PathBuf::from("data/~/x")));
        assert_eq!(expand("~user/x"), Some(PathBuf::from("~user/x")));
        let Some(home) = home() else {
            return;
        };
        assert_eq!(
            expand("~/dev/base.sqlite"),
            Some(home.join("dev/base.sqlite"))
        );
        assert_eq!(expand("~"), Some(home));
    }
}
