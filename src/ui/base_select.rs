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
//!
//! **The tags and the periods are offered here too** (`Compare`): they answer
//! the same question — from where does the review read — and they are kept
//! apart from the base, which the pull request and the merge go on using. A
//! tag is an entry among the branches; a period is picked in the calendar
//! beside the selector, and while it is the one shown it heads the list.
//! « Since the last tag » heads the tags: the choice of a release cycle, which
//! follows the next tag without being chosen again (`DiffRange::LastTag`).

use gpui_kit::component::{h_flex, select::SelectItem, v_flex, ActiveTheme};
use gpui_kit::{div, prelude::*, px, App, IntoElement, SharedString, Window};

use crate::git::{Branch, BranchKind, DiffRange, Tag};
use crate::tr;

/// The value of the "since my last review" entry.
///
/// A value no branch can have, so the selector's one list holds both: git
/// refuses a `:` anywhere in a ref name, where any word we picked could be
/// somebody's branch. (Not `@`: that one alone is refused as a whole ref, but
/// `refs/heads/@` is a perfectly good branch.)
pub const SINCE_REVIEW: &str = ":review";

/// The prefix of a tag's value. A `:` for the reason of `SINCE_REVIEW`: no
/// branch can be named like it, and no tag either.
const TAG_PREFIX: &str = ":tag:";

/// The value of the period's entry.
pub const PERIOD: &str = ":period";

/// The value of the « since the last tag » entry.
const LAST_TAG: &str = ":last-tag";

/// What the branch review reads from when it is neither the base nor the
/// review point.
///
/// **Not the base**: the base is also what the pull request targets and what
/// the merge goes into, and a tag or a week is neither. Kept beside it, and
/// for its reason: a choice of the user's, per worktree.
#[derive(Debug, Clone, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub enum Compare {
    /// From a tag up to HEAD.
    Tag(String),
    /// A period of days, `YYYY-MM-DD`; no end is "up to today".
    Dates { from: String, to: Option<String> },
    /// From the nearest tag behind HEAD, whichever it is at each reading.
    LastTag,
}

impl Compare {
    /// The range the review panel lists.
    ///
    /// A tag is a base like a branch — three dots, what was written since —
    /// spelled in full: `v1` alone would be the branch if one bore the name.
    pub fn range(&self) -> DiffRange {
        match self {
            Self::Tag(tag) => DiffRange::Branch {
                base: format!("refs/tags/{tag}"),
            },
            Self::Dates { from, to } => DiffRange::Dates {
                from: from.clone(),
                to: to.clone(),
            },
            Self::LastTag => DiffRange::LastTag,
        }
    }

    /// Its value in the selector.
    pub fn value(&self) -> String {
        match self {
            Self::Tag(tag) => format!("{TAG_PREFIX}{tag}"),
            Self::Dates { .. } => PERIOD.to_string(),
            Self::LastTag => LAST_TAG.to_string(),
        }
    }

    /// The tag a selector value names — or the last one —, if it names one.
    pub fn tag_of(value: &str) -> Option<Self> {
        if value == LAST_TAG {
            return Some(Self::LastTag);
        }
        value
            .strip_prefix(TAG_PREFIX)
            .filter(|tag| !tag.is_empty())
            .map(|tag| Self::Tag(tag.to_string()))
    }

    /// The days picked in the calendar, as a period.
    ///
    /// An end on today or after is no end: "the last seven days" picked on
    /// Monday still means up to now on Tuesday, and today's commits are only
    /// HEAD's anyway. The two ends come in either order.
    pub fn period(
        start: chrono::NaiveDate,
        end: chrono::NaiveDate,
        today: chrono::NaiveDate,
    ) -> Self {
        let (start, end) = (start.min(end), start.max(end));
        Self::Dates {
            from: start.format("%Y-%m-%d").to_string(),
            to: (end < today).then(|| end.format("%Y-%m-%d").to_string()),
        }
    }

    /// The words for it: what the closed selector and the boards read.
    pub fn words(&self) -> SharedString {
        match self {
            Self::Tag(tag) => tr!("range-since-tag", { tag: tag }),
            Self::LastTag => tr!("range-since-last-tag"),
            Self::Dates { from, to: None } => tr!("range-dates-since", { from: from }),
            Self::Dates { from, to: Some(to) } if from == to => {
                tr!("range-dates-on", { day: from })
            }
            Self::Dates { from, to: Some(to) } => {
                tr!("range-dates-between", { from: from, to: to })
            }
        }
    }
}

/// What an entry of the selector is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Branch,
    /// "Since my last review", which is no branch.
    ///
    /// **In the base selector and not a third panel**: it answers the question
    /// the selector asks — what the branch review compares against — and one
    /// more panel would be one more list of the same files to keep in step.
    Review,
    Tag,
    /// The period picked in the calendar, while it is the one shown.
    Period,
}

