//! The branch picker: the list, its filter, and everything one asks of a branch.
//!
//! It was a `PopupMenu` and had reached that shape's ceiling. A menu has no
//! filter field — a repository with two hundred branches is scrolled, not
//! searched — and, worse, **a submenu inside a scrolling menu is clipped by the
//! very scroll it needs**: gpui captures a deferred draw's content mask at
//! prepaint, so the popup a row would open is cut off at the viewport's edge.
//! That is what kept the picker down to two gestures where PhpStorm's has a
//! dozen.
//!
//! So it is a popover of our own, in **two steps** — which is what PhpStorm's
//! branch popup does too, and for the same reason: the second step *replaces*
//! the first inside the same surface, so nothing is ever nested and nothing can
//! be clipped.
//!
//! - **Step one** is the filtered list. Clicking a row checks the branch out,
//!   which is the gesture one makes ten times a day and which has to stay one
//!   click. The `…` at the row's end opens the second step.
//! - **Step two** is that branch's actions, each with its name written out.
//!
//! The `…` is painted on every row and not on the hovered one alone: a control
//! that appears under the pointer moves what is beside it, and what is beside it
//! here is the count one was reading.
//!
//! **A disabled action stays on the list, greyed.** What one wants to know
//! standing here is *why* a gesture is not available — the branch is checked out
//! elsewhere, it has no upstream — and a line that is simply absent says nothing
//! at all.
//!
//! # Why it may read the application from its own render
//!
//! `BranchPicker` is an **entity**, and the popover's content is that entity and
//! not an element built in place. A child view's `render` runs *after* the
//! parent's render closure has handed back — the dock's panels rest on the same
//! rule — so `self.app.read(cx)` here is not the borrow that panics. Building
//! the surface inline inside `Popover::content`, which runs **within**
//! `ClaudhubApp::render`, is what would.

use std::path::PathBuf;
use std::rc::Rc;

use gpui::{
    div, prelude::*, px, App, Context, Entity, Focusable as _, Hsla, KeyDownEvent, ScrollStrategy,
    SharedString, WeakEntity, Window,
};
use gpui_component::{
    button::{Button, ButtonVariants as _},
    h_flex,
    input::{Input, InputState},
    popover::{Popover, PopoverState},
    v_flex, v_virtual_list, ActiveTheme, Sizable as _, StyledExt as _,
};

use crate::git::BranchKind;
use crate::runtime::{Action, Cmd};
use crate::tr;
use crate::ui::app::ClaudhubApp;
use crate::ui::branches::{rows_for, BranchRow, Row};
use crate::ui::icons::icon;

/// How wide the surface is.
///
/// Two lines of text need room: below this, the commit subject that tells two
/// similarly named branches apart is truncated to the point of saying nothing.
/// It is `base_select`'s width, and for the same reason.
const WIDTH: gpui::Pixels = px(420.);

/// How tall the list grows before it scrolls.
const LIST_HEIGHT: gpui::Pixels = px(320.);

/// Which of the two steps is on screen.
enum Step {
    /// The filtered list of branches.
    List,
    /// One branch's actions.
    ///
    /// The whole row is kept and not the name alone: what the actions offer
    /// depends on where the branch is checked out and on whether it has an
    /// upstream, and re-deriving that at every frame would mean walking the
    /// branch list again.
    Actions(BranchRow),
}

/// The colours a row needs, read once.
///
/// A virtualised list's closure runs for every visible row at every frame:
/// `cx.theme()` borrows, and reading it in there would be a borrow per row and
/// per image.
#[derive(Clone, Copy)]
struct Look {
    accent: Hsla,
    muted: Hsla,
    border: Hsla,
    /// A branch's row: two lines of text and next to nothing around them.
    row: gpui::Pixels,
    /// A group's heading: one line, and shorter than a row — it is a rule with
    /// a name on it, not an entry.
    head: gpui::Pixels,
}

impl Look {
    fn of(cx: &App) -> Self {
        let unit = crate::ui::theme::row_height(cx);
        Self {
            accent: cx.theme().accent,
            muted: cx.theme().muted_foreground,
            border: cx.theme().border,
            // Two lines and a hair, not two rows: a list where every entry is
            // twice as tall as it needs to be shows four of them where it could
            // show seven, and what one comes here to do is compare.
            row: unit * 1.45,
            head: unit * 0.95,
        }
    }
}

