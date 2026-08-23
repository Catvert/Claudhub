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
    /// A language server runs on this worktree.
    ///
    /// Per worktree and off by default: a server is a process holding hundreds
    /// of megabytes on a large project, and a dozen open worktrees are not a
    /// dozen worktrees being edited. It lives in the store and not in the
    /// settings for the reason the store exists — this is where one is at, like
    /// the comparison base, not a preference written by hand.
    pub lsp: bool,
    /// The next note id. It cannot be derived from `notes`: a deleted note
    /// would free its number, and a note already sent to the agent is referred
    /// to there by a number that would then mean another one.
    pub next_note: u64,
    /// Where the work stood in this checkout.
    pub place: Place,
}

/// The file open in the built-in editor, and the checkout it belongs to.
///
/// Both, because they are two independent choices: the editor keeps showing a
/// file of the worktree one has left, and reopening it under the selected one
/// would read another file — or none.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct OpenFile {
    pub worktree: PathBuf,
    pub path: PathBuf,
}

/// The SQL console as it stood.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct OpenConsole {
    /// The connection's key (`db::Connection::key`) and not the connection
    /// itself: it names it without carrying its password, and the settings are
    /// where a connection is described — a copy here would age the day one is
    /// edited.
    pub connection: String,
    pub database: Option<String>,
    /// The text of the query, sent or not. It is what one comes back to
    /// tomorrow, and the result is not: replaying a `SELECT` nobody asked for
    /// on opening is a query against a production server one did not ask for.
    pub query: String,
}

/// Where the work stood in one checkout.
///
/// **What one had chosen, never what one had obtained**: the screen, the tabs,
/// the console's connection and the text of its query — no diff, no result
/// grid, no status. A `SELECT` replayed on arrival is a query against a server
/// nobody asked to reach, and everything else comes back on its own from the
/// reads the selection already triggers.
///
/// It lives **per worktree** because that is what it describes: the file open in
/// the editor is one of that checkout's files, and the screen one was on says
/// what one was doing *there*. A single global place meant that going to another
/// project to check one thing and coming back cost the file, the screen and the
/// query — four gestures to get back to a state nobody had left on purpose.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Place {
    /// The screen being looked at, by `Workspace::key`.
    ///
    /// **Only a screen one works in**: the settings and the multiplexer are
    /// detours, and coming back to a project should not put one back in the
    /// detour one took while leaving it. It is the rule `worked_in` follows.
    pub screen: Option<String>,
    /// The file that was **on screen**, among the tabs. Kept apart from the
    /// list so that a state written before tabs existed still opens what it
    /// named, and so that reopening knows which tab to bring forward.
    pub editing: Option<OpenFile>,
    /// Every file the editor had open, in tab order.
    pub tabs: Vec<OpenFile>,
    pub console: Option<OpenConsole>,
}

impl Place {
    /// Nothing worth putting back.
    pub fn is_empty(&self) -> bool {
        *self == Self::default()
    }
}

/// What the window was showing when it was closed.
///
/// One field now: **which worktree**. The rest of where one was moved into that
/// worktree's own entry the day places became per checkout — see [`Place`] —
/// and what stays here is precisely what is not about any one of them: which of
/// the twelve the window opens on.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Session {
    /// The selected worktree. It only wins over the first of the list, never
    /// over the checkout `claudhub` was launched from — see
    /// `ClaudhubApp::repo_opened`.
    pub worktree: Option<PathBuf>,
    /// **Legacy**: where the work stood, back when there was one place for the
    /// whole window. Poured into its worktree's entry once, then cleared —
    /// the path `migrate_sentry` and the notes' recovery already take.
    ///
    /// Flattened, so a `state.json` written before this change is read without
    /// a migration of its shape: the three keys were at this level already.
    #[serde(flatten)]
    pub place: Place,
}

