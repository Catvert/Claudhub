//! The writes under way, and what the window says about them.
//!
//! A `fetch`, a `push`, a `wt up` take seconds, sometimes minutes, during which
//! nothing moved on screen. This is the set of what has gone out and has not
//! come back — what a button reads to spin, and what the status bar reads to
//! name what is still running.
//!
//! Free of gpui on purpose. The single failure mode of a waiting indicator is a
//! button that spins for ever, which says less than a button that never spun,
//! and it comes from a key put down that never gets taken back: that is a thing
//! to be tested, not a thing to be watched for.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use crate::runtime::Action;

/// One operation under way, keyed exactly as the worker will answer.
///
/// `write_then_refresh` echoes the worktree and the action it was given, so
/// what is put down here is exactly what [`InFlight::finish`] will find. The
/// worktree is an `Option` because `wt up` and `wt down` name none — `wt` works
/// from the main repository.
type Key = (Option<PathBuf>, Action);

#[derive(Debug, Default)]
pub struct InFlight {
    running: HashSet<Key>,
    /// The worktree a `wt up` or `wt down` is working on.
    ///
    /// Those two do not name a worktree in their answer, so `running` alone
    /// would say "something is starting" without saying where, and every badge
    /// in the list would spin. One at a time: starting two projects at once is
    /// not a gesture.
    wt_pending: Option<PathBuf>,
}

impl InFlight {
    /// Remembers a write is under way.
    pub fn start(&mut self, worktree: Option<PathBuf>, action: Action) {
        self.running.insert((worktree, action));
    }

    /// Takes the key back, and lets go of the `wt` worktree when it was one.
    pub fn finish(&mut self, worktree: &Option<PathBuf>, action: Action) {
        self.running.remove(&(worktree.clone(), action));
        if matches!(action, Action::WtUp | Action::WtDown) {
            self.wt_pending = None;
        }
    }

    /// Forgets everything under way.
    ///
    /// For the two moments when what was in flight will never answer: the
    /// server dying, and a new one taking its place.
    pub fn clear(&mut self) {
        self.running.clear();
        self.wt_pending = None;
    }

    /// Is this operation under way? What a button reads to turn.
    pub fn is_running(&self, worktree: Option<&Path>, action: Action) -> bool {
        self.running
            .contains(&(worktree.map(Path::to_path_buf), action))
    }

    pub fn is_empty(&self) -> bool {
        self.running.is_empty()
    }

    /// The worktree a `wt up` or `wt down` is working on.
    pub fn wt_target(&self) -> Option<&Path> {
        self.wt_pending.as_deref()
    }

    pub fn set_wt_target(&mut self, worktree: PathBuf) {
        self.wt_pending = Some(worktree);
    }

    /// The i18n keys of what is running, for the status bar.
    ///
    /// **Sorted and without duplicates**, and sorted on the key rather than on
    /// the translated label: a `HashSet` iterates in a different order on every
    /// frame, and the words would dance. Sorting on the key also keeps the order
    /// the same in both languages, which the label would not.
    ///
    /// Several actions share `running-generic` — `Action::running_key` has a
    /// deliberate wildcard — and they are one line, not three.
    pub fn announcements(&self) -> Vec<&'static str> {
        let mut keys: Vec<&'static str> = self
            .running
            .iter()
            .map(|(_, action)| action.running_key())
            .collect();
        keys.sort_unstable();
        keys.dedup();
        keys
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn worktree(path: &str) -> Option<PathBuf> {
        Some(PathBuf::from(path))
    }

    #[test]
    fn a_write_stops_running_when_its_own_answer_comes_back() {
        let mut flight = InFlight::default();
        flight.start(worktree("/p/a"), Action::Push);
        assert!(flight.is_running(Some(Path::new("/p/a")), Action::Push));
        // Another worktree's answer, and another action's, leave it alone.
        flight.finish(&worktree("/p/b"), Action::Push);
        flight.finish(&worktree("/p/a"), Action::Pull);
        assert!(flight.is_running(Some(Path::new("/p/a")), Action::Push));
        flight.finish(&worktree("/p/a"), Action::Push);
        assert!(!flight.is_running(Some(Path::new("/p/a")), Action::Push));
        assert!(flight.is_empty());
    }

    #[test]
    fn the_same_write_on_two_worktrees_spins_two_buttons() {
        let mut flight = InFlight::default();
        flight.start(worktree("/p/a"), Action::Fetch);
        flight.start(worktree("/p/b"), Action::Fetch);
        flight.finish(&worktree("/p/a"), Action::Fetch);
        assert!(!flight.is_running(Some(Path::new("/p/a")), Action::Fetch));
        assert!(flight.is_running(Some(Path::new("/p/b")), Action::Fetch));
    }

    #[test]
    fn wt_up_names_no_worktree_so_it_carries_its_own() {
        let mut flight = InFlight::default();
        flight.start(None, Action::WtUp);
        flight.set_wt_target(PathBuf::from("/p/a"));
        assert_eq!(flight.wt_target(), Some(Path::new("/p/a")));
        // The answer names nothing: it is the action alone that lets the badge go.
        flight.finish(&None, Action::WtUp);
        assert!(flight.wt_target().is_none());
        assert!(flight.is_empty());
    }

    #[test]
    fn a_dead_server_leaves_nothing_spinning() {
        let mut flight = InFlight::default();
        flight.start(worktree("/p/a"), Action::Push);
        flight.start(None, Action::WtDown);
        flight.set_wt_target(PathBuf::from("/p/a"));
        flight.clear();
        assert!(flight.is_empty());
        assert!(flight.wt_target().is_none());
    }

    #[test]
    fn the_status_bar_reads_the_same_words_in_the_same_order() {
        let mut flight = InFlight::default();
        flight.start(worktree("/p/a"), Action::Push);
        flight.start(worktree("/p/b"), Action::Fetch);
        // Two worktrees, one word: the bar names operations, it does not count
        // them.
        flight.start(worktree("/p/c"), Action::Fetch);
        let names = flight.announcements();
        assert_eq!(names, vec!["running-fetch", "running-push"]);
        // And it is the same on the next frame, whatever the set's order.
        assert_eq!(flight.announcements(), names);
    }

    #[test]
    fn the_unnamed_actions_share_one_line() {
        let mut flight = InFlight::default();
        flight.start(worktree("/p/a"), Action::Stage);
        flight.start(worktree("/p/a"), Action::Unstage);
        assert_eq!(flight.announcements(), vec!["running-generic"]);
    }
}