pub(super) struct BranchPicker {
    app: WeakEntity<ClaudhubApp>,
    query: Entity<InputState>,
    step: Step,
    scroll: gpui_component::VirtualListScrollHandle,
    /// Keyboard cursor into the **displayed** list, group headings included:
    /// what the arrows move is a row on screen, and a cursor counted on anything
    /// else drifts the moment a heading leaves with its group.
    cursor: usize,
    /// The rows on screen, kept between frames — see `rows`.
    rows: Rc<Vec<Row>>,
    /// The list has to be laid out again. Set by the three things that change
    /// it: the filter, a fold, and the repository itself.
    stale: bool,
    /// The groups one has closed.
    ///
    /// Two of them at most — the locals and the remotes — so a pair of flags and
    /// not a set: closing the remotes is what one does on a repository whose
    /// `origin` carries a hundred branches nobody has checked out. It does not
    /// outlive the window: a fold here is a reading posture, not a preference.
    folded: [bool; 2],
    /// The popover carrying us, so that a gesture can close it. Handed over by
    /// the content closure — a popover's state lives in element state, and that
    /// is the only place it is reachable from.
    popover: Option<Entity<PopoverState>>,
}

impl BranchPicker {
    pub(super) fn new(window: &mut Window, cx: &mut Context<ClaudhubApp>) -> Entity<Self> {
        let owner = cx.entity();
        let app = owner.downgrade();
        let query = cx.new(|cx| InputState::new(window, cx).placeholder(tr!("branch-filter")));
        cx.new(|cx| {
            // Typing filters: the list is laid out again, once, and a frame is
            // asked for.
            cx.subscribe(
                &query,
                |this: &mut Self, _, _event: &gpui_component::input::InputEvent, cx| {
                    this.stale = true;
                    cx.notify();
                },
            )
            .detach();
            // The list is a projection of the repository's branches, and they
            // move under it — a fetch, a checkout, a branch created next door.
            // Nothing else would tell the prepared list to let go.
            cx.observe(&owner, |this: &mut Self, _, _cx| this.stale = true)
                .detach();
            Self {
                app,
                query,
                step: Step::List,
                scroll: gpui_component::VirtualListScrollHandle::new(),
                cursor: 0,
                rows: Rc::new(Vec::new()),
                stale: true,
                folded: [false; 2],
                popover: None,
            }
        })
    }

    /// Puts the picker back where it opens: the whole list, nothing typed.
    ///
    /// A filter left over from last time is the one thing a picker must not
    /// reopen with — it reads as a repository that has lost its branches.
    fn reset(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.step = Step::List;
        self.cursor = 0;
        self.stale = true;
        self.query
            .update(cx, |input, cx| input.set_value("", window, cx));
        cx.notify();
    }

    fn close(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(popover) = self.popover.clone() {
            popover.update(cx, |state, cx| state.dismiss(window, cx));
        }
    }

    /// The rows on screen, headings included.
    ///
    /// **Kept between frames.** It was laid out again on every frame of the
    /// popover — every branch lowercased for the filter, every row's name and
    /// subject cloned — for a list that only moves when the filter, a fold or
    /// the repository does. Those three are what set `stale`.
    fn rows(&mut self, cx: &App) -> Rc<Vec<Row>> {
        if self.stale {
            self.rows = Rc::new(self.build_rows(cx));
            self.stale = false;
        }
        self.rows.clone()
    }

    fn build_rows(&self, cx: &App) -> Vec<Row> {
        let Some(app) = self.app.upgrade() else {
            return Vec::new();
        };
        let app = app.read(cx);
        let Some(repo) = app.active_path().and_then(|w| app.repo_of(&w)) else {
            return Vec::new();
        };
        let query = self.query.read(cx).value();
        let rows = rows_for(&repo.branches, &query);
        // **A filter ignores the folds**, the window's rule for every foldable
        // list: a query that found something and shows nothing is read as a
        // query that found nothing.
        if query.trim().is_empty() {
            return fold(rows, self.folded);
        }
        rows
    }

