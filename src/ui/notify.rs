//! What an operation's outcome says.
//!
//! Everything a gesture has to say is a **balloon, top right**, and this module
//! is what turns an answer into one. There used to be two surfaces — one line
//! in the status bar, a balloon beside it for what had to be read — and the bar
//! lost: the last message of a window belongs where the eye is, which is the
//! panel one has just clicked in, and the bottom edge is precisely where it is
//! not. What the bar held well, it held for nobody.
//!
//! Two things are decided here, and both have a case behind them:
//!
//! - **What it is called.** A success is named after what was attempted, a
//!   failure by one key for all of them — what failed is in the body, git
//!   naming the operation itself (`error: failed to push some refs`).
//! - **How long it stays.** A success fades; a failure waits to be dismissed.
//!   That one is the view's to apply, and `Level` is what says it.
//!
//! No gpui here, as in `inflight.rs` and `notes.rs`: what an outcome reads as
//! is a decision, and a decision is a thing one tests rather than watches.

use crate::runtime::Action;

/// The title every failure carries.
pub const FAILED: &str = "notify-failed";

/// How an outcome reads.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Level {
    Success,
    Error,
}

/// An outcome, as the balloon wants it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Notice {
    /// The i18n key naming the outcome, resolved by the view.
    pub title: &'static str,
    /// What git said. Empty when it said nothing worth reading, and the
    /// balloon is then its title alone — which is the whole message of a stage
    /// or a checkout.
    pub body: String,
    /// The lines the body does not carry. Nought almost always; the view says
    /// so when it is not, and that is what makes the cut below honest.
    pub hidden: usize,
    pub level: Level,
}

/// How many lines a balloon carries.
///
/// A balloon is a message, not a window: a rebase that touches two hundred
/// files answered with two hundred lines, and the balloon then covered the
/// review it was reporting on — with no way to scroll it and nothing under it
/// reachable. The number is chosen so that the answers one actually reads pass
/// whole: a pull naming its files, a push naming its refs, a failure and its
/// two lines of git.
const MAX_LINES: usize = 14;

/// The balloon an outcome deserves.
///
/// **Every outcome gets one.** It was not always so: while the bar carried a
/// line for each, a balloon was reserved for what could not fit in it — more
/// than one line, or a round trip whose answer is read after the fact. With the
/// bar gone, that rule became a way of saying nothing at all, and a gesture
/// that answers nothing reads as a gesture that did nothing.
pub fn notice(action: Action, output: &str, level: Level) -> Notice {
    let trimmed = output.trim();
    let lines = trimmed.lines().count();
    // **The first lines and not the last.** Git puts what it was doing at the
    // top — `error: failed to push some refs` — and the tail of a long answer
    // is a list, of which one line is as good as another.
    let body = match lines > MAX_LINES {
        true => trimmed
            .lines()
            .take(MAX_LINES)
            .collect::<Vec<_>>()
            .join("\n"),
        false => trimmed.to_string(),
    };
    Notice {
        title: match level {
            Level::Success => action.success_key(),
            // One key for every failure, rather than a third family of thirty
            // mirroring `success_key`: what failed is in the body, git naming
            // the operation itself — `error: failed to push some refs`.
            Level::Error => FAILED,
        },
        body,
        hidden: lines.saturating_sub(MAX_LINES),
        level,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A pull answers with a file per line, and all of it is read: the balloon
    /// is the surface that can hold it, which is why it exists.
    #[test]
    fn an_answer_is_carried_whole() {
        let pull = "Updating 9ae0f50b..55e38b36\nFast-forward\n app/Config.php | 6 +\n";
        let balloon = notice(Action::Pull, pull, Level::Success);
        assert_eq!(balloon.title, Action::Pull.success_key());
        assert!(balloon.body.contains("app/Config.php"));
        assert!(balloon.body.starts_with("Updating"), "{}", balloon.body);
    }

    /// **A gesture that answers nothing still says it happened.** Git says
    /// nothing at all about a `git add`; the name of what was attempted is the
    /// whole message, and while the status bar carried it this earned no
    /// balloon and would now be silence.
    #[test]
    fn a_gesture_with_nothing_to_say_still_gets_a_balloon() {
        let staged = notice(Action::Stage, "", Level::Success);
        assert_eq!(staged.title, Action::Stage.success_key());
        assert!(staged.body.is_empty());
        assert_eq!(staged.level, Level::Success);
    }

    /// A failure is what one comes back to read, and it is the level that keeps
    /// it on screen until it is dismissed.
    #[test]
    fn a_failure_is_named_by_one_key_and_says_why_in_its_body() {
        let balloon = notice(Action::Push, "  no upstream branch\n", Level::Error);
        assert_eq!(balloon.level, Level::Error);
        assert_eq!(balloon.title, FAILED);
        assert_eq!(balloon.body, "no upstream branch");
        assert_eq!(balloon.hidden, 0);
    }

    /// A long answer is cut, and **says that it is**: a balloon taller than the
    /// window covers the very thing it reports on, and one that quietly dropped
    /// its tail would be worse than one that did not open.
    #[test]
    fn a_long_answer_is_cut_and_says_how_much() {
        let long: String = (0..40)
            .map(|n| format!(" app/File{n}.php | 2 +\n"))
            .collect();
        let balloon = notice(Action::Pull, &long, Level::Success);
        assert_eq!(balloon.body.lines().count(), MAX_LINES);
        assert_eq!(balloon.hidden, 40 - MAX_LINES);
        // The head, which is where git says what it was doing.
        assert!(
            balloon.body.starts_with("app/File0.php"),
            "{}",
            balloon.body
        );
    }

    /// Exactly the limit is not a cut: a balloon that said "and 0 more" would
    /// be reporting on itself.
    #[test]
    fn what_fits_is_carried_whole() {
        let fits: String = (0..MAX_LINES).map(|n| format!("line {n}\n")).collect();
        let balloon = notice(Action::Pull, &fits, Level::Success);
        assert_eq!(balloon.body.lines().count(), MAX_LINES);
        assert_eq!(balloon.hidden, 0);
    }
}