/// A branch as the selector offers it — or, first in the list when one has
/// been set, the review point; the period when one is shown; the tags.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BaseChoice {
    /// The value: the branch's name, or a `:` word for the rest.
    pub name: SharedString,
    /// What is read instead of the value, for what is no branch.
    pub label: Option<SharedString>,
    pub subject: SharedString,
    pub author: SharedString,
    pub date: SharedString,
    pub remote: bool,
    /// True when this is the branch checked out in the worktree being looked
    /// at: comparing it to itself would show nothing.
    pub is_head: bool,
    pub kind: Kind,
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
            label: Some(tr!("range-since-review")),
            subject: tr!("range-since-review-at", { when: when }),
            author: SharedString::default(),
            date: SharedString::default(),
            remote: false,
            is_head: false,
            kind: Kind::Review,
        }
    }

    /// The period's entry.
    pub fn period(period: &Compare) -> Self {
        Self {
            name: SharedString::from(PERIOD),
            label: Some(period.words()),
            subject: tr!("range-dates-detail"),
            author: SharedString::default(),
            date: SharedString::default(),
            remote: false,
            is_head: false,
            kind: Kind::Period,
        }
    }

    /// « Since the last tag », at the head of the tags.
    pub fn last_tag() -> Self {
        Self {
            name: SharedString::from(LAST_TAG),
            label: Some(Compare::LastTag.words()),
            subject: tr!("range-last-tag-detail"),
            author: SharedString::default(),
            date: SharedString::default(),
            remote: false,
            is_head: false,
            kind: Kind::Tag,
        }
    }

    pub fn tag(tag: &Tag) -> Self {
        Self {
            name: SharedString::from(Compare::Tag(tag.name.clone()).value()),
            label: Some(SharedString::from(tag.name.clone())),
            subject: SharedString::from(tag.subject.clone()),
            author: SharedString::from(tag.author.clone()),
            date: SharedString::from(tag.date.clone()),
            remote: false,
            is_head: false,
            kind: Kind::Tag,
        }
    }

    /// `worktree` is the checkout being looked at: "here" is its branch, not
    /// the one git marks as HEAD where the list was read (the main worktree).
    pub fn of(branch: &Branch, worktree: &std::path::Path) -> Self {
        Self {
            name: SharedString::from(branch.name.clone()),
            label: None,
            subject: SharedString::from(branch.subject.clone()),
            author: SharedString::from(branch.author.clone()),
            date: SharedString::from(branch.date.clone()),
            remote: branch.kind == BranchKind::Remote,
            is_head: branch.is_head_in(worktree),
            kind: Kind::Branch,
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
    /// or for the rest the words for it: its value would say nothing.
    fn title(&self) -> SharedString {
        self.label.clone().unwrap_or_else(|| self.name.clone())
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
                    .when(self.kind == Kind::Tag, |el| {
                        el.child(tag(tr!("branch-tag"), cx))
                    })
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
            label: None,
            subject: subject.to_string().into(),
            author: author.to_string().into(),
            date: date.to_string().into(),
            remote: false,
            is_head: false,
            kind: Kind::Branch,
        }
    }

    fn day(text: &str) -> chrono::NaiveDate {
        chrono::NaiveDate::parse_from_str(text, "%Y-%m-%d").unwrap()
    }

    /// A period that reaches today stays open: picked on Monday, "the last
    /// seven days" still runs to now on Tuesday.
    #[test]
    fn a_period_reaching_today_has_no_end() {
        let today = day("2026-10-01");
        assert_eq!(
            Compare::period(day("2026-09-25"), today, today),
            Compare::Dates {
                from: "2026-09-25".into(),
                to: None
            }
        );
        assert_eq!(
            Compare::period(day("2026-09-01"), day("2026-09-07"), today),
            Compare::Dates {
                from: "2026-09-01".into(),
                to: Some("2026-09-07".into())
            }
        );
        // Picked backwards, read forwards.
        assert_eq!(
            Compare::period(day("2026-09-07"), day("2026-09-01"), today),
            Compare::period(day("2026-09-01"), day("2026-09-07"), today)
        );
    }

    /// A tag is compared in full, never by a name a branch could also bear,
    /// and its value is told from a branch's.
    #[test]
    fn a_tag_reads_back_from_its_value_and_compares_as_a_tag() {
        let tag = Compare::Tag("v1.2.0".into());
        assert_eq!(Compare::tag_of(&tag.value()), Some(tag.clone()));
        assert_eq!(
            tag.range(),
            DiffRange::Branch {
                base: "refs/tags/v1.2.0".into()
            }
        );
        assert_eq!(Compare::tag_of("dev"), None);
        assert_eq!(Compare::tag_of(SINCE_REVIEW), None);
        assert_eq!(Compare::tag_of(PERIOD), None);
        assert_eq!(Compare::tag_of(":tag:"), None);
        // The last tag is no tag of that name: one can be called `last-tag`.
        assert_eq!(
            Compare::tag_of(&Compare::LastTag.value()),
            Some(Compare::LastTag)
        );
        assert_eq!(
            Compare::tag_of(&Compare::Tag("last-tag".into()).value()),
            Some(Compare::Tag("last-tag".into()))
        );
        assert_eq!(Compare::LastTag.range(), DiffRange::LastTag);
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