    /// Where the two halves of the wire go: a checkout is made in the worktree
    /// being looked at, everything else is a write on the repository's refs.
    fn targets(&self, cx: &App) -> Option<(PathBuf, PathBuf)> {
        let app = self.app.upgrade()?;
        let app = app.read(cx);
        let worktree = app.active_path()?;
        let main = app.main_of(&worktree)?;
        Some((worktree, main))
    }

    /// Runs `f` on the application, then closes: every action here is the end of
    /// the gesture the picker was opened for.
    fn act(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
        f: impl FnOnce(&mut ClaudhubApp, &mut Window, &mut Context<ClaudhubApp>),
    ) {
        if let Some(app) = self.app.upgrade() {
            app.update(cx, |this, cx| f(this, window, cx));
        }
        self.close(window, cx);
    }

    fn checkout(&mut self, branch: &BranchRow, window: &mut Window, cx: &mut Context<Self>) {
        if branch.is_head || branch.taken() {
            return;
        }
        let Some((worktree, _)) = self.targets(cx) else {
            return;
        };
        let name = branch.name.clone();
        self.act(window, cx, move |app, _window, cx| {
            app.start(
                Some(worktree.clone()),
                Action::Checkout,
                Cmd::Checkout {
                    worktree,
                    branch: name,
                },
                cx,
            );
        });
    }

    // — Step one: the list ————————————————————————————————————————

    /// Moves the keyboard cursor, stepping over the group headings.
    ///
    /// A heading is a row on screen and therefore counted, but it is not
    /// somewhere one can land: an arrow that stops on "Locales" reads as stuck.
    /// It wraps — what one is walking is a handful of names, and an arrow that
    /// stops answering at the last of them reads as broken too.
    fn step_cursor(&mut self, delta: isize, cx: &mut Context<Self>) {
        let rows = self.rows(cx);
        let Some(next) = crate::ui::picker::step_cursor(
            rows.len(),
            |ix| matches!(rows[ix], Row::Branch(_)),
            self.cursor,
            delta,
        ) else {
            return;
        };
        self.cursor = next;
        self.scroll.scroll_to_item(next, ScrollStrategy::Top);
        cx.notify();
    }

    /// The arrows and `Enter`, taken **before** the field sees them.
    ///
    /// In capture phase and on an ancestor of the input: a single-line
    /// `InputState` binds Up and Down to the ends of its text, so left to bubble
    /// they would never reach the list. Escape is deliberately untouched — it
    /// belongs to the popover, which is what one expects it to close.
    fn on_key(&mut self, event: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        if !matches!(self.step, Step::List) {
            return;
        }
        match event.keystroke.key.as_str() {
            "down" => {
                cx.stop_propagation();
                self.step_cursor(1, cx);
            }
            "up" => {
                cx.stop_propagation();
                self.step_cursor(-1, cx);
            }
            "enter" => {
                cx.stop_propagation();
                let rows = self.rows(cx);
                if let Some(Row::Branch(row)) = rows.get(self.cursor).cloned() {
                    self.checkout(&row, window, cx);
                }
            }
            _ => {}
        }
    }

    fn render_list(&mut self, cx: &mut Context<Self>) -> gpui::AnyElement {
        let look = Look::of(cx);
        let rows = self.rows(cx);
        let count = rows.len();
        let cursor = self.cursor;
        let folded = self.folded;
        // A heading is not as tall as an entry, so the list is a
        // `v_virtual_list` and not a `uniform_list` — the same swap the diff's
        // wrapping and the merge view make, and for the same reason.
        let sizes = Rc::new(
            rows.iter()
                .map(|row| match row {
                    Row::Group(_) => gpui::size(px(0.), look.head),
                    Row::Branch(_) => gpui::size(px(0.), look.row),
                })
                .collect::<Vec<_>>(),
        );
        let entity = cx.entity();
        let build = {
            let rows = rows.clone();
            move |ix: usize, cx: &mut App| match &rows[ix] {
                Row::Group(kind) => {
                    group_heading(&entity, ix, *kind, folded[group_ix(*kind)], look)
                }
                Row::Branch(row) => branch_row(&entity, ix, row, ix == cursor, look, cx),
            }
        };
        v_flex()
            .w_full()
            .min_h_0()
            .child(
                div()
                    .w_full()
                    .px_1()
                    .py_1()
                    .child(Input::new(&self.query).xsmall()),
            )
            .child(if count == 0 {
                div()
                    .w_full()
                    .p_3()
                    .text_sm()
                    .text_color(look.muted)
                    .child(tr!("branch-none"))
                    .into_any_element()
            } else {
                crate::ui::scroll::vertical(
                    "branch-list",
                    &self.scroll,
                    v_virtual_list(
                        cx.entity(),
                        "branch-rows",
                        sizes,
                        move |_, range, _window, cx| {
                            range.map(|ix| build(ix, cx)).collect::<Vec<_>>()
                        },
                    )
                    .size_full()
                    .track_scroll(&self.scroll),
                )
                .h(LIST_HEIGHT)
                .into_any_element()
            })
            .child(self.render_list_footer(look, cx))
            .into_any_element()
    }

