//! What an operation's outcome says, and where it says it.
//!
//! The status bar was the only place anything was reported, and it is the
//! wrong one as soon as there is something to *read*: a `git pull` answers
//! with `Updating 9ae0f50b..55e38b36`, `Fast-forward`, and one line per file
//! changed. Poured into a bar one line high, that text does not truncate — it
//! wraps, and the forty lines paint themselves over the window from the bar
//! upwards, on top of whatever was there.
//!
//! Two places, therefore, and the split is the whole point of this module:
//!
//! - **The bar keeps one line**, always, whatever arrives. It says *what just
//!   happened* and it is glanced at, not read.
//! - **A balloon carries what has to be read** — the file list of a pull, the
//!   refs a push updated, the reason a merge refused. It is PhpStorm's
//!   convention, it stacks, it can be dismissed, and it does not fight the
//!   window for room.
//!
//! No gpui here, as in `inflight.rs` and `notes.rs`: what deserves a balloon
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

/// An outcome as the two surfaces want it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Notice {
    /// The i18n key naming the outcome, resolved by the view.
    pub title: &'static str,
    /// What git said, whole. Empty when it said nothing worth reading.
    pub body: String,
    pub level: Level,
}

/// The single line the status bar shows.
///
/// The **first** line and not the last: git leads with what it did — `Updating
/// 9ae0f50b..55e38b36`, `To github.com:…` — and the tail is the detail. An
/// output that says nothing falls back to the action's own success label.
pub fn headline(output: &str) -> Option<String> {
    output
        .lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .map(str::to_string)
}

/// The balloon an outcome deserves, if it deserves one.
///
/// Three rules, and each has a case behind it:
///
/// - **Every failure gets one.** The bar is overwritten by the next message,
///   and a `git push` refused for want of an upstream is precisely what one
///   comes back to read a minute later.
/// - **An output with more than one line gets one**: that is the definition of
///   "there is something to read here", and it is what a pull produces.
/// - **Nothing else does.** A `Stage` that finishes in ten milliseconds with
///   `Staged` to say for itself would be a balloon nobody has time to read,
///   and one that pushes the useful ones off the screen.
pub fn notice(action: Action, output: &str, level: Level) -> Option<Notice> {
    let body = output.trim();
    if level == Level::Success && body.lines().filter(|l| !l.trim().is_empty()).count() < 2 {
        return None;
    }
    Some(Notice {
        title: match level {
            Level::Success => action.success_key(),
            // One key for every failure, rather than a third family of thirty
            // mirroring `success_key`: what failed is in the body, git naming
            // the operation itself — `error: failed to push some refs`.
            Level::Error => FAILED,
        },
        body: body.to_string(),
        level,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_bar_keeps_the_first_line_and_only_that() {
        let pull = "Updating 9ae0f50b..55e38b36\nFast-forward\n app/Config.php | 6 +\n";
        assert_eq!(
            headline(pull).as_deref(),
            Some("Updating 9ae0f50b..55e38b36")
        );
        // This is the whole defect: a bar one line high cannot hold this.
        assert!(!headline(pull).unwrap().contains('\n'));
    }

    #[test]
    fn a_leading_blank_line_is_not_the_headline() {
        assert_eq!(
            headline("\n\n  Already up to date.\n").as_deref(),
            Some("Already up to date.")
        );
        assert_eq!(headline("   \n"), None);
    }

    /// A pull that changed files has a list to show; one that changed nothing
    /// says so in a line, and a line belongs in the bar.
    #[test]
    fn only_an_outcome_with_something_to_read_earns_a_balloon() {
        let pull = "Updating 9ae0f50b..55e38b36\nFast-forward\n app/Config.php | 6 +\n";
        let balloon = notice(Action::Pull, pull, Level::Success).expect("something to read");
        assert_eq!(balloon.title, Action::Pull.success_key());
        assert!(balloon.body.contains("app/Config.php"));

        assert_eq!(
            notice(Action::Pull, "Already up to date.", Level::Success),
            None
        );
        assert_eq!(notice(Action::Stage, "", Level::Success), None);
    }

    /// A failure is what one comes back to read, and the bar is overwritten by
    /// the next message that goes through it.
    #[test]
    fn a_failure_always_earns_one_however_short() {
        let balloon = notice(Action::Push, "no upstream branch", Level::Error)
            .expect("a failure is always worth a balloon");
        assert_eq!(balloon.level, Level::Error);
        assert_eq!(balloon.title, FAILED);
    }
}
