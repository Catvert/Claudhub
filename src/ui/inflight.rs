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

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use crate::runtime::{Action, Ticket};

/// What one operation under way is about: what a button asks after. The
/// worktree is an `Option` because `wt up` and `wt down` name none — `wt`
/// works from the main repository.
type Doing = (Option<PathBuf>, Action);

/// The operations under way, **by ticket**.
///
/// The ticket is exactly what comes back: the worker stamps its command's on
/// the `Done` or `Failed` it answers with, whatever worktree and action that
/// answer names. Keyed by the pair instead, a write that panicked past its own
/// net — which answers with neither — left its button spinning for good, and
/// a write sent from elsewhere under the same pair let another's go.
#[derive(Debug, Default)]
pub struct InFlight {
    running: HashMap<Ticket, Doing>,
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
    pub fn start(&mut self, ticket: Ticket, worktree: Option<PathBuf>, action: Action) {
        self.running.insert(ticket, (worktree, action));
    }

    /// Takes the ticket back, and lets go of the `wt` worktree when it was one
    /// of those two.
    pub fn finish(&mut self, ticket: Ticket) {
        let Some((_, action)) = self.running.remove(&ticket) else {
            return;
        };
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
    ///
    /// Scanned rather than looked up: building the key would copy a path, and
    /// this is read several times per frame for a set that holds a handful of
    /// entries at most — one per write a hand has started.
    pub fn is_running(&self, worktree: Option<&Path>, action: Action) -> bool {
        self.running
            .values()
            .any(|(path, running)| *running == action && path.as_deref() == worktree)
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
    /// the translated label: a `HashMap` iterates in a different order on every
    /// frame, and the words would dance. Sorting on the key also keeps the order
    /// the same in both languages, which the label would not.
    ///
    /// Several actions share `running-generic` — `Action::running_key` has a
    /// deliberate wildcard — and they are one line, not three.
    pub fn announcements(&self) -> Vec<&'static str> {
        let mut keys: Vec<&'static str> = self
            .running
            .values()
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
        flight.start(Ticket(1), worktree("/p/a"), Action::Push);
        assert!(flight.is_running(Some(Path::new("/p/a")), Action::Push));
        // Another command's answer leaves it alone.
        flight.finish(Ticket(2));
        assert!(flight.is_running(Some(Path::new("/p/a")), Action::Push));
        flight.finish(Ticket(1));
        assert!(!flight.is_running(Some(Path::new("/p/a")), Action::Push));
        assert!(flight.is_empty());
    }

    #[test]
    fn the_same_write_on_two_worktrees_spins_two_buttons() {
        let mut flight = InFlight::default();
        flight.start(Ticket(1), worktree("/p/a"), Action::Fetch);
        flight.start(Ticket(2), worktree("/p/b"), Action::Fetch);
        flight.finish(Ticket(1));
        assert!(!flight.is_running(Some(Path::new("/p/a")), Action::Fetch));
        assert!(flight.is_running(Some(Path::new("/p/b")), Action::Fetch));
    }

    /// Two of the same write on the same worktree are two tickets: the first
    /// answer does not turn off the button the second still spins.
    #[test]
    fn the_same_write_twice_spins_until_both_are_back() {
        let mut flight = InFlight::default();
        flight.start(Ticket(1), None, Action::Branch);
        flight.start(Ticket(2), None, Action::Branch);
        flight.finish(Ticket(1));
        assert!(flight.is_running(None, Action::Branch));
        flight.finish(Ticket(2));
        assert!(flight.is_empty());
    }

    #[test]
    fn wt_up_names_no_worktree_so_it_carries_its_own() {
        let mut flight = InFlight::default();
        flight.start(Ticket(1), None, Action::WtUp);
        flight.set_wt_target(PathBuf::from("/p/a"));
        assert_eq!(flight.wt_target(), Some(Path::new("/p/a")));
        // Something else coming back leaves the badge where it is.
        flight.finish(Ticket(2));
        assert_eq!(flight.wt_target(), Some(Path::new("/p/a")));
        flight.finish(Ticket(1));
        assert!(flight.wt_target().is_none());
        assert!(flight.is_empty());
    }

    #[test]
    fn a_dead_server_leaves_nothing_spinning() {
        let mut flight = InFlight::default();
        flight.start(Ticket(1), worktree("/p/a"), Action::Push);
        flight.start(Ticket(2), None, Action::WtDown);
        flight.set_wt_target(PathBuf::from("/p/a"));
        flight.clear();
        assert!(flight.is_empty());
        assert!(flight.wt_target().is_none());
    }

    #[test]
    fn the_status_bar_reads_the_same_words_in_the_same_order() {
        let mut flight = InFlight::default();
        flight.start(Ticket(1), worktree("/p/a"), Action::Push);
        flight.start(Ticket(2), worktree("/p/b"), Action::Fetch);
        // Two worktrees, one word: the bar names operations, it does not count
        // them.
        flight.start(Ticket(3), worktree("/p/c"), Action::Fetch);
        let names = flight.announcements();
        assert_eq!(names, vec!["running-fetch", "running-push"]);
        // And it is the same on the next frame, whatever the map's order.
        assert_eq!(flight.announcements(), names);
    }

    #[test]
    fn the_unnamed_actions_share_one_line() {
        let mut flight = InFlight::default();
        flight.start(Ticket(1), worktree("/p/a"), Action::Stage);
        flight.start(Ticket(2), worktree("/p/a"), Action::Unstage);
        assert_eq!(flight.announcements(), vec!["running-generic"]);
    }
}