    /// What one asks of the list as a whole rather than of a branch in it.
    fn render_list_footer(&self, look: Look, cx: &mut Context<Self>) -> impl IntoElement {
        h_flex()
            .w_full()
            .px_1()
            .py_0p5()
            .gap_1()
            .items_center()
            .border_t_1()
            .border_color(look.border)
            .child(
                Button::new("branch-new")
                    .ghost()
                    .xsmall()
                    .icon(icon("plus"))
                    .label(tr!("branch-new"))
                    .on_click(cx.listener(|this, _, window, cx| {
                        this.act(window, cx, |app, window, cx| {
                            app.prompt_new_branch(window, cx)
                        });
                    })),
            )
            .child(div().flex_1())
            // A fetch is what makes the counts on this list true: they are read
            // off references a remote has moved, and nothing here refreshes them.
            .child(
                Button::new("branch-fetch")
                    .ghost()
                    .xsmall()
                    .icon(icon("refresh-cw"))
                    .tooltip(tr!("action-fetch"))
                    .on_click(cx.listener(|this, _, window, cx| {
                        let Some((worktree, _)) = this.targets(cx) else {
                            return;
                        };
                        this.act(window, cx, move |app, _window, cx| {
                            app.start(
                                Some(worktree.clone()),
                                Action::Fetch,
                                Cmd::Fetch { worktree },
                                cx,
                            );
                        });
                    })),
            )
    }

    // — Step two: one branch's actions ————————————————————————————

