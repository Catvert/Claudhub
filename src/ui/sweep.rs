//! What the periodic sweep asks for, and when it holds back — pure, tested.
//!
//! The sweep beats every two seconds whatever the workers are doing. Two
//! rules keep it from outrunning them:
//!
//! - **A periodic reading is not sent again before its answer is back.**
//!   Behind a slow scan on the background worker, the next ones would pile
//!   up and then all arrive stale, one after the other. The summaries, the
//!   cards and `wt` ran unchecked every ten seconds: a pass that took longer
//!   — a status over an agent's growing diff, a slow probe — grew the queue
//!   by one pass every ten seconds, and everything else on the worker
//!   answered later and later the longer the window stayed open. An answer
//!   that never
//!   comes — the server lost, a command dropped before the handshake — does
//!   not hold the reading back for ever: past `GIVE_UP`, it is asked again,
//!   and `ServerLost` forgets everything at once.
//! - **The home cards read what is on show at every pass, the rest one pass
//!   in `OUTLINES_ALL_EVERY`** (`outlines_due`): four processes and more per
//!   worktree, on the single background worker, for cards nobody sees. The
//!   context sheet reads the others' too, and ages by a minute at most.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

/// How long an answer is waited for before the reading is asked again.
const GIVE_UP: Duration = Duration::from_secs(30);

/// One summary pass in this many reads every worktree's card, and not only
/// those on show: every minute, at ten seconds a pass.
const OUTLINES_ALL_EVERY: u32 = 6;

/// A periodic reading of the whole window, answered by a single event.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) enum Reading {
    /// `ScanAgents`, every two seconds.
    Agents,
    /// `LoadSummaries`, every ten.
    Summaries,
    /// `LoadOutlines`, with the summaries while the home screen is up.
    Outlines,
    /// `WtScan`, with the summaries.
    Wt,
}

/// The periodic readings on their way, and the count of the passes.
#[derive(Debug, Default)]
pub(crate) struct Sweep {
    /// The readings asked for, since when.
    asked: HashMap<Reading, Instant>,
    /// `ReadCanvas`, by worktree, since when.
    canvas: HashMap<PathBuf, Instant>,
    /// The summary passes made — see `outlines_due`.
    pass: u32,
}

impl Sweep {
    /// Whether a reading may be sent; if so, it is now asked.
    pub fn ask(&mut self, reading: Reading, now: Instant) -> bool {
        let free = free(self.asked.get(&reading).copied(), now);
        if free {
            self.asked.insert(reading, now);
        }
        free
    }

    pub fn answered(&mut self, reading: Reading) {
        self.asked.remove(&reading);
    }

    /// Whether a worktree's node files may be read by the sweep.
    pub fn canvas_free(&self, worktree: &Path, now: Instant) -> bool {
        free(self.canvas.get(worktree).copied(), now)
    }

    /// A worktree's node files asked for — by the sweep or by anything else.
    pub fn canvas_asked(&mut self, worktree: PathBuf, now: Instant) {
        self.canvas.insert(worktree, now);
    }

    pub fn canvas_answered(&mut self, worktree: &Path) {
        self.canvas.remove(worktree);
    }

    /// The server is gone: what it was asked will not come back.
    pub fn forget_asked(&mut self) {
        self.asked.clear();
        self.canvas.clear();
    }

    /// The worktrees whose home card this summary pass reads, and the pass
    /// counted.
    pub fn outlines(&mut self, all: &[PathBuf], shown: &[PathBuf]) -> Vec<PathBuf> {
        let due = outlines_due(all, shown, self.pass);
        self.pass = self.pass.wrapping_add(1);
        due
    }
}

/// Whether a reading asked at `asked` no longer stands in the way.
fn free(asked: Option<Instant>, now: Instant) -> bool {
    asked.is_none_or(|at| now.saturating_duration_since(at) >= GIVE_UP)
}

/// Every worktree once in `OUTLINES_ALL_EVERY` passes — the first one
/// included — and those on show at every pass.
fn outlines_due(all: &[PathBuf], shown: &[PathBuf], pass: u32) -> Vec<PathBuf> {
    let everyone = pass.is_multiple_of(OUTLINES_ALL_EVERY);
    all.iter()
        .filter(|worktree| everyone || shown.contains(worktree))
        .cloned()
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn paths(names: &[&str]) -> Vec<PathBuf> {
        names.iter().map(PathBuf::from).collect()
    }

    /// Asked, a scan waits for its answer — or for the ceiling, when the
    /// answer was lost — and the lost server frees everything.
    #[test]
    fn a_reading_waits_for_its_answer_or_the_ceiling() {
        let now = Instant::now();
        let mut sweep = Sweep::default();
        assert!(sweep.ask(Reading::Agents, now));
        assert!(!sweep.ask(Reading::Agents, now + Duration::from_secs(2)));
        // Each reading waits for its own answer, not another's.
        assert!(sweep.ask(Reading::Summaries, now + Duration::from_secs(2)));
        sweep.answered(Reading::Agents);
        assert!(sweep.ask(Reading::Agents, now + Duration::from_secs(4)));
        assert!(sweep.ask(Reading::Agents, now + Duration::from_secs(4) + GIVE_UP));
        assert!(!sweep.ask(Reading::Summaries, now + Duration::from_secs(12)));

        let a = PathBuf::from("/a");
        assert!(sweep.canvas_free(&a, now));
        sweep.canvas_asked(a.clone(), now);
        assert!(!sweep.canvas_free(&a, now + Duration::from_secs(2)));
        assert!(sweep.canvas_free(Path::new("/b"), now));
        sweep.canvas_answered(&a);
        assert!(sweep.canvas_free(&a, now));

        sweep.canvas_asked(a.clone(), now);
        sweep.forget_asked();
        assert!(sweep.canvas_free(&a, now));
        assert!(sweep.ask(Reading::Agents, now));
        assert!(sweep.ask(Reading::Summaries, now));
    }

    /// Those on show at every pass, the others once a minute — starting
    /// with the first pass, which has nothing older to show.
    #[test]
    fn the_cards_on_show_are_read_at_every_pass() {
        let all = paths(&["/a", "/b", "/c"]);
        let shown = paths(&["/b"]);
        let mut sweep = Sweep::default();
        assert_eq!(sweep.outlines(&all, &shown), all);
        for _ in 1..OUTLINES_ALL_EVERY {
            assert_eq!(sweep.outlines(&all, &shown), shown);
        }
        assert_eq!(sweep.outlines(&all, &shown), all);
        // Something on show that is not open is not asked for.
        assert!(outlines_due(&all, &paths(&["/z"]), 1).is_empty());
    }
}
