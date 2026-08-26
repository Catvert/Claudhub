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

use gpui::{div, prelude::*, px, App, IntoElement, SharedString, Window};
use gpui_component::{h_flex, select::SelectItem, v_flex, ActiveTheme};

use crate::git::{Branch, BranchKind};
use crate::tr;

/// A branch as the selector offers it.
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
}

impl BaseChoice {
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

    fn title(&self) -> SharedString {
        self.name.clone()
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
                    .child(div().flex_1().min_w_0().truncate().child(self.name.clone()))
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
pub const MENU_WIDTH: gpui::Pixels = px(420.);

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
        }
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