    fn render_actions(&mut self, row: &BranchRow, cx: &mut Context<Self>) -> gpui::AnyElement {
        let look = Look::of(cx);
        let remote = row.kind == BranchKind::Remote;
        // A remote-tracking name is its own upstream; a local one has to declare
        // it. Nothing to publish to and nothing to come down from otherwise.
        let has_remote = remote || row.tracked;
        let name = row.name.clone();
        let checkable = !row.is_head && !row.taken();

        let mut list = v_flex().w_full().p_1().gap_0p5();
        // Checking out first: it is what the row's own click does, and a list of
        // actions that hid its main one would read as if arriving here had cost
        // it.
        list = list.child(self.action(
            "checkout",
            "git-branch",
            tr!("branch-checkout"),
            checkable,
            {
                let row = row.clone();
                move |this, window, cx| this.checkout(&row, window, cx)
            },
            look,
            cx,
        ));
        list = list.child(self.action(
            "new-from",
            "plus",
            tr!("branch-new-from", { name: name.clone() }),
            true,
            {
                let from = name.clone();
                move |this, window, cx| {
                    // Re-cloned at each call: this is an `Fn`, and a menu row may
                    // be clicked more than once before the surface closes.
                    let from = from.clone();
                    this.act(window, cx, move |app, window, cx| {
                        app.prompt_new_branch_from(from, window, cx)
                    })
                }
            },
            look,
            cx,
        ));
        list = list.child(self.action(
            "worktree-from",
            "folder-open",
            tr!("branch-new-worktree"),
            !row.taken(),
            {
                let branch = name.clone();
                move |this, window, cx| {
                    let Some((_, main)) = this.targets(cx) else {
                        return;
                    };
                    let branch = branch.clone();
                    this.act(window, cx, move |app, window, cx| {
                        app.worktree_from_branch(main, branch, window, cx)
                    })
                }
            },
            look,
            cx,
        ));

        list = list.child(separator(look));
        list = list.child(self.action(
            "compare",
            "file-diff",
            tr!("branch-compare"),
            !row.is_head,
            {
                let base = name.clone();
                move |this, window, cx| {
                    let base = base.clone();
                    this.act(window, cx, move |app, window, cx| {
                        app.compare_against(base, window, cx)
                    })
                }
            },
            look,
            cx,
        ));

        list = list.child(separator(look));
        list = list.child(self.action(
            "merge",
            "git-merge",
            tr!("branch-merge-into"),
            !row.is_head,
            {
                let from = name.clone();
                move |this, window, cx| {
                    let Some((worktree, _)) = this.targets(cx) else {
                        return;
                    };
                    let from = from.clone();
                    this.act(window, cx, move |app, _window, cx| {
                        app.start(
                            Some(worktree.clone()),
                            Action::Merge,
                            Cmd::Merge {
                                worktree,
                                from,
                                no_ff: false,
                            },
                            cx,
                        )
                    })
                }
            },
            look,
            cx,
        ));
        list = list.child(self.action(
            "rebase",
            "git-pull-request",
            tr!("branch-rebase-onto"),
            !row.is_head,
            {
                let onto = name.clone();
                move |this, window, cx| {
                    let Some((worktree, _)) = this.targets(cx) else {
                        return;
                    };
                    let onto = onto.clone();
                    this.act(window, cx, move |app, _window, cx| {
                        app.start(
                            Some(worktree.clone()),
                            Action::Rebase,
                            Cmd::Rebase { worktree, onto },
                            cx,
                        )
                    })
                }
            },
            look,
            cx,
        ));

        // The two network gestures. Neither is offered on a remote-tracking
        // name: there is no local ref there to publish, and nothing to bring up
        // to date but the fetch that made it.
        if !remote {
            list = list.child(separator(look));
            list = list.child(self.action(
                "update",
                "arrow-down-to-line",
                tr!("branch-update"),
                has_remote,
                {
                    let branch = name.clone();
                    move |this, window, cx| {
                        let Some((_, main)) = this.targets(cx) else {
                            return;
                        };
                        let branch = branch.clone();
                        this.act(window, cx, move |app, _window, cx| {
                            app.start(None, Action::Pull, Cmd::UpdateBranch { main, branch }, cx)
                        })
                    }
                },
                look,
                cx,
            ));
            list = list.child(self.action(
                "push",
                "arrow-up-from-line",
                tr!("branch-push"),
                true,
                {
                    let branch = name.clone();
                    move |this, window, cx| {
                        let Some((_, main)) = this.targets(cx) else {
                            return;
                        };
                        let branch = branch.clone();
                        this.act(window, cx, move |app, _window, cx| {
                            app.start(
                                None,
                                Action::Push,
                                Cmd::PushBranch {
                                    main,
                                    branch,
                                    force_with_lease: false,
                                },
                                cx,
                            )
                        })
                    }
                },
                look,
                cx,
            ));

            list = list.child(separator(look));
            list = list.child(self.action(
                "rename",
                "pencil",
                tr!("branch-rename"),
                true,
                {
                    let from = name.clone();
                    move |this, window, cx| {
                        let from = from.clone();
                        this.act(window, cx, move |app, window, cx| {
                            app.prompt_rename_branch(from, window, cx)
                        })
                    }
                },
                look,
                cx,
            ));
            list = list.child(self.action(
                "delete",
                "trash-2",
                tr!("branch-delete"),
                checkable,
                {
                    let branch = name.clone();
                    move |this, window, cx| {
                        let branch = branch.clone();
                        this.act(window, cx, move |app, window, cx| {
                            app.confirm_delete_branch(branch, window, cx)
                        })
                    }
                },
                look,
                cx,
            ));
        }
        // On the remote, and said as such: it is a different regret from the
        // local one, and nobody undoes it for you.
        if has_remote {
            if remote {
                list = list.child(separator(look));
            }
            list = list.child(self.action(
                "delete-remote",
                "trash-2",
                tr!("branch-delete-remote"),
                true,
                {
                    let branch = name.clone();
                    move |this, window, cx| {
                        let branch = branch.clone();
                        this.act(window, cx, move |app, window, cx| {
                            app.confirm_delete_remote_branch(branch, window, cx)
                        })
                    }
                },
                look,
                cx,
            ));
        }

        v_flex()
            .w_full()
            .child(self.render_actions_header(row, look, cx))
            .child(list)
            .into_any_element()
    }

