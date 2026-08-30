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

use std::path::{Path, PathBuf};

use gpui::{prelude::*, Context, Window};

use crate::git::{Branch, BranchKind};
use crate::runtime::Cmd;
use crate::tr;
use crate::ui::app::ClaudhubApp;
use gpui_component::{ActiveTheme as _, WindowExt as _};

/// One row of the list.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum Row {
    /// What the log beside the list is showing, above the branches.
    ///
    /// **Only where the list drives a log** — the docked column. The top bar's
    /// popover checks a branch out, and a scope is not something one checks
    /// out: the two rows would be two entries that do nothing there.
    Scope(Scope),
    /// A group heading: the locals first, the remotes after.
    ///
    /// It carries **how many branches stand under it**, which is what a folded
    /// group has left to say: "Remotes" over nothing tells one the group is
    /// closed, not that there are eighty-seven behind it. The count is the
    /// filtered one — the heading answers the list on screen, not the
    /// repository.
    Group {
        kind: BranchKind,
        count: usize,
    },
    Branch(BranchRow),
}

/// The two scopes that are not a branch.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Scope {
    /// The checkout one is on, whatever its name — PhpStorm writes it
    /// `HEAD (Current Branch)`, and it is the list's first row for the same
    /// reason: it is where one comes back to.
    Head,
    /// Every reference at once. Not PhpStorm's, and kept because it is where
    /// the graph earns its keep: parallel branches side by side.
    All,
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
    /// It declares an upstream. Not the same question as "is it behind": a
    /// branch level with its remote has one and shows no count, and it is the
    /// upstream — not the divergence — that says whether there is anything to
    /// update from or to delete over there.
    pub(super) tracked: bool,
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
///
/// `active` is the worktree being looked at, and it is what "here" means: the
/// list is read once per repository, so git's own HEAD mark points at the main
/// worktree's branch whatever checkout is on screen — the picker said "here"
/// on `dev` from every linked worktree, and refused to merge it.
pub(super) fn rows_for(
    branches: &[Branch],
    filter: &str,
    active: Option<&Path>,
    scopes: bool,
) -> Vec<Row> {
    let needle = filter.trim().to_lowercase();
    let mut rows = Vec::new();
    // **Dropped as soon as one types.** They are not branch names, so a filter
    // that keeps them would leave two rows standing over an empty list and
    // read as a search that found them.
    if scopes && needle.is_empty() {
        rows.push(Row::Scope(Scope::Head));
        rows.push(Row::Scope(Scope::All));
    }
    for kind in [BranchKind::Local, BranchKind::Remote] {
        let matching: Vec<Row> = branches
            .iter()
            .filter(|branch| branch.kind == kind)
            .filter(|branch| needle.is_empty() || branch.name.to_lowercase().contains(&needle))
            .map(|branch| {
                Row::Branch(BranchRow {
                    name: branch.name.clone(),
                    kind: branch.kind,
                    is_head: active.is_some_and(|worktree| branch.is_head_in(worktree)),
                    detail: detail(branch),
                    ahead: branch.upstream.as_ref().map(|up| up.ahead).unwrap_or(0),
                    behind: branch.upstream.as_ref().map(|up| up.behind).unwrap_or(0),
                    tracked: branch.upstream.is_some(),
                    taken_by: branch.checked_out_at.clone(),
                })
            })
            .collect();
        // An empty group has no heading: on a search that finds only remotes, a
        // "Local" title followed by nothing reads like a display glitch.
        if !matching.is_empty() {
            rows.push(Row::Group {
                kind,
                count: matching.len(),
            });
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

/// The deletion being confirmed, while the dialog is open.
///
/// An entity of its own and not a field of `ClaudhubApp`, like `StashDraft`: the
/// closure `open_dialog` keeps is called back from the root view's render, in
/// the middle of a borrow of the application, where reading it is a panic.
pub struct BranchDeletion {
    branch: String,
    /// There is something on `origin` to delete — otherwise no box at all.
    remote: bool,
    /// Delete it there too. **Unticked**: it is the half nobody undoes for you.
    also_remote: bool,
}

impl gpui::Render for BranchDeletion {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let (remote, also) = (self.remote, self.also_remote);
        gpui_component::v_flex()
            .w(gpui::px(420.))
            .gap_2()
            .child(gpui::div().text_sm().child(tr!("branch-delete-help")))
            .when(remote, |el| {
                el.child(
                    gpui_component::checkbox::Checkbox::new("branch-delete-remote")
                        .label(tr!("branch-delete-also-remote"))
                        .checked(also)
                        .on_click(cx.listener(|this, _, _window, cx| {
                            this.also_remote = !this.also_remote;
                            cx.notify();
                        })),
                )
            })
            // Said only once it has been asked for: a warning about a gesture
            // one has not chosen is a line one learns to read past.
            .when(remote && also, |el| {
                el.child(
                    gpui::div()
                        .text_xs()
                        .text_color(cx.theme().muted_foreground)
                        .child(tr!("branch-delete-remote-help")),
                )
            })
    }
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

    /// A new branch starting from another one, which the picker names.
    ///
    /// `Cmd::CreateBranch` with a start point **switches onto it**, which is
    /// what "new branch from" means everywhere else: one does not create a
    /// branch in order to stay where one was.
    pub(super) fn prompt_new_branch_from(
        &mut self,
        from: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        // The name is suggested and stays editable: `origin/feat` gives `feat`,
        // which is the branch one meant nine times out of ten.
        let suggestion = crate::git::branch::short_name(&from).to_string();
        self.open_text_dialog_with(
            tr!("branch-new-from", { name: from.clone() }),
            tr!("branch-new-placeholder"),
            suggestion,
            window,
            cx,
            move |this, name, _window, cx| {
                let name = name.trim().to_string();
                if name.is_empty() {
                    return;
                }
                let Some(worktree) = this.active.clone() else {
                    return;
                };
                this.start(
                    Some(worktree.clone()),
                    crate::runtime::Action::Branch,
                    Cmd::CreateBranch {
                        worktree,
                        name,
                        from: Some(from.clone()),
                    },
                    cx,
                );
            },
        );
    }

    /// Renames a branch, checked out or not.
    ///
    /// The field opens on the current name rather than empty: a rename is nearly
    /// always an edit of two characters, and retyping `feature/PROJ-1234` is
    /// what makes the gesture go unused.
    pub(super) fn prompt_rename_branch(
        &mut self,
        from: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.open_text_dialog_with(
            tr!("branch-rename"),
            tr!("branch-new-placeholder"),
            from.clone(),
            window,
            cx,
            move |this, to, _window, cx| {
                let to = to.trim().to_string();
                if to.is_empty() || to == from {
                    return;
                }
                let Some(main) = this.active.clone().and_then(|w| this.main_of(&w)) else {
                    return;
                };
                this.start(
                    None,
                    crate::runtime::Action::Branch,
                    Cmd::RenameBranch {
                        main,
                        from: from.clone(),
                        to,
                    },
                    cx,
                );
            },
        );
    }

    /// Removes a local branch, once confirmed — and its counterpart on `origin`
    /// when the box in the dialog is ticked.
    ///
    /// `-d` and never `-D`: git refuses a branch whose commits are nowhere else,
    /// and that refusal is the only net there is — nothing brings the commits
    /// back afterwards. The dialog says so, so that the error, when it comes,
    /// is one the user was warned of rather than one they have to interpret.
    ///
    /// **One entry and a box, where there were two entries.** Deleting a branch
    /// one is done with means deleting it in both places, and two menu items
    /// meant doing the gesture twice — and reading two dialogs to find out that
    /// the second was the other half of the first. The box is **unticked**: the
    /// remote half is the one nobody undoes for you, so it is asked for and
    /// never assumed.
    pub(super) fn confirm_delete_branch(
        &mut self,
        branch: String,
        // The branch has a counterpart on `origin` — no box without one, since
        // it would offer a deletion that can only come back as an error.
        remote: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let draft = cx.new(|_| BranchDeletion {
            branch: branch.clone(),
            remote,
            also_remote: false,
        });
        let entity = cx.entity();
        window.open_dialog(cx, move |dialog, _window, _cx| {
            let (entity, draft) = (entity.clone(), draft.clone());
            dialog
                .title(tr!("branch-delete-title", { name: branch.clone() }))
                .child(draft.clone())
                .overlay_closable(false)
                .close_button(false)
                .footer(crate::ui::dialogs::confirm())
                .on_ok(move |_, _window, cx| {
                    // Read on click, where the borrow has been given back: the
                    // closure above runs inside the application's own render.
                    let draft = draft.read(cx);
                    let (name, also) = (draft.branch.clone(), draft.also_remote);
                    entity.update(cx, |this, cx| this.delete_branch(name, also, cx));
                    true
                })
        });
    }

    /// The two commands a confirmed deletion sends, in the order that costs
    /// least when the first one fails.
    ///
    /// **The remote goes first**, which is the stash's order and the tags': `git
    /// branch -d` is the one that can be refused — a branch carrying commits
    /// that are nowhere else — and it is refused *because* those commits would
    /// be lost. Sending it first would leave the remote standing behind a local
    /// branch one has just been told one cannot delete; sending it last means
    /// the refusal arrives after the half one asked for went through.
    fn delete_branch(&mut self, name: String, also_remote: bool, cx: &mut Context<Self>) {
        let Some(main) = self.active.clone().and_then(|w| self.main_of(&w)) else {
            return;
        };
        if also_remote {
            self.start(
                None,
                crate::runtime::Action::Branch,
                Cmd::DeleteRemoteBranch {
                    main: main.clone(),
                    name: name.clone(),
                },
                cx,
            );
        }
        self.start(
            None,
            crate::runtime::Action::Branch,
            Cmd::DeleteBranch {
                main,
                name,
                force: false,
            },
            cx,
        );
    }

    /// Removes a branch from `origin`, once confirmed.
    pub(super) fn confirm_delete_remote_branch(
        &mut self,
        branch: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.confirm_branch(
            tr!("branch-delete-remote-title", { name: branch.clone() }),
            tr!("branch-delete-remote-help"),
            move |main| Cmd::DeleteRemoteBranch {
                main,
                name: branch.clone(),
            },
            window,
            cx,
        );
    }

    /// The shape a deletion's confirmation takes: a title, a sentence saying
    /// what it costs, and the command built once the repository is known.
    ///
    /// What is left of it since the local deletion grew a box of its own: the
    /// remote-tracking name's own gesture, where there is nothing to tick — the
    /// remote half is the only half there is.
    fn confirm_branch(
        &mut self,
        title: gpui::SharedString,
        help: gpui::SharedString,
        cmd: impl Fn(PathBuf) -> Cmd + 'static,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let entity = cx.entity();
        let cmd = std::rc::Rc::new(cmd);
        window.open_dialog(cx, move |dialog, _window, _cx| {
            let (entity, cmd) = (entity.clone(), cmd.clone());
            dialog
                .title(title.clone())
                .child(gpui::div().text_sm().child(help.clone()))
                .overlay_closable(false)
                .close_button(false)
                .footer(crate::ui::dialogs::confirm())
                .on_ok(move |_, _window, cx| {
                    entity.update(cx, |this, cx| {
                        let Some(main) = this.active.clone().and_then(|w| this.main_of(&w)) else {
                            return;
                        };
                        this.start(None, crate::runtime::Action::Branch, cmd(main), cx);
                    });
                    true
                })
        });
    }

    /// Compares the current worktree against another branch.
    ///
    /// It sets the review's base and goes to the Git screen — which is all
    /// "compare with" is here: the branch review *is* that comparison, and it
    /// keeps the base one chose, per worktree. The step is written on the trail
    /// like every other change of screen made with the mouse.
    pub(super) fn compare_against(
        &mut self,
        base: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.set_base(base, cx);
        self.travel_to_panel(crate::ui::panels::DiffPanel::NAME, window, cx);
    }

    /// Checks an existing branch out into a fresh worktree.
    ///
    /// **Through `wt` when the project has a `wt.toml`**: the creation dialog
    /// opens on the branch, the folder suggested, and the project's copies,
    /// ports, `post_new` and questions follow as for any creation. This gesture
    /// used to go straight to git, and the worktree it made — under the
    /// project's root, so taken for one `wt` knew — had none of that, with
    /// nothing to say so.
    ///
    /// Without a project, the bare git add: the folder takes the branch's
    /// name, slashes becoming dashes — `origin/feat/x` cannot be a folder name.
    pub(super) fn worktree_from_branch(
        &mut self,
        main: PathBuf,
        branch: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.wt_project(&main).is_some() {
            self.setup_worktree(main, Some(branch), window, cx);
            return;
        }
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
            .map(|p| crate::wslpath::join(p, format!("{repo_name}-wt")))
            .unwrap_or_else(|| crate::wslpath::join(&main, "worktrees"));
        self.git.send(Cmd::AddWorktree {
            main,
            path: crate::wslpath::join(&root, slug),
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
        let names: Vec<String> = rows_for(&branches, "", None, false)
            .into_iter()
            .map(|row| match row {
                Row::Group {
                    kind: BranchKind::Local,
                    ..
                } => "== locales".into(),
                Row::Group {
                    kind: BranchKind::Remote,
                    ..
                } => "== distantes".into(),
                Row::Scope(scope) => format!("{scope:?}"),
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

    /// A heading says how many branches stand under it, and it counts what the
    /// filter left: the number answers the list on screen, which is the only
    /// list one can see. Read on a folded group, where it is all that is left
    /// of the branches.
    #[test]
    fn a_group_counts_the_branches_the_filter_left() {
        let branches = vec![
            branch("main", BranchKind::Local),
            branch("wt/essai", BranchKind::Local),
            branch("origin/feature", BranchKind::Remote),
        ];
        let counts = |filter: &str| -> Vec<(BranchKind, usize)> {
            rows_for(&branches, filter, None, false)
                .into_iter()
                .filter_map(|row| match row {
                    Row::Group { kind, count } => Some((kind, count)),
                    _ => None,
                })
                .collect()
        };
        assert_eq!(
            counts(""),
            vec![(BranchKind::Local, 2), (BranchKind::Remote, 1)]
        );
        assert_eq!(counts("essai"), vec![(BranchKind::Local, 1)]);
    }

    #[test]
    fn the_filter_ignores_case_and_drops_empty_headings() {
        let branches = vec![
            branch("main", BranchKind::Local),
            branch("origin/Feature-X", BranchKind::Remote),
        ];
        let rows = rows_for(&branches, "feature", None, false);
        // No local matches any more: its heading disappears with it, otherwise
        // a title followed by nothing reads like a display glitch.
        assert_eq!(
            rows,
            vec![
                Row::Group {
                    kind: BranchKind::Remote,
                    count: 1,
                },
                match rows_for(&branches, "", None, false)
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

    /// The reported gesture: from the worktree `wt/integration-tests`, the
    /// picker said "here" on `dev` — the **main** worktree's branch, the one
    /// git marks as HEAD where the list is read — and refused to merge it.
    /// "Here" is the branch the worktree on screen holds, nothing else.
    #[test]
    fn here_is_the_branch_of_the_worktree_looked_at_not_the_mains() {
        let main = PathBuf::from("/repo");
        let linked = PathBuf::from("/repo-wt/integration");
        let mut dev = branch("dev", BranchKind::Local);
        dev.checked_out_at = Some(main);
        let mut wt = branch("wt/integration-tests", BranchKind::Local);
        wt.checked_out_at = Some(linked.clone());
        let branches = vec![dev, wt];

        let heads: Vec<(String, bool)> = rows_for(&branches, "", Some(&linked), false)
            .into_iter()
            .filter_map(|row| match row {
                Row::Branch(row) => Some((row.name, row.is_head)),
                Row::Group { .. } | Row::Scope(_) => None,
            })
            .collect();
        assert_eq!(
            heads,
            vec![("dev".into(), false), ("wt/integration-tests".into(), true)]
        );
        // And `dev`, held elsewhere, is "taken" — greyed, not mergeable-onto —
        // while the branch of this very worktree is not.
        let rows = rows_for(&branches, "", Some(&linked), false);
        let taken: Vec<bool> = rows
            .iter()
            .filter_map(|row| match row {
                Row::Branch(row) => Some(row.taken()),
                Row::Group { .. } | Row::Scope(_) => None,
            })
            .collect();
        assert_eq!(taken, vec![true, false]);
    }

    #[test]
    fn divergence_comes_from_the_upstream() {
        let mut b = branch("main", BranchKind::Local);
        b.upstream = Some(Upstream {
            name: "origin/main".into(),
            ahead: 2,
            behind: 3,
        });
        let rows = rows_for(std::slice::from_ref(&b), "", None, false);
        let Some(Row::Branch(row)) = rows.into_iter().nth(1) else {
            panic!("une branche");
        };
        assert_eq!((row.ahead, row.behind), (2, 3));
    }

    /// The two scope rows lead the docked list and belong to no group: they are
    /// what the log is pointed at when it is pointed at no branch in
    /// particular.
    #[test]
    fn the_docked_list_leads_with_the_two_scopes() {
        let branches = vec![branch("main", BranchKind::Local)];
        let rows = rows_for(&branches, "", None, true);
        assert_eq!(rows.first(), Some(&Row::Scope(Scope::Head)));
        assert_eq!(rows.get(1), Some(&Row::Scope(Scope::All)));
        assert!(matches!(rows.get(2), Some(Row::Group { .. })));
        // And the popover, which checks out, has neither: a scope is not
        // something one checks out.
        assert!(!rows_for(&branches, "", None, false)
            .iter()
            .any(|row| matches!(row, Row::Scope(_))));
    }

    /// They go as soon as one types. They are not branch names, so keeping them
    /// would leave two rows standing over an empty list and read as a search
    /// that found them.
    #[test]
    fn a_filter_drops_the_scopes() {
        let rows = rows_for(&[branch("main", BranchKind::Local)], "mai", None, true);
        assert!(!rows.iter().any(|row| matches!(row, Row::Scope(_))));
    }
}
