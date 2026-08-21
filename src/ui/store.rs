//! What Claudhub remembers about a worktree between runs.
//!
//! Separate from the settings, and for the same reason: this is not a
//! preference written by hand but the state of work in progress — the base a
//! worktree is compared against, the folders collapsed in it, the next note
//! number. A `settings.json` holding a few hundred lines per repository would
//! no longer be editable.
//!
//! This file is written **from the interface thread**, which departs from
//! "`src/ui/` never does I/O". That is `settings.rs`'s precedent and the same
//! reason: a few kilobytes written once every half second are not worth a
//! round trip through the protocol. The rule targets git commands, whose
//! fastest already costs a frame, not the preference being put away.
//!
//! Like the settings, everything here is optional (`#[serde(default)]`): an
//! added field does not break a file already written, and an unreadable file
//! never prevents startup.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::Duration;

use gpui::{App, BorrowAppContext};
use serde::{Deserialize, Serialize};

use crate::ui::notes::Note;

/// The same delay as the settings: short enough that an abrupt shutdown loses
/// nothing visible, long enough that a mouse drag does not write one file per
/// frame.
const SAVE_DELAY: Duration = Duration::from_millis(500);

/// What survives a restart for a given checkout.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct WorktreeState {
    /// The main repository this checkout belongs to.
    ///
    /// It only serves the purge, and is indispensable to it: without it, an
    /// entry missing from the worktree list of a repository just opened cannot
    /// be told from an entry belonging to a repository not yet opened — and
    /// forgetting it would erase the notes of a perfectly live worktree.
    pub repo: PathBuf,
    /// The branch review's comparison base. The choice belongs to the
    /// worktree, and relearning it on every launch was the first gap this
    /// store filled.
    pub base: Option<String>,
    /// Folders collapsed in the review list.
    ///
    /// A `Vec` and not a `HashSet`: that is the shape JSON can write, and the
    /// view turns it back into a set on load.
    pub collapsed: Vec<PathBuf>,
    /// Review notes from an earlier state file, and nothing else.
    ///
    /// They now live in a folder of Markdown files: this field exists only to
    /// pour them in once, when the folder arrives, after which it is emptied.
    /// Nothing writes it any more.
    pub notes: Vec<Note>,
    /// The next note id. It cannot be derived from `notes`: a deleted note
    /// would free its number, and a note already sent to the agent is referred
    /// to there by a number that would then mean another one.
    pub next_note: u64,
}

/// What survives a restart for a repository, worktrees taken together.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct RepoState {
    /// The Sentry project, which belongs to the repository, not to the worktree or the account.
    pub sentry_project: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Store {
    /// Key: the checkout path, as everywhere else in Claudhub.
    pub worktrees: HashMap<PathBuf, WorktreeState>,
    /// Key: the main repository path.
    pub repos: HashMap<PathBuf, RepoState>,
}

impl Store {
    pub fn load() -> Self {
        let Some(path) = state_path() else {
            return Self::default();
        };
        match std::fs::read_to_string(&path) {
            Ok(text) => serde_json::from_str(&text).unwrap_or_else(|e| {
                // Overwriting a file we failed to read would lose the notes of
                // every worktree over a single malformed key.
                log::warn!("unreadable state ({}): {e}", path.display());
                Self::default()
            }),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Self::default(),
            Err(e) => {
                log::warn!("reading the state: {e}");
                Self::default()
            }
        }
    }

    pub fn save(&self) {
        let Some(path) = state_path() else { return };
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        match serde_json::to_string_pretty(self) {
            Ok(json) => {
                if let Err(e) = crate::ui::settings::write_private(&path, &json) {
                    log::warn!("writing the state: {e}");
                }
            }
            Err(e) => log::warn!("serialising the state: {e}"),
        }
    }

    pub fn worktree(&self, path: &Path) -> Option<&WorktreeState> {
        self.worktrees.get(path)
    }

    /// A checkout's state, created if needed.
    ///
    /// The repository is required on write because that is the only moment we
    /// know it for sure, and the purge depends on it.
    pub fn worktree_mut(&mut self, path: &Path, repo: &Path) -> &mut WorktreeState {
        let state = self.worktrees.entry(path.to_path_buf()).or_default();
        state.repo = repo.to_path_buf();
        state
    }