    fn render_actions_header(
        &self,
        row: &BranchRow,
        look: Look,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        h_flex()
            .w_full()
            .p_1()
            .gap_1()
            .items_center()
            .border_b_1()
            .border_color(look.border)
            .child(
                Button::new("branch-back")
                    .ghost()
                    .xsmall()
                    .icon(icon("arrow-left"))
                    .tooltip(tr!("branch-back"))
                    .on_click(cx.listener(|this, _, _window, cx| {
                        this.step = Step::List;
                        cx.notify();
                    })),
            )
            .child(icon("git-branch").xsmall().text_color(look.muted))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .text_sm()
                    .font_semibold()
                    .child(SharedString::from(row.name.clone())),
            )
            .when(row.kind == BranchKind::Remote, |el| {
                el.child(tag(tr!("branch-remote"), look))
            })
            .when(row.is_head, |el| el.child(tag(tr!("branch-here"), look)))
    }

    /// One action: an icon, its name, and nothing else.
    ///
    /// A row and not a `Button`: a button centres its label, and a column of
    /// centred labels of very unequal length reads as a heap rather than as a
    /// list.
    #[allow(clippy::too_many_arguments)]
    fn action(
        &self,
        id: &'static str,
        glyph: &'static str,
        label: SharedString,
        enabled: bool,
        on_click: impl Fn(&mut Self, &mut Window, &mut Context<Self>) + 'static,
        look: Look,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        h_flex()
            .id(id)
            .w_full()
            .px_2()
            .py_1()
            .gap_2()
            .items_center()
            .rounded(cx.theme().radius)
            .when(!enabled, |el| el.text_color(look.muted))
            .when(enabled, |el| {
                el.cursor_pointer()
                    .hover(|s| s.bg(look.accent.opacity(0.4)))
                    .on_click(cx.listener(move |this, _, window, cx| on_click(this, window, cx)))
            })
            .child(icon(glyph).xsmall().text_color(look.muted))
            .child(div().flex_1().min_w_0().truncate().text_sm().child(label))
    }
}

impl Render for BranchPicker {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let body = match &self.step {
            Step::List => self.render_list(cx),
            Step::Actions(row) => {
                let row = row.clone();
                self.render_actions(&row, cx)
            }
        };
        v_flex()
            .w(WIDTH)
            .min_h_0()
            .capture_key_down(cx.listener(Self::on_key))
            .child(body)
    }
}

/// Which of the two flags a group's fold lives in.
fn group_ix(kind: BranchKind) -> usize {
    match kind {
        BranchKind::Local => 0,
        BranchKind::Remote => 1,
    }
}

/// Hides the branches of the groups one has closed, keeping their headings.
///
/// A heading with nothing under it is what a fold *is*: removing it too would
/// leave no way back.
fn fold(rows: Vec<Row>, folded: [bool; 2]) -> Vec<Row> {
    let mut shut = false;
    rows.into_iter()
        .filter(|row| match row {
            Row::Group(kind) => {
                shut = folded[group_ix(*kind)];
                true
            }
            Row::Branch(_) => !shut,
        })
        .collect()
}

