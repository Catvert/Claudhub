//! Where one was when the window closed, and how it is put back.
//!
//! The settings say how Claudhub looks, `layout.json` where the panels are, the
//! store what a worktree is compared against. None of them said what one was
//! *doing*: reopening meant picking the repository again, the worktree again,
//! the file again, and retyping the query that was ready to go. That is four
//! gestures to get back to a state nobody had left on purpose.
//!
//! What is put back is chosen by one rule: **what one had chosen**, never what
//! one had obtained. The screen, the files open in the editor, the console's
//! connection and the text of its query — but no diff, no result grid, no
//! status. A `SELECT` replayed on arrival is a query against a server nobody
//! asked to reach, and everything else comes back on its own from the reads the
//! selection already triggers.
//!
//! **And it is kept per worktree** (`store::Place`), which is the other half of
//! the same idea: going to another project to check one thing and coming back
//! used to cost the file, the screen and the query. `layout.json` still says
//! where the panels are — that is the *window's* geometry, and it is the same
//! on every project — and it still names the screen the window opens on, which
//! is what answers before any repository has.
//!
//! Three moments, and they are not the same:
//!
//! - **The screen and the console** are put back at every arrival: they are one
//!   apiece for the whole window, so the worktree one leaves has just written
//!   over them.
//! - **The tabs** are put back on the first arrival only. A file's panel hides
//!   itself when the editing root changes and comes back with it — the same
//!   answer terminals give — so a worktree visited earlier in the session still
//!   has its files, opened where one left them.
//! - **Nothing at all** while the selection is a stopgap. Until the
//!   repositories have finished answering, the window shows the first checkout
//!   of whichever replied first, and replaying its place would open files in a
//!   project the session never asked for — `session_terminal_due`, the rule the
//!   first terminal already follows.

use std::path::{Path, PathBuf};

use gpui::{App, Context, Window};

use crate::ui::app::ClaudhubApp;
use crate::ui::settings::Settings;
use crate::ui::store::{OpenConsole, OpenFile, Place, Store};

/// Nothing selected yet.
pub(super) const SELECTION_NONE: u8 = 0;
/// The first checkout of a repository that has just opened: a stopgap, so the
/// window is never empty while the repository holding the remembered worktree
/// is still being enumerated.
const SELECTION_FALLBACK: u8 = 1;
/// The checkout the shell **happened to sit in** when `claudhub` was
/// launched with no argument. Ambient, not chosen: a terminal parked in some
/// project for other reasons must not hijack the session — the remembered
/// worktree displaces it. Firmer than the fallback: with nothing remembered,
/// where one launched is the best guess there is.
const SELECTION_LAUNCHED: u8 = 2;
/// The worktree of the previous session.
pub(super) const SELECTION_SESSION: u8 = 3;
/// The folder **named**: `claudhub <chemin>`, « Ouvrir avec Claudhub », the
/// folder picker, and every choice made by hand. Nothing displaces it.
pub(super) const SELECTION_CHOSEN: u8 = 4;

/// Which checkout a repository that has just opened should show, and how firmly.
///
/// Three candidates, strongest first: the checkout the window was **launched
/// in**, the worktree of the **previous session**, and failing both the
/// **first** of the list, which is the main repository. The rank is what lets
/// the answers come back in any order: the repositories are enumerated by
/// whichever worker finishes first, and without it the remembered worktree
/// would win or lose depending on the day.
///
/// `opened_at` alone does not say "launched here": a remembered repository asks
/// for its own root, so it too comes back with a checkout. It is the launch
/// directory, which the view knows, that tells the two apart — and `chosen`
/// says whether that directory was **named** (an argument, a hand-over) or
/// merely the shell's ambient working directory, which yields to the session.
pub(super) fn pick_worktree(
    opened_at: Option<PathBuf>,
    worktrees: &[PathBuf],
    launch_dir: Option<&Path>,
    chosen: bool,
    remembered: Option<&Path>,
) -> Option<(u8, PathBuf)> {
    let launched = opened_at
        .clone()
        .filter(|path| launch_dir.is_some_and(|dir| dir.starts_with(path)))
        .map(|path| {
            (
                if chosen {
                    SELECTION_CHOSEN
                } else {
                    SELECTION_LAUNCHED
                },
                path,
            )
        });
    let remembered = remembered
        .filter(|path| worktrees.iter().any(|known| known == path))
        .map(|path| (SELECTION_SESSION, path.to_path_buf()));
    let first = opened_at
        .or_else(|| worktrees.first().cloned())
        .map(|path| (SELECTION_FALLBACK, path));
    launched.or(remembered).or(first)
}

