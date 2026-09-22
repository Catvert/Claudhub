//! The comparison base selector.
//!
//! An entry is more than a name. Choosing between `dev`, `origin/dev` and
//! `wt/dev-2` means knowing which moved last and what it carries — otherwise
//! you have to leave Claudhub and ask git before you can click. That
//! information is already read along with the branch list: showing it costs no
//! extra command.
//!
//! It is shown in the entry itself rather than in a tooltip: a list is scanned
//! by eye, and information that requires stopping on each row to reveal it does
//! not help you compare.
//!
//! The first entry, once a review point has been set, is no branch: it is
//! "since my last review" (`git::snapshot`), and it says when the point was set.

use gpui_kit::component::{h_flex, select::SelectItem, v_flex, ActiveTheme};
use gpui_kit::{div, prelude::*, px, App, IntoElement, SharedString, Window};

use crate::git::{Branch, BranchKind};
use crate::tr;

/// The value of the "since my last review" entry.
///
/// A value no branch can have, so the selector's one list holds both: git
/// refuses a `:` anywhere in a ref name, where any word we picked could be
/// somebody's branch. (Not `@`: that one alone is refused as a whole ref, but
/// `refs/heads/@` is a perfectly good branch.)
pub const SINCE_REVIEW: &str = ":review";

/// A branch as the selector offers it — or, first in the list when one has
/// been set, the review point.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BaseChoice {
    pub name: SharedString,
    pub subject: SharedString,
    pub author: SharedString,
    pub date: SharedString,
    pub remote: bool,
    /// True when this is the branch checked out in the worktree being looked
    /// at: comparing it to itself would show nothing.
    pub is_head: bool,
    /// The "since my last review" entry, which is no branch.
    ///
    /// **In the base selector and not a third panel**: it answers the question
    /// the selector asks — what the branch review compares against — and one
    /// more panel would be one more list of the same files to keep in step.
    pub review: bool,
}

/// How long ago something was, in the words the window uses elsewhere.
///
/// Pure, for the tests: the words themselves are `tr!`'s, and a clock read
/// inside would make the answer depend on when the test runs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Ago {
    JustNow,
    Minutes(i64),
    Hours(i64),
    /// A day or more: the moment itself says more than a count of days.
    Earlier,
}

pub fn ago(now: i64, at: i64) -> Ago {
    let minutes = (now - at) / 60;
    match minutes {
        // Ahead of us is a clock out of step: "just now" is the honest reading.
        i64::MIN..=0 => Ago::JustNow,
        1..=59 => Ago::Minutes(minutes),
        60..=1439 => Ago::Hours(minutes / 60),
        _ => Ago::Earlier,
    }
}

impl BaseChoice {
    /// The review point's entry, saying when it was set.
    pub fn since_review(at: i64, now: i64) -> Self {
        let when = match ago(now, at) {
            Ago::JustNow => tr!("when-just-now").to_string(),
            Ago::Minutes(n) => tr!("when-minutes", { n: n }).to_string(),
            Ago::Hours(n) => tr!("when-hours", { n: n }).to_string(),
            Ago::Earlier => chrono::DateTime::from_timestamp(at, 0)
                .map(|at| {
                    at.with_timezone(&chrono::Local)
                        .format("%Y-%m-%d %H:%M")
                        .to_string()
                })
                .unwrap_or_default(),
        };
        Self {
            name: SharedString::from(SINCE_REVIEW),
            subject: tr!("range-since-review-at", { when: when }),
            author: SharedString::default(),
            date: SharedString::default(),
            remote: false,
            is_head: false,
            review: true,
        }
    }

    /// `worktree` is the checkout being looked at: "here" is its branch, not
    /// the one git marks as HEAD where the list was read (the main worktree).
    pub fn of(branch: &Branch, worktree: &std::path::Path) -> Self {
        Self {
            name: SharedString::from(branch.name.clone()),
            subject: SharedString::from(branch.subject.clone()),
            author: SharedString::from(branch.author.clone()),
            date: SharedString::from(branch.date.clone()),
            remote: branch.kind == BranchKind::Remote,
            is_head: branch.is_head_in(worktree),
            review: false,
        }
    }