/// What survives a restart for a repository, worktrees taken together.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct RepoState {
    /// **Legacy**: read once so `migrate_sentry` can pour it into the Sentry
    /// plugin's per-repository state, then cleared.
    pub sentry_project: Option<String>,
    /// What each plugin has remembered about this repository: plugin id → its
    /// own keys.
    ///
    /// **Per repository and not per worktree**, because that is what a
    /// project's configuration means — a board's identifier, an API's base
    /// address are the same across five checkouts of the same code. A plugin
    /// wanting finer grain puts the worktree in its own key; the namespace is
    /// its own.
    #[serde(default)]
    pub plugin_state:
        std::collections::BTreeMap<String, std::collections::BTreeMap<String, String>>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Store {
    /// Key: the checkout path, as everywhere else in Claudhub.
    pub worktrees: HashMap<PathBuf, WorktreeState>,
    /// Key: the main repository path.
    pub repos: HashMap<PathBuf, RepoState>,
    /// Where one was, all repositories taken together.
    pub session: Session,
    /// The checkouts pinned to the top bar, in the order they were pinned.
    ///
    /// Here and not in the settings, for the reason this file exists: a pin is
    /// where one is working this week, not a preference one writes by hand. And
    /// a flat list across every repository, because that is what the row in the
    /// bar is — one goes from a checkout of one project to a checkout of
    /// another, which is the switch the pins exist to make one click.
    ///
    /// A path that no repository claims is kept and simply not painted: a
    /// repository one has not opened yet is not a repository whose pins should
    /// be forgotten. What `forget_missing` drops is a checkout git has just
    /// said is gone.
    pub pinned: Vec<PathBuf>,
}

impl Store {
    pub fn load() -> Self {
        let mut store = Self::read();
        store.migrate_sentry();
        store.migrate_place();
        store
    }

    /// The window's one place becomes its worktree's.
    ///
    /// The same path as a fresh write, like `migrate_sentry`: the field is
    /// poured into the entry and cleared, so it happens once, and a worktree
    /// that already has a place of its own keeps it. An entry created here
    /// carries no repository, which the purge reads as "not mine to forget" —
    /// the very rule that protects a state written before that field existed.
    fn migrate_place(&mut self) {
        if self.session.place.is_empty() {
            return;
        }
        let place = std::mem::take(&mut self.session.place);
        let Some(worktree) = self.session.worktree.clone() else {
            return;
        };
        let state = self.worktrees.entry(worktree).or_default();
        if state.place.is_empty() {
            state.place = place;
        }
    }

    /// The Sentry project of each repository becomes the Sentry **plugin**'s.
    ///
    /// The same path as a fresh write, like the notes' recovery and
    /// `migrate_agents`: the field is poured into `plugin_state` and cleared,
    /// so it happens once, and a repository already configured through the
    /// plugin keeps what it has.
    fn migrate_sentry(&mut self) {
        for repo in self.repos.values_mut() {
            let Some(project) = repo.sentry_project.take() else {
                continue;
            };
            if project.trim().is_empty() {
                continue;
            }
            repo.plugin_state
                .entry("sentry".to_string())
                .or_default()
                .entry("project".to_string())
                .or_insert(project);
        }
    }

    fn read() -> Self {
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
        let mut gone = Vec::new();
        self.worktrees.retain(|path, state| {
            let keep =
                state.repo.as_os_str().is_empty() || state.repo != repo || alive.contains(path);
            if !keep {
                gone.push(path.clone());
            }
            keep
        });
        // A pin on a checkout that no longer exists goes with it. It is the one
        // moment we know for sure, and the pin carries no repository of its own
        // to be judged on later.
        self.pinned.retain(|path| !gone.contains(path));
    }

    pub fn is_pinned(&self, path: &Path) -> bool {
        self.pinned.iter().any(|pinned| pinned == path)
    }