/// Whether the session's first terminal may be opened on the worktree now
/// shown.
///
/// The rule the window cannot express: a selection **weaker than the session's**
/// is a stopgap — the first checkout of whichever repository answered first,
/// there so the window is not empty — and a terminal opened on it is a shell in
/// a project one never asked for. It waits for the repositories still owed an
/// answer; when none is left, the stopgap *is* the answer.
///
/// `pending` counts `Cmd::OpenRepo` alone, which always answers. The launch
/// probe is silent when the directory is not a repository, so counting it would
/// mean waiting for ever.
pub(super) fn session_terminal_due(rank: u8, pending: usize) -> bool {
    rank >= SELECTION_SESSION || pending == 0
}

impl ClaudhubApp {
    /// What the store keeps of a checkout's place.
    fn place_of(&self, worktree: &Path, cx: &App) -> Place {
        crate::ui::store::Store::global(cx)
            .worktrees
            .get(worktree)
            .map(|state| state.place.clone())
            .unwrap_or_default()
    }

    /// Puts back where the work stood in the worktree now shown.
    ///
    /// Called from the two places a selection can settle: the gesture itself,
    /// and the moment the last repository answers — a stopgap that nothing
    /// displaces *becomes* the answer, and it is only then that it may open
    /// anything. Idempotent by `settled`, which is what lets both call it.
    pub(super) fn settle_place(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(worktree) = self.active.clone() else {
            return;
        };
        if !session_terminal_due(self.selection_rank, self.opening_repos) {
            return;
        }
        if self.settled.as_deref() == Some(worktree.as_path()) {
            return;
        }
        self.settled = Some(worktree.clone());
        // Nothing is filed while the halves are being swapped one after
        // another: the first of them files.
        self.settling = true;
        let place = self.place_of(&worktree, cx);
        // The document first: what follows opens files and a console, and doing
        // it the other way round would show them landing behind the tab one is
        // about to leave.
        //
        // A name nothing can build is no document to come back to: a plugin's
        // detail, when that plugin has been uninstalled between two sessions.
        if let Some(name) = place
            .document
            .as_deref()
            .and_then(crate::ui::panels::registered_name)
        {
            self.reveal_panel(name, window, cx);
        }
        self.restore_consoles(&worktree, place.consoles.clone(), window, cx);
        // The tabs on the **first** arrival only: a file's panel hides itself
        // when the editing root changes and comes back with it, so a worktree
        // visited earlier in this session still has its files.
        if self.replayed.insert(worktree) {
            self.restore_tabs(place, cx);
        }
        self.settling = false;
        cx.notify();
    }

    /// Reopens the files a checkout had open, in tab order.
    fn restore_tabs(&mut self, place: Place, cx: &mut Context<Self>) {
        // The tabs, or the single file a state written before them named.
        let mut queue = place.tabs.clone();
        if queue.is_empty() {
            queue.extend(place.editing.clone());
        }
        let Some(first) = queue.first() else {
            return;
        };
        // They all belong to one checkout — a place files the editors of the
        // tree it describes — so one existence check answers for all.
        if !self.worktree_exists(&first.worktree) {
            return;
        }
        self.replaying = queue.clone();
        self.pending_files = queue;
        self.replaying_focus = place.editing;
        self.restoring_files = true;
        self.read_next_file(cx);
    }

    /// Asks for the next remembered tab, or ends the restore.
    fn read_next_file(&mut self, cx: &mut Context<Self>) {
        if self.pending_files.is_empty() {
            self.restoring_files = false;
            cx.notify();
            return;
        }
        let next = self.pending_files.remove(0);
        self.git.send(self.read_file_cmd(next.worktree, next.path));
        cx.notify();
    }

    /// One remembered tab has arrived: asks for the one after it, and brings
    /// the tab that was on screen forward once they are all there.
    ///
    /// The activation is left to the end on purpose: each opening makes its own
    /// tab the displayed one, so doing it earlier would be undone by the file
    /// after it.
    pub(super) fn continue_restore(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.read_next_file(cx);
        if self.restoring_files {
            return;
        }
        let Some(open) = self.replaying_focus.take() else {
            self.replaying.clear();
            return;
        };
        let panel = self
            .editors(&open.worktree)
            .and_then(|tabs| Some(tabs.open.get(tabs.index_of(&open.path)?)?.panel.clone()));
        for panel in panel.iter() {
            crate::ui::panels::FilePanel::activate(panel, window, cx);
        }
        self.replaying.clear();
    }

