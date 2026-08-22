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
const SELECTION_SESSION: u8 = 2;
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

impl ClaudhubApp {
    /// Reopens the file that was in the editor, once its worktree is known to
    /// exist.
    ///
    /// Called every time a repository opens, because that is the only moment we
    /// learn about its checkouts. What never finds its worktree stays in
    /// `restoring` and simply goes away with the window: a file whose worktree
    /// has been removed is nothing to report.
    pub(super) fn restore_editing(&mut self, cx: &mut Context<Self>) {
        let Some(open) = self.restoring.editing.clone() else {
            return;
        };
        if self.restore_asked || !self.worktree_exists(&open.worktree) {
            return;
        }
        self.restore_asked = true;
        self.git.send(Cmd::ReadFile {
            worktree: open.worktree,
            path: open.path,
        });
        cx.notify();
    }

    /// Is this content the one the restore asked for.
    ///
    /// It is what keeps the reopened file from calling up the "Files" screen:
    /// opening a file is a gesture that carries one to it, but here the gesture
    /// is a restore, and the screen being put back is the one from
    /// `layout.json`. The answer consumes the request: a second read of the
    /// same file, this time asked for by hand, gets the usual behaviour.
    pub(super) fn take_restored_editing(&mut self, worktree: &Path, path: &Path) -> bool {
        let restored = self.restoring.editing.take();
        // Cleared either way: a file opened by hand ends the restore, and
        // leaving the request standing would make `persist_session` keep filing
        // a file nobody managed to open.
        restored.is_some_and(|open| open.worktree == worktree && open.path == path)
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
                .editing
                .as_ref()
                .map(|editing| OpenFile {
                    worktree: editing.worktree.clone(),
                    path: editing.path.clone(),
                })
                .or_else(|| self.restoring.editing.clone()),
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