/// A group's heading, in the list.
///
/// **The whole line folds**, not a chevron alone: a chevron is eight pixels
/// wide, and what one is aiming at is the group. Closing the remotes is what one
/// does on a repository whose `origin` carries a hundred branches nobody has
/// checked out.
fn group_heading(
    picker: &Entity<BranchPicker>,
    index: usize,
    kind: BranchKind,
    folded: bool,
    look: Look,
) -> gpui::AnyElement {
    let picker = picker.clone();
    h_flex()
        .id(("branch-group", index))
        .h(look.head)
        .w_full()
        .pl_1()
        .pr(crate::ui::theme::scroll_gutter())
        .gap_1()
        .items_center()
        .cursor_pointer()
        .hover(|s| s.bg(look.accent.opacity(0.3)))
        .on_click(move |_, _window, cx| {
            picker.update(cx, |this, cx| {
                this.folded[group_ix(kind)] = !this.folded[group_ix(kind)];
                this.stale = true;
                cx.notify();
            });
        })
        .child(
            icon(if folded {
                "chevron-right"
            } else {
                "chevron-down"
            })
            .xsmall()
            .text_color(look.muted),
        )
        .child(
            div()
                .flex_1()
                .min_w_0()
                .truncate()
                .text_xs()
                .font_semibold()
                .text_color(look.muted)
                .child(match kind {
                    BranchKind::Local => tr!("branches-local"),
                    BranchKind::Remote => tr!("branches-remote"),
                }),
        )
        .into_any_element()
}

/// One branch: what it is called, what it carries, and the way into its actions.
///
/// A band and not a pill — the window's rule for every list: what a selected row
/// designates is the *row*, which is what the click and the `…` act on.
fn branch_row(
    picker: &Entity<BranchPicker>,
    index: usize,
    row: &BranchRow,
    at_cursor: bool,
    look: Look,
    _cx: &mut App,
) -> gpui::AnyElement {
    // Where it is checked out matters more than what it carries: that is what
    // explains a greyed row.
    let detail = match row.taken_by.as_ref().filter(|_| !row.is_head) {
        Some(path) => format!(
            "{} {}",
            tr!("branch-checked-out"),
            path.file_name().unwrap_or_default().to_string_lossy()
        ),
        None => row.detail.clone(),
    };
    let checkable = !row.is_head && !row.taken();
    let (for_click, for_menu) = (picker.clone(), picker.clone());
    let (clicked, opened) = (row.clone(), row.clone());

    h_flex()
        .id(("branch-row", index))
        .h(look.row)
        .w_full()
        // Set in from the heading: a flat column under a title reads as a list
        // that happens to have a title above it, not as the group's branches.
        // It is the chevron's own width.
        .pl_5()
        .pr(crate::ui::theme::scroll_gutter())
        .gap_1()
        .items_center()
        .when(at_cursor, |el| el.bg(look.accent.opacity(0.5)))
        .when(checkable, |el| {
            el.cursor_pointer()
                .hover(|s| s.bg(look.accent.opacity(0.4)))
                .on_click(move |_, window, cx| {
                    for_click.update(cx, |this, cx| this.checkout(&clicked, window, cx));
                })
        })
        .child(
            v_flex()
                .flex_1()
                .min_w_0()
                .justify_center()
                .child(
                    h_flex()
                        .w_full()
                        .min_w_0()
                        .gap_1()
                        .items_center()
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .truncate()
                                .text_sm()
                                .when(row.is_head, |el| el.font_semibold())
                                .when(!checkable && !row.is_head, |el| el.text_color(look.muted))
                                .child(SharedString::from(row.name.clone())),
                        )
                        .when(row.is_head, |el| el.child(tag(tr!("branch-here"), look))),
                )
                .when(!detail.is_empty(), |el| {
                    el.child(
                        div()
                            .w_full()
                            .min_w_0()
                            .truncate()
                            .text_xs()
                            .text_color(look.muted)
                            .child(detail),
                    )
                }),
        )
        // Behind before ahead: that is what has to be integrated before one can
        // push.
        .when(row.behind > 0, |el| {
            el.child(
                div()
                    .flex_none()
                    .text_xs()
                    .text_color(look.muted)
                    .child(format!("↓{}", row.behind)),
            )
        })
        .when(row.ahead > 0, |el| {
            el.child(
                div()
                    .flex_none()
                    .text_xs()
                    .text_color(look.muted)
                    .child(format!("↑{}", row.ahead)),
            )
        })
        .child(
            Button::new(("branch-actions", index))
                .ghost()
                .xsmall()
                .icon(icon("ellipsis"))
                .tooltip(tr!("branch-actions"))
                .on_click(move |_, _window, cx| {
                    // The row is clickable too, and its click checks out: without
                    // this the `…` would switch branch on its way to the menu.
                    cx.stop_propagation();
                    for_menu.update(cx, |this, cx| {
                        this.step = Step::Actions(opened.clone());
                        cx.notify();
                    });
                }),
        )
        .into_any_element()
}

