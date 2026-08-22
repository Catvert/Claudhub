//! Where one was when the window closed, and how it is put back.
//!
//! The settings say how Claudhub looks, `layout.json` where the panels are, the
//! store what a worktree is compared against. None of them said what one was
//! *doing*: reopening meant picking the repository again, the worktree again,
//! the file again, and retyping the query that was ready to go. That is four
//! gestures to get back to a state nobody had left on purpose.
//!
//! Three things are put back, and the choice of the three is the whole rule:
//! **what one had chosen**, never what one had obtained. The selected worktree,
//! the file open in the editor, the console's connection and the text of its
//! query — but no diff, no result grid, no status. A `SELECT` replayed on
//! opening is a query against a server nobody asked to reach, and everything
//! else arrives on its own from the reads the selection already triggers.
//!
//! The screen is not here either: it lives in `layout.json` with the panels'
//! places, which is where a window's geometry belongs.

use std::path::{Path, PathBuf};

use gpui::{App, Context, Window};

use crate::runtime::Cmd;
use crate::ui::app::ClaudhubApp;
use crate::ui::settings::Settings;
use crate::ui::store::{OpenConsole, OpenFile, Session, Store};

/// Nothing selected yet.
pub(super) const SELECTION_NONE: u8 = 0;
/// The first checkout of a repository that has just opened: a stopgap, so the
/// window is never empty while the repository holding the remembered worktree
/// is still being enumerated.
const SELECTION_FALLBACK: u8 = 1;
/// The worktree of the previous session.
pub(super) const SELECTION_SESSION: u8 = 2;
/// The checkout `claudhub` was launched from, and every choice made by hand.
/// Nothing displaces it — launching in a worktree opens *that* worktree.
pub(super) const SELECTION_CHOSEN: u8 = 3;

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
/// directory, which the view knows, that tells the two apart.
pub(super) fn pick_worktree(
    opened_at: Option<PathBuf>,
    worktrees: &[PathBuf],
    launch_dir: Option<&Path>,
    remembered: Option<&Path>,
) -> Option<(u8, PathBuf)> {
    let launched = opened_at
        .clone()
        .filter(|path| launch_dir.is_some_and(|dir| dir.starts_with(path)))
        .map(|path| (SELECTION_CHOSEN, path));
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
    /// Reopens the file that was in the editor, once its worktree is known to
    /// exist.
    ///
    /// Called every time a repository opens, because that is the only moment we
    /// learn about its checkouts. What never finds its worktree stays in
    /// `restoring` and simply goes away with the window: a file whose worktree
    /// has been removed is nothing to report.
    pub(super) fn restore_editing(&mut self, cx: &mut Context<Self>) {
        if self.restore_asked {
            return;
        }
        // The tabs, or the single file a state written before them named.
        let mut queue = self.restoring.tabs.clone();
        if queue.is_empty() {
            queue.extend(self.restoring.editing.clone());
        }
        let Some(first) = queue.first().cloned() else {
            return;
        };
        // They all belong to one worktree — the session files the editors of
        // the tree it was looking at — so one existence check answers for all.
        if !self.worktree_exists(&first.worktree) {
            return;
        }
        self.restore_asked = true;
        self.restoring_files = true;
        self.pending_files = queue;
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
        self.git.send(Cmd::ReadFile {
            worktree: next.worktree,
            path: next.path,
        });
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
        let Some(open) = self.restoring.editing.clone() else {
            return;
        };
        let panel = self
            .editors(&open.worktree)
            .and_then(|tabs| Some(tabs.open.get(tabs.index_of(&open.path)?)?.panel.clone()));
        if let Some(panel) = panel {
            crate::ui::panels::FilePanel::activate(&panel, window, cx);
        }
    }

    /// Is this content the one the restore asked for.
    ///
    /// It is what keeps the reopened file from calling up the "Files" screen:
    /// opening a file is a gesture that carries one to it, but here the gesture
    /// is a restore, and the screen being put back is the one from
    /// `layout.json`. The answer consumes the request: a second read of the
    /// same file, this time asked for by hand, gets the usual behaviour.
    pub(super) fn take_restored_editing(&mut self, worktree: &Path, path: &Path) -> bool {
        self.restoring_files
            && self
                .restoring
                .tabs
                .iter()
                .chain(self.restoring.editing.iter())
                .any(|open| open.worktree == worktree && open.path == path)
    }

    /// Reopens the SQL console on the connection it was on, with the query that
    /// was in the editor — not sent.
    ///
    /// It does not wait for anything: a connection is described in the
    /// settings, not in a repository. Named by its key rather than copied, so a
    /// connection edited in the meantime is the one that opens — and one that
    /// has been deleted simply does not come back.
    pub(super) fn restore_console(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(console) = self.restoring.console.take() else {
            return;
        };
        let Some(connection) = Settings::global(cx)
            .databases
            .iter()
            .find(|candidate| candidate.key() == console.connection)
            .cloned()
        else {
            return;
        };
        self.reopen_db_console(connection, console.database, console.query, window, cx);
    }

    /// Files where one is, and schedules the store's deferred write.
    ///
    /// Called from the gestures that move it — choosing a worktree, opening or
    /// closing a file, opening or closing the console, typing in the query
    /// editor — and not on quitting: a window is also closed by a crash, by a
    /// session ending, by a `kill`, and a state only written on the way out is
    /// a state one loses precisely on the days it would have helped.
    pub(super) fn persist_session(&mut self, cx: &mut App) {
        let session = Session {
            // What has not been put back yet stands in for what is not there:
            // the query editor emits a value per keystroke, and the first of
            // them arrives before any repository has answered. Without this,
            // typing one letter on startup would erase the worktree one is
            // about to be given back.
            worktree: self
                .active
                .clone()
                .or_else(|| self.restoring.worktree.clone()),
            editing: self
                .editing()
                .map(|editing| OpenFile {
                    worktree: editing.worktree.clone(),
                    path: editing.path.clone(),
                })
                .or_else(|| self.restoring.editing.clone()),
            // What has not been put back yet stands in for what is not there,
            // as the worktree above does: a keystroke lands before the first
            // file has come back, and filing an empty list there would lose
            // every tab of the previous session.
            tabs: self
                .editing_root()
                .and_then(|root| self.editors(&root))
                .filter(|tabs| !tabs.open.is_empty())
                .map(|tabs| {
                    tabs.open
                        .iter()
                        .map(|editing| OpenFile {
                            worktree: editing.worktree.clone(),
                            path: editing.path.clone(),
                        })
                        .collect()
                })
                .unwrap_or_else(|| self.restoring.tabs.clone()),
            console: self
                .query
                .connection
                .as_ref()
                .map(|connection| OpenConsole {
                    connection: connection.key(),
                    database: self.query.database.clone(),
                    query: self.db_query_input.read(cx).value().to_string(),
                }),
        };
        Store::update_global(cx, |store| store.session = session);
    }
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
    fn the_launch_directory_beats_everything() {
        // Launching `claudhub` in a worktree opens *that* worktree, whatever
        // the previous session was looking at.
        let chosen = pick_worktree(
            Some(PathBuf::from("/r/wt/b")),
            &paths(&["/r", "/r/wt/a", "/r/wt/b"]),
            Some(Path::new("/r/wt/b/app/Http")),
            Some(Path::new("/r/wt/a")),
        );
        assert_eq!(chosen, Some((SELECTION_CHOSEN, PathBuf::from("/r/wt/b"))));
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
            Some(Path::new("/r/wt/a")),
        );
        assert_eq!(chosen, Some((SELECTION_FALLBACK, PathBuf::from("/other"))));
    }

    #[test]
    fn a_repository_without_a_checkout_chooses_nothing() {
        assert_eq!(pick_worktree(None, &[], None, None), None);
    }
}