    /// Is this content the one a restore asked for.
    ///
    /// It is what keeps a reopened file from calling up the "Files" screen:
    /// opening a file is a gesture that carries one to it, but here the gesture
    /// is an arrival, and the screen being put back is the place's own. The
    /// answer consumes nothing: the list is cleared when the last file lands.
    pub(super) fn take_restored_editing(&mut self, worktree: &Path, path: &Path) -> bool {
        self.restoring_files
            && self
                .replaying
                .iter()
                .any(|open| open.worktree == worktree && open.path == path)
    }

    /// Puts this checkout's SQL consoles back, each on the connection it was
    /// using and with the query that was in its editor — not sent.
    ///
    /// **Nothing is cleared any more**, where one console for the whole window
    /// had to be: a console belongs to its worktree and keeps its tab, hidden,
    /// while one looks elsewhere. That is also why this does nothing when the
    /// checkout already has one — arriving is not opening a second set.
    ///
    /// A connection is named by its key rather than copied, so one edited in
    /// the meantime is the one that opens — and one that has been deleted
    /// simply does not come back.
    fn restore_consoles(
        &mut self,
        worktree: &Path,
        consoles: Vec<OpenConsole>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.consoles_of(worktree).next().is_some() {
            return;
        }
        for console in consoles {
            let Some(connection) = Settings::global(cx)
                .databases
                .iter()
                .find(|candidate| candidate.key() == console.connection)
                .cloned()
            else {
                continue;
            };
            self.reopen_db_console(
                worktree.to_path_buf(),
                connection,
                console.database,
                console.query,
                window,
                cx,
            );
        }
    }

    /// Files where one is, and schedules the store's deferred write.
    ///
    /// Called from the gestures that move it — choosing a worktree, opening or
    /// closing a file, opening or closing the console, typing in the query
    /// editor, changing screen — and not on quitting: a window is also closed by
    /// a crash, by a session ending, by a `kill`, and a state only written on
    /// the way out is a state one loses precisely on the days it would have
    /// helped.
    pub(super) fn persist_session(&mut self, cx: &mut App) {
        // What has not been put back yet stands in for what is not there: the
        // query editor emits a value per keystroke, and the first of them
        // arrives before any repository has answered. Without this, typing one
        // letter on startup would erase the worktree one is about to be given
        // back.
        //
        // And a **stopgap stands aside for the same reason**: while
        // repositories are still owed an answer, what the window shows is the
        // first checkout of whichever replied first — filing it would replace
        // the remembered worktree with a race's winner, and a close or a
        // crash in that window teleports the next session to a project nobody
        // chose. When nothing is pending any more, the stopgap *is* the
        // answer — `session_terminal_due`, the first terminal's own rule.
        let worktree = self
            .active
            .clone()
            .filter(|_| session_terminal_due(self.selection_rank, self.opening_repos))
            .or_else(|| self.restoring.worktree.clone());
        let repo = worktree.as_deref().and_then(|path| self.main_of(path));
        // No worktree, or one whose repository is not open: there is nowhere to
        // file a place, and inventing an entry would leave a row the purge
        // cannot judge.
        let (Some(here), Some(repo)) = (worktree.clone(), repo) else {
            Store::update_global_if(cx, |store| set_worktree(store, worktree));
            return;
        };
        // A place is only written once its own has been put back: until then
        // what the window shows is the previous project's, and filing it here
        // would copy one checkout's tabs on to another.
        // Nothing is filed while a place is being put back — neither during the
        // swap itself nor while the files are still on their way. A keystroke in
        // the query editor lands well before the last of them, and an empty tab
        // list written then would lose every one of them.
        if self.settling || self.restoring_files || self.settled.as_deref() != Some(here.as_path())
        {
            Store::update_global_if(cx, |store| set_worktree(store, worktree));
            return;
        }
        let place = Place {
            // The document in front of the centre, which is where one was: a
            // tool window is beside what one is doing rather than what one is
            // doing, so folding one is not a place to be brought back to.
            document: self.front_document(cx),
            editing: self.editing().map(|editing| OpenFile {
                worktree: editing.worktree.clone(),
                path: editing.path.clone(),
            }),
            tabs: self
                .editing_root()
                .and_then(|root| self.editors(&root))
                .map(|tabs| {
                    tabs.open
                        .iter()
                        .map(|editing| OpenFile {
                            worktree: editing.worktree.clone(),
                            path: editing.path.clone(),
                        })
                        .collect()
                })
                .unwrap_or_default(),
            consoles: self
                .consoles_of(here.as_path())
                .filter_map(|console| {
                    let connection = console.state.connection.as_ref()?;
                    Some(OpenConsole {
                        connection: connection.key(),
                        database: console.state.database.clone(),
                        query: console.input.read(cx).value().to_string(),
                    })
                })
                .collect(),
        };
        // One write for both: this runs on every keystroke of the query editor,
        // and each `update_global` used to copy the whole state to find out
        // whether anything had moved.
        Store::update_global_if(cx, |store| {
            let mut changed = set_worktree(store, worktree);
            // A checkout seen for the first time — or one whose repository
            // `worktree_mut` is about to fill in — is a change in itself, place
            // or no place: without this its row would only reach the disk on
            // the next gesture.
            changed |= store.worktree(&here).is_none_or(|state| state.repo != repo);
            let state = store.worktree_mut(&here, &repo);
            if state.place != place {
                state.place = place;
                changed = true;
            }
            changed
        });
    }
}