    /// Pins a checkout, or unpins it. A new pin goes **last**: the row is read
    /// left to right, and a pin that pushed the others along would move the
    /// button one was aiming at.
    pub fn toggle_pin(&mut self, path: &Path) {
        if let Some(ix) = self.pinned.iter().position(|pinned| pinned == path) {
            self.pinned.remove(ix);
        } else {
            self.pinned.push(path.to_path_buf());
        }
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
    ///
    /// Whether anything moved is told by comparing a copy of the whole state,
    /// which is fine for a gesture but not for what happens per keystroke:
    /// there, [`Store::update_global_if`] lets the caller answer itself.
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

    /// The same, for a caller that knows whether it changed anything: the
    /// closure answers, and no copy of the state is taken.
    pub fn update_global_if(cx: &mut App, f: impl FnOnce(&mut Store) -> bool) {
        let changed = cx.update_global::<StateStore, _>(|store, _| f(&mut store.store));
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
        // Including the session, which no file written before it carries.
        assert_eq!(store.session, Session::default());
    }

    fn a_place() -> Place {
        Place {
            screen: Some("db".into()),
            editing: Some(OpenFile {
                worktree: PathBuf::from("/r/wt/a"),
                path: PathBuf::from("app/Models/User.php"),
            }),
            tabs: vec![
                OpenFile {
                    worktree: PathBuf::from("/r/wt/a"),
                    path: PathBuf::from("routes/web.php"),
                },
                OpenFile {
                    worktree: PathBuf::from("/r/wt/a"),
                    path: PathBuf::from("app/Models/User.php"),
                },
            ],
            console: Some(OpenConsole {
                connection: "mysql:root@localhost:3306/".into(),
                database: Some("shop".into()),
                query: "SELECT * FROM users".into(),
            }),
        }
    }

    #[test]
    fn a_place_survives_the_round_trip() {
        let mut store = Store {
            session: Session {
                worktree: Some(PathBuf::from("/r/wt/a")),
                ..Default::default()
            },
            ..Default::default()
        };
        store
            .worktree_mut(Path::new("/r/wt/a"), Path::new("/r"))
            .place = a_place();
        let text = serde_json::to_string(&store).unwrap();
        let back: Store = serde_json::from_str(&text).unwrap();
        assert_eq!(
            back.worktree(Path::new("/r/wt/a")).unwrap().place,
            a_place()
        );
        assert_eq!(back.session.worktree, store.session.worktree);
        // The password is nowhere in it: a connection is named by its key, and
        // described in the settings.
        assert!(!text.contains("password"));
    }

    /// A `state.json` from before places were per worktree carries the three
    /// keys at the session's level, and the flatten reads them there: the
    /// migration then pours them into the worktree the session names, once.
    #[test]
    fn a_window_wide_place_moves_into_its_worktree() {
        let text = r#"{
            "worktrees": {"/r/wt/a": {"repo": "/r", "base": "dev"}},
            "session": {
                "worktree": "/r/wt/a",
                "editing": {"worktree": "/r/wt/a", "path": "routes/web.php"},
                "tabs": [{"worktree": "/r/wt/a", "path": "routes/web.php"}]
            }
        }"#;
        let mut store: Store = serde_json::from_str(text).unwrap();
        store.migrate_place();
        let state = store.worktree(Path::new("/r/wt/a")).unwrap();
        assert_eq!(state.place.tabs.len(), 1);
        assert_eq!(
            state.base.as_deref(),
            Some("dev"),
            "nothing else is touched"
        );
        // Emptied, so a second run finds nothing to pour and a place edited
        // since is not overwritten by a copy of the old one.
        assert!(store.session.place.is_empty());
        assert_eq!(store.session.worktree, Some(PathBuf::from("/r/wt/a")));
    }

    /// A place with no worktree to file it under is dropped rather than left
    /// where a second migration would find it.
    #[test]
    fn a_place_without_a_worktree_goes_nowhere() {
        let mut store = Store {
            session: Session {
                worktree: None,
                place: a_place(),
            },
            ..Default::default()
        };
        store.migrate_place();
        assert!(store.session.place.is_empty());
        assert!(store.worktrees.is_empty());
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

    #[test]
    fn a_pin_goes_last_and_toggles_off() {
        let mut store = Store::default();
        store.toggle_pin(Path::new("/r/a"));
        store.toggle_pin(Path::new("/r/b"));
        assert_eq!(
            store.pinned,
            vec![PathBuf::from("/r/a"), PathBuf::from("/r/b")]
        );
        store.toggle_pin(Path::new("/r/a"));
        assert_eq!(store.pinned, vec![PathBuf::from("/r/b")]);
        assert!(!store.is_pinned(Path::new("/r/a")));
    }

    /// A pin follows the checkout it names: git says it is gone, the button
    /// goes. What belongs to a repository nobody has enumerated stays — the
    /// rule the notes already follow.
    #[test]
    fn purging_takes_the_pins_of_what_it_forgets() {
        let mut store = Store::default();
        store.worktree_mut(Path::new("/r/a"), Path::new("/r")).base = Some("dev".into());
        store.worktree_mut(Path::new("/r/b"), Path::new("/r")).base = Some("dev".into());
        store.toggle_pin(Path::new("/r/a"));
        store.toggle_pin(Path::new("/r/b"));
        store.toggle_pin(Path::new("/elsewhere/c"));

        store.forget_missing(Path::new("/r"), &[PathBuf::from("/r/a")]);

        assert_eq!(
            store.pinned,
            vec![PathBuf::from("/r/a"), PathBuf::from("/elsewhere/c")]
        );
    }
}