    /// The second line: what the branch carries, and since when.
    ///
    /// Empty pieces are dropped rather than replaced by a dash: a freshly
    /// cloned repository has no relative author to show, and punctuation around
    /// nothing reads like lost data.
    pub fn detail(&self) -> String {
        [
            self.subject.as_ref(),
            self.author.as_ref(),
            self.date.as_ref(),
        ]
        .into_iter()
        .filter(|part| !part.trim().is_empty())
        .collect::<Vec<_>>()
        .join(" · ")
    }
}

impl SelectItem for BaseChoice {
    type Value = SharedString;

    /// What the closed selector reads, after "Base:" — the name of the base,
    /// or for the review point the words for it: its value would say nothing.
    fn title(&self) -> SharedString {
        if self.review {
            tr!("range-since-review")
        } else {
            self.name.clone()
        }
    }

    fn value(&self) -> &Self::Value {
        &self.name
    }

    fn render(&self, _: &mut Window, cx: &mut App) -> impl IntoElement {
        let muted = cx.theme().muted_foreground;
        let detail = self.detail();
        v_flex()
            .w_full()
            .min_w_0()
            .gap_0p5()
            .child(
                h_flex()
                    .w_full()
                    .min_w_0()
                    .gap_1()
                    .items_center()
                    .child(div().flex_1().min_w_0().truncate().child(self.title()))
                    .when(self.remote, |el| el.child(tag(tr!("branch-remote"), cx)))
                    .when(self.is_head, |el| el.child(tag(tr!("branch-here"), cx))),
            )
            .when(!detail.is_empty(), |el| {
                el.child(
                    div()
                        .w_full()
                        .min_w_0()
                        .truncate()
                        .text_xs()
                        .text_color(muted)
                        .child(detail),
                )
            })
    }
}

fn tag(label: SharedString, cx: &App) -> impl IntoElement {
    div()
        .flex_none()
        .px_1()
        .rounded(cx.theme().radius)
        .bg(cx.theme().secondary)
        .text_xs()
        .text_color(cx.theme().muted_foreground)
        .child(label)
}

/// Menu width. Two lines of text need room; below this width the commit
/// subject is truncated to the point of saying nothing.
pub const MENU_WIDTH: gpui_kit::Pixels = px(420.);

#[cfg(test)]
mod tests {
    use super::*;

    fn choice(subject: &str, author: &str, date: &str) -> BaseChoice {
        BaseChoice {
            name: "dev".into(),
            subject: subject.to_string().into(),
            author: author.to_string().into(),
            date: date.to_string().into(),
            remote: false,
            is_head: false,
            review: false,
        }
    }

    #[test]
    fn a_review_point_reads_as_how_long_ago_it_was_set() {
        let now = 1_790_000_000;
        assert_eq!(ago(now, now), Ago::JustNow);
        assert_eq!(ago(now, now - 30), Ago::JustNow);
        // A clock out of step does not say "in five minutes".
        assert_eq!(ago(now, now + 300), Ago::JustNow);
        assert_eq!(ago(now, now - 5 * 60), Ago::Minutes(5));
        assert_eq!(ago(now, now - 3 * 3600 - 60), Ago::Hours(3));
        assert_eq!(ago(now, now - 2 * 86_400), Ago::Earlier);
    }

    /// No branch can take the entry's value: git refuses a `:` in a ref name.
    #[test]
    fn the_review_entry_cannot_be_a_branch() {
        let out = std::process::Command::new("git")
            .args(["check-ref-format", "--branch", SINCE_REVIEW])
            .output()
            .expect("git");
        assert!(!out.status.success());
    }

    #[test]
    fn the_detail_line_joins_what_it_has() {
        assert_eq!(
            choice("Fix the rendering", "Zoé", "2 hours ago").detail(),
            "Fix the rendering · Zoé · 2 hours ago"
        );
    }

    #[test]
    fn an_empty_part_does_not_leave_its_punctuation_behind() {
        // A branch with no readable author must not produce "subject ·  · yesterday".
        assert_eq!(
            choice("Subject", "", "yesterday").detail(),
            "Subject · yesterday"
        );
        assert_eq!(choice("", "", "").detail(), "");
    }
}