/// Files which worktree the window is on, and says whether that moved.
fn set_worktree(store: &mut Store, worktree: Option<PathBuf>) -> bool {
    if store.session.worktree == worktree {
        return false;
    }
    store.session.worktree = worktree;
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    fn paths(list: &[&str]) -> Vec<PathBuf> {
        list.iter().map(PathBuf::from).collect()
    }

    /// The bug this rule exists for: the terminal used to open on whatever
    /// checkout filled the window first, so on a project the session had not
    /// asked for — and the flag then denied the remembered worktree its own.
    #[test]
    fn a_stopgap_selection_does_not_get_the_session_terminal() {
        assert!(!session_terminal_due(SELECTION_FALLBACK, 2));
        // Nothing left to answer: the stopgap is the answer.
        assert!(session_terminal_due(SELECTION_FALLBACK, 0));
    }

    /// The remembered worktree and every gesture are final, whatever is still
    /// being enumerated: waiting for the rest would leave a shell one has to
    /// ask for.
    #[test]
    fn a_settled_selection_gets_it_at_once() {
        assert!(session_terminal_due(SELECTION_SESSION, 3));
        assert!(session_terminal_due(SELECTION_CHOSEN, 3));
    }

    #[test]
    fn a_named_folder_beats_everything() {
        // `claudhub <chemin>` and « Ouvrir avec Claudhub » open *that*
        // worktree, whatever the previous session was looking at.
        let chosen = pick_worktree(
            Some(PathBuf::from("/r/wt/b")),
            &paths(&["/r", "/r/wt/a", "/r/wt/b"]),
            Some(Path::new("/r/wt/b/app/Http")),
            true,
            Some(Path::new("/r/wt/a")),
        );
        assert_eq!(chosen, Some((SELECTION_CHOSEN, PathBuf::from("/r/wt/b"))));
    }

    /// The bug this distinction exists for: a terminal parked in some project
    /// — a `just run` from the checkout, a shell one forgot to leave — made
    /// every launch "teleport" there, and then filed it as the session.
    #[test]
    fn an_ambient_launch_directory_yields_to_the_session() {
        let ambient = pick_worktree(
            Some(PathBuf::from("/r/wt/b")),
            &paths(&["/r", "/r/wt/a", "/r/wt/b"]),
            Some(Path::new("/r/wt/b/app/Http")),
            false,
            Some(Path::new("/r/wt/a")),
        );
        // Graded below the session — the remembered worktree displaces it,
        // whichever repository answers first — and above the fallback: with
        // nothing remembered, where one launched is the best guess.
        assert_eq!(
            ambient,
            Some((SELECTION_LAUNCHED, PathBuf::from("/r/wt/b")))
        );
    }

    #[test]
    fn the_remembered_worktree_beats_the_first_of_the_list() {
        // A remembered repository asks for its own root: `opened_at` names the
        // main checkout, which is exactly the fallback the session must win
        // against.
        let chosen = pick_worktree(
            Some(PathBuf::from("/r")),
            &paths(&["/r", "/r/wt/a"]),
            None,
            false,
            Some(Path::new("/r/wt/a")),
        );
        assert_eq!(chosen, Some((SELECTION_SESSION, PathBuf::from("/r/wt/a"))));
    }

    #[test]
    fn a_worktree_of_another_repository_is_not_a_candidate() {
        // Every repository that opens asks the question, and the one holding
        // the remembered worktree may not have answered yet: this one takes its
        // own first checkout, and the rank lets the other displace it later.
        let chosen = pick_worktree(
            Some(PathBuf::from("/other")),
            &paths(&["/other"]),
            None,
            false,
            Some(Path::new("/r/wt/a")),
        );
        assert_eq!(chosen, Some((SELECTION_FALLBACK, PathBuf::from("/other"))));
    }

    #[test]
    fn a_repository_without_a_checkout_chooses_nothing() {
        assert_eq!(pick_worktree(None, &[], None, false, None), None);
    }
}
