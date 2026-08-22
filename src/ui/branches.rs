//! The branches: what the picker lists, and the two gestures on a branch.
//!
//! They serve two purposes: switching the current worktree to another branch,
//! and creating a worktree from an existing branch — the opening gesture of a
//! review, when an agent's work has landed on a branch not yet checked out.
//!
//! There is no panel any more: the list lives in the top bar's branch picker,
//! beside the worktree it applies to (see `ui::topbar`). What stays here is the
//! decision — which branch appears, under which heading — which is free of gpui
//! and tested, and the two gestures, which are dialogs.

use std::path::PathBuf;

use gpui::{Context, Window};

use crate::git::{Branch, BranchKind};
use crate::runtime::Cmd;
use crate::tr;
use crate::ui::app::ClaudhubApp;

/// One row of the list.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum Row {
    /// A group heading: the locals first, the remotes after.
    Group(BranchKind),
    Branch(BranchRow),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct BranchRow {
    pub(super) name: String,
    pub(super) kind: BranchKind,
    pub(super) is_head: bool,
    /// What the branch carries, in one line: its last subject and its date.
    pub(super) detail: String,
    pub(super) ahead: usize,
    pub(super) behind: usize,
    /// Worktree that already holds it. Git refuses two checkouts of the same
    /// branch: saying so beforehand beats an error.
    pub(super) taken_by: Option<PathBuf>,
}

impl BranchRow {
    /// Neither checkable out here nor elsewhere: it is already somewhere.
    pub(super) fn taken(&self) -> bool {
        self.taken_by.is_some() && !self.is_head
    }
}

/// Turns the branches into a list, filtered and grouped.
///
/// A free, tested function: it is this view's only decision — which one
/// appears, under which group.
pub(super) fn rows_for(branches: &[Branch], filter: &str) -> Vec<Row> {
    let needle = filter.trim().to_lowercase();
    let mut rows = Vec::new();
    for kind in [BranchKind::Local, BranchKind::Remote] {
        let matching: Vec<Row> = branches
            .iter()
            .filter(|branch| branch.kind == kind)
            .filter(|branch| needle.is_empty() || branch.name.to_lowercase().contains(&needle))
            .map(|branch| {
                Row::Branch(BranchRow {
                    name: branch.name.clone(),
                    kind: branch.kind,
                    is_head: branch.is_head,
                    detail: detail(branch),
                    ahead: branch.upstream.as_ref().map(|up| up.ahead).unwrap_or(0),
                    behind: branch.upstream.as_ref().map(|up| up.behind).unwrap_or(0),
                    taken_by: branch.checked_out_at.clone(),
                })
            })
            .collect();
        // An empty group has no heading: on a search that finds only remotes, a
        // "Local" title followed by nothing reads like a display glitch.
        if !matching.is_empty() {
            rows.push(Row::Group(kind));
            rows.extend(matching);
        }
    }
    rows
}

/// The second line: the last commit's subject, then its date.
///
/// Empty pieces are dropped rather than separated by a middle dot surrounding
/// nothing — a freshly cloned repository does not always have a subject to
/// show.
fn detail(branch: &Branch) -> String {
    [branch.subject.as_str(), branch.date.as_str()]
        .into_iter()
        .filter(|part| !part.trim().is_empty())
        .collect::<Vec<_>>()
        .join(" · ")
}

impl ClaudhubApp {
    pub(super) fn prompt_new_branch(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.open_text_dialog(
            tr!("branch-new"),
            tr!("branch-new-placeholder"),
            window,
            cx,
            |this, name, _window, cx| {
                let name = name.trim().to_string();
                if name.is_empty() {
                    return;
                }
                let Some(worktree) = this.active.clone() else {
                    return;
                };
                this.git.send(Cmd::CreateBranch {
                    worktree,
                    name,
                    from: None,
                });
                cx.notify();
            },
        );
    }

    /// Checks an existing branch out into a fresh worktree.
    ///
    /// The folder takes the branch's name, slashes becoming dashes:
    /// `origin/feat/x` cannot be a folder name.
    pub(super) fn worktree_from_branch(
        &mut self,
        main: PathBuf,
        branch: String,
        cx: &mut Context<Self>,
    ) {
        let local = branch
            .strip_prefix("origin/")
            .unwrap_or(&branch)
            .to_string();
        let slug = local.replace('/', "-");
        let repo_name = main
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| "repo".into());
        let root = main
            .parent()
            .map(|p| p.join(format!("{repo_name}-wt")))
            .unwrap_or_else(|| main.join("worktrees"));
        self.git.send(Cmd::AddWorktree {
            main,
            path: root.join(slug),
            branch: local,
            from: None,
        });
        cx.notify();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::git::Upstream;

    fn branch(name: &str, kind: BranchKind) -> Branch {
        Branch {
            name: name.into(),
            kind,
            is_head: false,
            date: "hier".into(),
            subject: "Un commit".into(),
            author: "Zoé".into(),
            upstream: None,
            checked_out_at: None,
        }
    }

    #[test]
    fn locals_come_first_each_under_its_own_heading() {
        let branches = vec![
            branch("main", BranchKind::Local),
            branch("origin/feature", BranchKind::Remote),
            branch("wt/essai", BranchKind::Local),
        ];
        let names: Vec<String> = rows_for(&branches, "")
            .into_iter()
            .map(|row| match row {
                Row::Group(BranchKind::Local) => "== locales".into(),
                Row::Group(BranchKind::Remote) => "== distantes".into(),
                Row::Branch(row) => row.name,
            })
            .collect();
        assert_eq!(
            names,
            vec![
                "== locales",
                "main",
                "wt/essai",
                "== distantes",
                "origin/feature"
            ]
        );
    }

    #[test]
    fn the_filter_ignores_case_and_drops_empty_headings() {
        let branches = vec![
            branch("main", BranchKind::Local),
            branch("origin/Feature-X", BranchKind::Remote),
        ];
        let rows = rows_for(&branches, "feature");
        // No local matches any more: its heading disappears with it, otherwise
        // a title followed by nothing reads like a display glitch.
        assert_eq!(
            rows,
            vec![
                Row::Group(BranchKind::Remote),
                match rows_for(&branches, "")
                    .into_iter()
                    .find(|r| matches!(r, Row::Branch(b) if b.name == "origin/Feature-X"))
                {
                    Some(row) => row,
                    None => panic!("the remote branch should exist"),
                }
            ]
        );
    }

    #[test]
    fn the_detail_line_skips_what_it_does_not_have() {
        let mut b = branch("main", BranchKind::Local);
        assert_eq!(detail(&b), "Un commit · hier");
        b.subject = String::new();
        assert_eq!(detail(&b), "hier");
        b.date = String::new();
        assert_eq!(detail(&b), "");
    }

    #[test]
    fn divergence_comes_from_the_upstream() {
        let mut b = branch("main", BranchKind::Local);
        b.upstream = Some(Upstream {
            name: "origin/main".into(),
            ahead: 2,
            behind: 3,
        });
        let rows = rows_for(std::slice::from_ref(&b), "");
        let Some(Row::Branch(row)) = rows.into_iter().nth(1) else {
            panic!("une branche");
        };
        assert_eq!((row.ahead, row.behind), (2, 3));
    }
}