    /// Forgets the checkouts of a repository that no longer exist.
    ///
    /// Called when git has just enumerated the worktrees: that is the only
    /// moment the list is certain. Entries from another repository — and those
    /// written before the `repo` field existed, whose repository is empty — are
    /// left untouched: a dead entry is better than an erased note.
    pub fn forget_missing(&mut self, repo: &Path, alive: &[PathBuf]) {
        self.worktrees.retain(|path, state| {
            state.repo.as_os_str().is_empty() || state.repo != repo || alive.contains(path)
        });
    }
}

// --- Global -----------------------------------------------------------------

pub struct StateStore {
    store: Store,
    /// True when a deferred write is already scheduled: later changes join it
    /// instead of scheduling another.
    saving: bool,
}

impl gpui::Global for StateStore {}

impl Store {
    /// Installs the loaded state. To be called once, at startup.
    pub fn init_global(self, cx: &mut App) {
        cx.set_global(StateStore {
            store: self,
            saving: false,
        });
    }

    pub fn global(cx: &App) -> &Store {
        &cx.global::<StateStore>().store
    }

    /// Changes the state and schedules its write.
    ///
    /// No visible side effect, unlike the settings: nothing here carries a font
    /// or a theme, and the calling view already knows what it has to redraw.
    pub fn update_global(cx: &mut App, f: impl FnOnce(&mut Store)) {
        let changed = cx.update_global::<StateStore, _>(|store, _| {
            let before = store.store.clone();
            f(&mut store.store);
            store.store != before
        });
        if changed {
            schedule_save(cx);
        }
    }
}

fn schedule_save(cx: &mut App) {
    let already_scheduled =
        cx.update_global::<StateStore, _>(|store, _| std::mem::replace(&mut store.saving, true));
    if already_scheduled {
        return;
    }
    cx.spawn(async move |cx| {
        cx.background_executor().timer(SAVE_DELAY).await;
        let store = cx.update(|cx| {
            cx.update_global::<StateStore, _>(|store, _| {
                store.saving = false;
                store.store.clone()
            })
        });
        store.save();
    })
    .detach();
}

/// Where the state is kept, beside the settings and the layout.
fn state_path() -> Option<PathBuf> {
    crate::ui::settings::config_dir().map(|dir| dir.join("state.json"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_keys_take_their_defaults() {
        // A file written by a version that knew nothing of notes must keep
        // loading — which is the case for every file already on a user's disk
        // as soon as we add a field.
        let store: Store =
            serde_json::from_str(r#"{"worktrees":{"/w":{"base":"origin/main"}}}"#).unwrap();
        let state = store.worktree(Path::new("/w")).unwrap();
        assert_eq!(state.base.as_deref(), Some("origin/main"));
        assert!(state.notes.is_empty());
        assert!(state.collapsed.is_empty());
        assert!(store.repos.is_empty());
    }

    #[test]
    fn writing_a_worktree_records_its_repository() {
        let mut store = Store::default();
        store
            .worktree_mut(Path::new("/r/wt/a"), Path::new("/r"))
            .base = Some("dev".into());
        assert_eq!(
            store.worktree(Path::new("/r/wt/a")).unwrap().repo,
            PathBuf::from("/r")
        );
    }

    #[test]
    fn purging_only_touches_the_repository_being_listed() {
        let mut store = Store::default();
        store.worktree_mut(Path::new("/r/a"), Path::new("/r")).base = Some("dev".into());
        store.worktree_mut(Path::new("/r/b"), Path::new("/r")).base = Some("dev".into());
        // Another repository, not concerned by this enumeration.
        store
            .worktree_mut(Path::new("/other/a"), Path::new("/other"))
            .base = Some("main".into());
        // And an entry written before the repository was recorded: we do not
        // know whom it belongs to, so we leave it alone.
        store.worktrees.insert(
            PathBuf::from("/legacy"),
            WorktreeState {
                base: Some("main".into()),
                ..Default::default()
            },
        );

        store.forget_missing(Path::new("/r"), &[PathBuf::from("/r/a")]);

        assert!(store.worktree(Path::new("/r/a")).is_some());
        assert!(store.worktree(Path::new("/r/b")).is_none());
        assert!(store.worktree(Path::new("/other/a")).is_some());
        assert!(store.worktree(Path::new("/legacy")).is_some());
    }
}