fn separator(look: Look) -> impl IntoElement {
    div().w_full().my_0p5().h(px(1.)).bg(look.border)
}

fn tag(label: SharedString, look: Look) -> impl IntoElement {
    div()
        .flex_none()
        .px_1()
        .rounded_sm()
        .bg(look.accent)
        .text_xs()
        .text_color(look.muted)
        .child(label)
}

impl ClaudhubApp {
    /// The branch picker's button, and the surface it opens.
    ///
    /// `None` without a worktree: an empty picker offers a gesture that cannot
    /// be made.
    pub(super) fn render_branch_picker(&self, cx: &mut Context<Self>) -> Option<impl IntoElement> {
        let worktree = self.active_path()?;
        let label = self
            .active_worktree()
            .and_then(|w| w.branch.clone())
            .unwrap_or_else(|| tr!("branch-detached").to_string());
        // The lead and the lag come with it: they are read as part of the
        // branch, and they were the other half of what the status bar said.
        let (ahead, behind) = self
            .active_review()
            .map(|r| (r.status.ahead, r.status.behind))
            .unwrap_or((0, 0));
        let _ = worktree;
        let muted = cx.theme().muted_foreground;
        let picker = self.branch_picker.clone();
        let focus = picker.read(cx).query.read(cx).focus_handle(cx);
        let for_open = picker.clone();
        Some(
            Popover::new("branch-picker")
                .track_focus(&focus)
                .trigger(
                    Button::new("branch-picker-trigger").ghost().small().child(
                        h_flex()
                            .gap_1()
                            .items_center()
                            .text_color(muted)
                            .child(icon("git-branch").xsmall())
                            .child(div().max_w(px(220.)).truncate().text_sm().child(label))
                            .when(behind > 0, |el| el.child(format!("↓{behind}")))
                            .when(ahead > 0, |el| el.child(format!("↑{ahead}")))
                            .child(icon("chevron-down").xsmall()),
                    ),
                )
                // Opening puts the picker back at step one with an empty filter.
                // It is a click and not a render, so touching another entity here
                // is licit.
                .on_open_change(move |open, window, cx| {
                    if *open {
                        for_open.update(cx, |this, cx| this.reset(window, cx));
                    }
                })
                // The content **is** the entity, and that is what makes it able
                // to read the application: this closure runs inside
                // `ClaudhubApp::render`, a child view's render does not.
                .content(move |_state, _window, cx| {
                    let popover = cx.entity();
                    picker.update(cx, |this, _| this.popover = Some(popover));
                    picker.clone()
                })
                // The surface paints its own padding: a list's rows run edge to
                // edge, which the popover's own `p_3` would break.
                .appearance(true)
                .p_0(),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn branch(name: &str, kind: BranchKind) -> Row {
        Row::Branch(BranchRow {
            name: name.into(),
            kind,
            is_head: false,
            detail: String::new(),
            ahead: 0,
            behind: 0,
            tracked: false,
            taken_by: None,
        })
    }

    fn names(rows: &[Row]) -> Vec<String> {
        rows.iter()
            .map(|row| match row {
                Row::Group(BranchKind::Local) => "== locales".into(),
                Row::Group(BranchKind::Remote) => "== distantes".into(),
                Row::Branch(row) => row.name.clone(),
            })
            .collect()
    }

    /// A folded group keeps its heading: that heading **is** the fold, and
    /// removing it too would leave no way back.
    #[test]
    fn a_folded_group_keeps_its_heading_and_loses_its_branches() {
        let rows = vec![
            Row::Group(BranchKind::Local),
            branch("main", BranchKind::Local),
            Row::Group(BranchKind::Remote),
            branch("origin/feat", BranchKind::Remote),
        ];
        assert_eq!(
            names(&fold(rows.clone(), [false, true])),
            vec!["== locales", "main", "== distantes"]
        );
        assert_eq!(
            names(&fold(rows.clone(), [true, false])),
            vec!["== locales", "== distantes", "origin/feat"]
        );
        assert_eq!(names(&fold(rows, [false, false])).len(), 4);
    }
}
