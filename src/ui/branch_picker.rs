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
//! The `…` keeps its place on every row and shows itself on the one under the
//! pointer: a control that *appeared* there would move what is beside it, and
//! what is beside it here is the count one was reading — so it is hidden by
//! opacity and not by absence.
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
    v_flex, v_virtual_list, ActiveTheme, Disableable as _, Sizable as _, StyledExt as _,
};

use crate::git::{BranchKind, LogRange};
use crate::runtime::{Action, Cmd};
use crate::tr;
use crate::ui::app::ClaudhubApp;
use crate::ui::branches::{rows_for, BranchRow, Row, Scope};
use crate::ui::icons::icon;

/// How wide the surface is.
///
/// A branch's name and everything that qualifies it need room: below this, a
/// `wt/` name and the four chips at its shoulder — here, the checkout holding
/// it, what it owes and what it leads by — leave nothing for the name itself.
/// It is `base_select`'s width, and for the same reason.
const WIDTH: gpui::Pixels = px(420.);

/// How tall the list grows before it scrolls.
const LIST_HEIGHT: gpui::Pixels = px(320.);

/// Where the picker is painted.
///
/// The same list and the same actions in both; what differs is what ends a
/// gesture. A popover is dismissed by it — that is what one opened it for —
/// while a tool window stays, so the gesture ends by going back to the list it
/// started from.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum Mode {
    /// The top bar's popover, called up on the branch one is on.
    Popover,
    /// The tool window, which is a zone and not a surface.
    Docked,
}

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
    /// Where one stands. The strongest of the three, since it is the one row a
    /// list of forty is read to find.
    primary: Hsla,
    /// What is to be integrated before anything can be pushed — the lag.
    warning: Hsla,
    /// What is there and nowhere else yet — the lead.
    success: Hsla,
    /// A branch's row in the docked column: two lines of text and next to
    /// nothing around them.
    row: gpui::Pixels,
    /// The same row in the popover, where it carries the name alone.
    compact: gpui::Pixels,
    /// A group's heading: one line, and shorter than a row — it is a rule with
    /// a name on it, not an entry.
    head: gpui::Pixels,
    /// A scope row: one line, and an entry one aims at — so taller than a
    /// heading, and the height of a branch wherever a branch carries one line.
    scope: gpui::Pixels,
}

impl Look {
    fn of(cx: &App) -> Self {
        let unit = crate::ui::theme::row_height(cx);
        Self {
            accent: cx.theme().accent,
            muted: cx.theme().muted_foreground,
            border: cx.theme().border,
            primary: cx.theme().primary,
            warning: cx.theme().warning,
            success: cx.theme().success,
            // Two lines and a hair, not two rows: a list where every entry is
            // twice as tall as it needs to be shows four of them where it could
            // show seven, and what one comes here to do is compare.
            row: unit * 1.45,
            compact: unit * 1.15,
            head: unit * 0.95,
            scope: unit * 1.15,
        }
    }

    /// How tall a branch's row is, which is a question of where it is being
    /// read. See the second line's own comment in `branch_row`.
    fn branch(&self, mode: Mode) -> gpui::Pixels {
        match mode {
            Mode::Docked => self.row,
            Mode::Popover => self.compact,
        }
    }
}

pub(super) struct BranchPicker {
    app: WeakEntity<ClaudhubApp>,
    query: Entity<InputState>,
    mode: Mode,
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
    /// The wheel's smoothing, this view's own.
    ///
    /// The panels keep theirs on the application, keyed by the bar's id,
    /// because a panel is not an entity of its own; this is one, and one list
    /// is one motion — there is nothing to key. Without it the popover was the
    /// one list in the window whose wheel jumped, which reads as a different
    /// application under the same title bar.
    motion: crate::ui::motion::ScrollMotion,
    /// The popover carrying us, so that a gesture can close it. Handed over by
    /// the content closure — a popover's state lives in element state, and that
    /// is the only place it is reachable from.
    popover: Option<Entity<PopoverState>>,
}

impl BranchPicker {
    pub(super) fn new(
        mode: Mode,
        window: &mut Window,
        cx: &mut Context<ClaudhubApp>,
    ) -> Entity<Self> {
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
                mode,
                step: Step::List,
                scroll: gpui_component::VirtualListScrollHandle::new(),
                cursor: 0,
                rows: Rc::new(Vec::new()),
                stale: true,
                folded: [false; 2],
                motion: crate::ui::motion::ScrollMotion::new(crate::ui::motion::Axes::Vertical),
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

    /// Ends the gesture the picker was standing in.
    fn close(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        match self.mode {
            Mode::Popover => {
                if let Some(popover) = self.popover.clone() {
                    popover.update(cx, |state, cx| state.dismiss(window, cx));
                }
            }
            // A zone has nothing to dismiss, and one that emptied itself after
            // every action would be a tool window that closes when used. Back
            // to the list it was opened from, which is where the next gesture
            // starts.
            Mode::Docked => {
                self.step = Step::List;
                cx.notify();
            }
        }
    }

    /// The filter's focus handle, which is what `Ctrl+F` aims at in the tool
    /// window: the panel has a field and it **is** the search, the rule the
    /// project search already follows.
    pub(super) fn filter(&self, cx: &App) -> gpui::FocusHandle {
        self.query.focus_handle(cx)
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
        let Some(worktree) = app.active_path() else {
            return Vec::new();
        };
        let Some(repo) = app.repo_of(&worktree) else {
            return Vec::new();
        };
        let query = self.query.read(cx).value();
        // The scope rows only where the list drives a log — see `Row::Scope`.
        let rows = rows_for(
            &repo.branches,
            &query,
            Some(&worktree),
            matches!(self.mode, Mode::Docked),
        );
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

    /// What a click on a branch means — the third thing the mode decides.
    ///
    /// The popover **checks it out**: that is the gesture one opens it for, ten
    /// times a day, and it has to stay one click. The column beside the log
    /// **shows it** — one reads what a branch carries before deciding to take
    /// it, and a list that switched branch under the pointer is a list one
    /// cannot browse. Checking out from there is still one gesture, on the
    /// row's `…`.
    ///
    /// **A branch already checked out somewhere takes one there instead**, and
    /// that is the same answer to the same question. Git refuses two checkouts
    /// of one branch, so the row used to be greyed and inert — which answers
    /// "I want to work on `wt/fix`" with a refusal, when the window has the
    /// thing being asked for open one worktree away. The row says so before
    /// the click: see `branch_row`.
    fn activate(&mut self, branch: &BranchRow, window: &mut Window, cx: &mut Context<Self>) {
        match self.mode {
            Mode::Popover if branch.taken() => self.go_to_worktree(branch, window, cx),
            Mode::Popover => self.checkout(branch, window, cx),
            Mode::Docked => self.scope(
                LogRange::Ref {
                    name: branch.name.clone(),
                },
                cx,
            ),
        }
    }

    /// Goes to the checkout that already holds this branch.
    fn go_to_worktree(&mut self, branch: &BranchRow, window: &mut Window, cx: &mut Context<Self>) {
        let Some(worktree) = branch.taken_by.clone() else {
            return;
        };
        self.act(window, cx, move |app, window, cx| {
            app.select_worktree(worktree, window, cx)
        });
    }

    /// Points the log beside the list at something else.
    fn scope(&mut self, range: LogRange, cx: &mut Context<Self>) {
        let Some(app) = self.app.upgrade() else {
            return;
        };
        app.update(cx, |app, cx| app.set_history_range(range, cx));
        cx.notify();
    }

    /// What that log is showing, for the row that says so. `None` in the
    /// popover, which drives no log.
    fn log_range(&self, cx: &App) -> Option<LogRange> {
        if !matches!(self.mode, Mode::Docked) {
            return None;
        }
        let app = self.app.upgrade()?;
        let app = app.read(cx);
        Some(app.active_review()?.history_range.clone())
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
            // A heading is not somewhere one can land; a scope is.
            |ix| !matches!(rows[ix], Row::Group { .. }),
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
                match rows.get(self.cursor).cloned() {
                    Some(Row::Branch(row)) => self.activate(&row, window, cx),
                    Some(Row::Scope(scope)) => self.scope(range_of(scope), cx),
                    _ => {}
                }
            }
            _ => {}
        }
    }

    fn render_list(&mut self, window: &Window, cx: &mut Context<Self>) -> gpui::AnyElement {
        let look = Look::of(cx);
        // The transition, one step per frame. It asks for the next frame itself
        // for as long as it is moving.
        let base = crate::ui::scroll::Scrollable::base(&self.scroll);
        self.motion.advance(&base, window);
        // A popover is as tall as what it holds, up to a ceiling; a zone is as
        // tall as it was dragged, and the list is what takes what is left over
        // once the field and the footer have had theirs.
        let docked = matches!(self.mode, Mode::Docked);
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
                    Row::Group { .. } => gpui::size(px(0.), look.head),
                    Row::Scope(_) => gpui::size(px(0.), look.scope),
                    Row::Branch(_) => gpui::size(px(0.), look.branch(self.mode)),
                })
                .collect::<Vec<_>>(),
        );
        let entity = cx.entity();
        // What the log is pointed at, read once: the closure below runs for
        // every visible row on every frame.
        let log = self.log_range(cx);
        let mode = self.mode;
        let build = {
            let rows = rows.clone();
            move |ix: usize, cx: &mut App| match &rows[ix] {
                Row::Group { kind, count } => {
                    group_heading(&entity, ix, *kind, *count, folded[group_ix(*kind)], look)
                }
                Row::Scope(scope) => {
                    let shown = log.as_ref() == Some(&range_of(*scope));
                    scope_row(&entity, ix, *scope, shown, ix == cursor, look)
                }
                Row::Branch(row) => {
                    let shown = matches!(&log, Some(LogRange::Ref { name }) if *name == row.name);
                    branch_row(&entity, ix, row, ix == cursor, mode, shown, look, cx)
                }
            }
        };
        v_flex()
            .w_full()
            .min_h_0()
            .when(docked, |el| el.flex_1())
            .child(
                div().w_full().px_1().py_1().child(
                    Input::new(&self.query)
                        .xsmall()
                        // What the field does is filter, and a bare box above a
                        // list reads as somewhere to type a name. The glyph is
                        // the one the panels' own `Ctrl+F` wears.
                        .prefix(icon("search").xsmall().text_color(look.muted)),
                ),
            )
            .child(if count == 0 {
                div()
                    .w_full()
                    .p_3()
                    .text_sm()
                    .text_color(look.muted)
                    .when(docked, |el| el.flex_1())
                    .child(tr!("branch-none"))
                    .into_any_element()
            } else {
                crate::ui::scroll::smooth_wheel(
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
                    ),
                    base,
                    |this| &mut this.motion,
                    cx,
                )
                .when(docked, |el| el.flex_1().min_h_0())
                .when(!docked, |el| el.h(LIST_HEIGHT))
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
                    .small()
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
                    .small()
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
        // **Beside the checkout it stands in for**, and only where there is a
        // checkout to name. It is the exception to this surface's rule that a
        // refused action stays on the list greyed, and the reason is in the
        // label: that rule exists so one reads *why* a gesture is unavailable,
        // and "go to the worktree" for a branch held in none is not a refusal
        // but a sentence with a hole in it. What it does is what the row's own
        // click does; it is here so the gesture has a name, which is how one
        // finds out it exists.
        if let Some(worktree) = row.taken_by.clone().filter(|_| !row.is_head) {
            let label = worktree
                .file_name()
                .unwrap_or_default()
                .to_string_lossy()
                .into_owned();
            list = list.child(self.action(
                "go-to-worktree",
                "arrow-right",
                tr!("branch-open-worktree", { name: label }),
                true,
                {
                    let row = row.clone();
                    move |this, window, cx| this.go_to_worktree(&row, window, cx)
                },
                look,
                cx,
            ));
        }
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
            // **One entry for both halves.** The dialog carries the box that
            // takes `origin` with it — unticked, since that half is the one
            // nobody undoes for you. Two entries meant doing the gesture twice
            // to finish with a branch, which is what one does every time.
            list = list.child(self.action(
                "delete",
                "trash-2",
                tr!("branch-delete"),
                checkable,
                {
                    let (branch, has_remote) = (name.clone(), has_remote);
                    move |this, window, cx| {
                        let branch = branch.clone();
                        this.act(window, cx, move |app, window, cx| {
                            app.confirm_delete_branch(branch, has_remote, window, cx)
                        })
                    }
                },
                look,
                cx,
            ));
        }
        // A remote-tracking name has no local half to delete: there the gesture
        // is the remote one alone, and it says so.
        if remote {
            list = list.child(separator(look));
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
                    .small()
                    .icon(icon("arrow-left"))
                    .tooltip(tr!("branch-back"))
                    .on_click(cx.listener(|this, _, _window, cx| {
                        this.step = Step::List;
                        cx.notify();
                    })),
            )
            .child(icon("git-branch").xsmall().text_color(look.muted))
            // The same shoulder as in the list: the name takes what it needs,
            // its chips follow it, and the leftover room goes after them.
            .child(
                div()
                    .min_w_0()
                    .truncate()
                    .text_sm()
                    .font_semibold()
                    .child(SharedString::from(row.name.clone())),
            )
            .when(row.is_head, |el| {
                el.child(crate::ui::theme::chip(tr!("branch-here"), look.primary))
            })
            .when(row.kind == BranchKind::Remote, |el| {
                el.child(crate::ui::theme::chip(tr!("branch-remote"), look.muted))
            })
            .child(div().flex_1())
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
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let body = match &self.step {
            Step::List => self.render_list(window, cx),
            Step::Actions(row) => {
                let row = row.clone();
                self.render_actions(&row, cx)
            }
        };
        v_flex()
            .min_h_0()
            // A popover is a surface of its own and says how wide it is; a tool
            // window takes the zone it was given.
            .when(matches!(self.mode, Mode::Popover), |el| el.w(WIDTH))
            .when(matches!(self.mode, Mode::Docked), |el| el.size_full())
            .capture_key_down(cx.listener(Self::on_key))
            .child(body)
    }
}

/// The range a scope row asks for.
fn range_of(scope: Scope) -> LogRange {
    match scope {
        Scope::Head => LogRange::Head,
        Scope::All => LogRange::All,
    }
}

/// One of the two rows above the branches: what the log is pointed at when it
/// is pointed at no branch in particular.
///
/// Flush left, where a branch is set in under its heading: these belong to no
/// group, and indenting them would read as if they did.
fn scope_row(
    picker: &Entity<BranchPicker>,
    index: usize,
    scope: Scope,
    shown: bool,
    at_cursor: bool,
    look: Look,
) -> gpui::AnyElement {
    let picker = picker.clone();
    h_flex()
        .id(("branch-scope", index))
        .h(look.scope)
        .w_full()
        .pl_1()
        .pr(crate::ui::theme::scroll_gutter())
        .gap_1()
        .items_center()
        .cursor_pointer()
        .when(shown, |el| el.bg(look.accent))
        .when(at_cursor && !shown, |el| el.bg(look.accent.opacity(0.5)))
        .hover(|s| s.bg(look.accent.opacity(0.4)))
        .on_click(move |_, _window, cx| {
            picker.update(cx, |this, cx| this.scope(range_of(scope), cx));
        })
        .child(
            icon(match scope {
                Scope::Head => "crosshair",
                Scope::All => "list-tree",
            })
            .xsmall()
            .text_color(look.muted),
        )
        .child(
            div()
                .flex_1()
                .min_w_0()
                .truncate()
                .text_sm()
                .when(shown, |el| el.font_semibold())
                .child(match scope {
                    Scope::Head => tr!("history-head"),
                    Scope::All => tr!("history-all"),
                }),
        )
        .into_any_element()
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
            Row::Group { kind, .. } => {
                shut = folded[group_ix(*kind)];
                true
            }
            Row::Branch(_) => !shut,
            // Above the groups, and belonging to none of them.
            Row::Scope(_) => true,
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
    count: usize,
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
        // **How many are under it**, and it is what a closed group has left to
        // say: a heading over nothing tells one the group is shut, not that
        // eighty-seven remotes are behind it. Beside the title rather than at
        // the far right, for the reason "here" sits beside its name: it
        // qualifies the word, so it belongs against it.
        .child(
            div()
                .flex_none()
                .text_xs()
                .text_color(look.muted)
                .child(count.to_string()),
        )
        .child(div().flex_1())
        .into_any_element()
}

/// One branch: what it is called, what it carries, and the way into its actions.
///
/// A band and not a pill — the window's rule for every list: what a selected row
/// designates is the *row*, which is what the click and the `…` act on.
///
/// The line reads left to right in the order one asks the questions: which
/// branch, where it stands, what it is worth. The name comes first and the
/// chips that qualify it sit **at its shoulder**; the two counts are pushed to
/// the right edge, where they line up down the list and can be compared
/// without reading a word.
#[allow(clippy::too_many_arguments)]
fn branch_row(
    picker: &Entity<BranchPicker>,
    index: usize,
    row: &BranchRow,
    at_cursor: bool,
    mode: Mode,
    // `shown`: it is what the log beside the list is showing.
    shown: bool,
    look: Look,
    _cx: &mut App,
) -> gpui::AnyElement {
    // The worktree holding it, as a chip beside the name and no longer in
    // place of what the branch carries: those are two questions, and answering
    // the first used to throw the second away — the subject that tells
    // `wt/fix-a` from `wt/fix-b` is exactly what one reads when choosing
    // between two branches, checked out elsewhere or not.
    let taken = row.taken_by.as_ref().filter(|_| !row.is_head).map(|path| {
        SharedString::from(
            path.file_name()
                .unwrap_or_default()
                .to_string_lossy()
                .into_owned(),
        )
    });
    let checkable = !row.is_head && !row.taken();
    // A click here goes to the checkout that holds it — see `activate`. Only in
    // the popover: the docked column's click points the log at the branch, which
    // is the one thing it does for every row, and an arrow promising a journey
    // there would promise the wrong one.
    let travels = matches!(mode, Mode::Popover) && row.taken();
    // **Every row answers in the docked list.** What a click does there is show
    // the branch's commits, and there is no branch one cannot read — not the
    // one checked out here, not the one checked out next door.
    let clickable = checkable || travels || matches!(mode, Mode::Docked);
    let (for_click, for_menu) = (picker.clone(), picker.clone());
    let (clicked, opened) = (row.clone(), row.clone());
    // The `…` comes out on the row under the pointer and on the row the list is
    // standing on, and stays out of the way everywhere else: forty rows each
    // carrying a permanent `…` is a column of dots one reads before the branch
    // names. One is always on screen — the row one is on — which is what keeps
    // the gesture discoverable, the rule the panels' magnifier already follows.
    //
    // Hidden by opacity and not by absence: the row must not change shape under
    // the pointer, and nothing can be clicked blind — the hover that reveals it
    // is the same one that has to happen before it can be aimed at.
    let group = SharedString::from(format!("branch-row-{index}"));
    let armed = at_cursor || shown;

    h_flex()
        .id(("branch-row", index))
        .group(group.clone())
        .h(look.branch(mode))
        .w_full()
        // **What the click will do**, wherever it is not the obvious thing.
        // A row that takes one to another checkout says so in words: the arrow
        // in its chip is what one sees, this is what one reads, and between the
        // two nobody clicks to find out. It wins over the subject below, which
        // is a nicety where this is the difference between two gestures.
        //
        // Otherwise the subject, when the row is not showing it — see the
        // second line further down. A popover of forty branches is read to find
        // one, and what one finds it by is its name.
        .when_some(
            match (travels, taken.clone()) {
                (true, Some(name)) => Some(tr!("branch-go-to-worktree", { name: name })),
                _ => (!row.detail.is_empty() && !matches!(mode, Mode::Docked))
                    .then(|| SharedString::from(row.detail.clone())),
            },
            |el, text| {
                el.tooltip(move |window, cx| {
                    gpui_component::tooltip::Tooltip::new(text.clone()).build(window, cx)
                })
            },
        )
        // The rule down the left edge of the branch one is on: the mark every
        // editor puts on the file it has open, and what makes that row findable
        // in a list of forty without reading a word of it. The chip says which
        // one, the rule says where — one is read, the other is seen.
        //
        // **Every row carries the border, and all but one carry it in nothing.**
        // Adding it to a single row would move that row's text two pixels right
        // of its neighbours', which reads as a misalignment rather than as a
        // mark.
        .border_l_2()
        .border_color(match row.is_head {
            true => look.primary,
            false => gpui::transparent_black(),
        })
        // Set in from the heading: a flat column under a title reads as a list
        // that happens to have a title above it, not as the group's branches.
        // It is the chevron's own width.
        .pl_5()
        .pr(crate::ui::theme::scroll_gutter())
        .gap_1()
        .items_center()
        // The band says what the log is on; the fainter one says where the
        // keyboard is. Two different questions, so two different weights.
        .when(shown, |el| el.bg(look.accent))
        .when(at_cursor && !shown, |el| el.bg(look.accent.opacity(0.5)))
        .when(clickable, |el| {
            el.cursor_pointer()
                .hover(|s| s.bg(look.accent.opacity(0.4)))
                .on_click(move |_, window, cx| {
                    for_click.update(cx, |this, cx| this.activate(&clicked, window, cx));
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
                        // **The name takes what it needs and no more.** It was
                        // `flex_1`, which took the whole line and left "here"
                        // against the right edge — a chip so far from the name
                        // it qualifies that it read as a column of its own,
                        // and on a short name half the row was empty between
                        // the two. Shrinkable and `min_w_0` instead: it
                        // truncates only once there is nothing left to give,
                        // and what is left over goes to the spacer below.
                        .child(
                            div()
                                .min_w_0()
                                .truncate()
                                .text_sm()
                                .when(row.is_head, |el| el.font_semibold())
                                .child(SharedString::from(row.name.clone())),
                        )
                        .when(row.is_head, |el| {
                            el.child(crate::ui::theme::chip(tr!("branch-here"), look.primary))
                        })
                        // **The name is no longer set back** when the branch is
                        // checked out elsewhere. Grey is what a window says of
                        // something it refuses, and this row is not refused any
                        // more: it goes somewhere.
                        .children(taken.map(|name| worktree_chip(name, look.muted, travels)))
                        // **The counts belong to the name's line.** Behind
                        // before ahead: that is what has to be integrated
                        // before anything can be pushed. Warning against
                        // success, the pair the rest of the window uses for
                        // owed and gained.
                        //
                        // They were centred against both lines, at the right
                        // edge of the row — a column one could read down, but
                        // one that answered about a branch while sitting level
                        // with its commit subject. What "↓2" says is something
                        // about the *name*, so it goes where the name is, at
                        // its shoulder with the rest of what qualifies it.
                        .when(row.behind > 0, |el| {
                            el.child(crate::ui::theme::chip(
                                SharedString::from(format!("↓{}", row.behind)),
                                look.warning,
                            ))
                        })
                        .when(row.ahead > 0, |el| {
                            el.child(crate::ui::theme::chip(
                                SharedString::from(format!("↑{}", row.ahead)),
                                look.success,
                            ))
                        })
                        // The room nobody claimed. It is what holds the chips
                        // at the name's shoulder instead of at the edge.
                        .child(div().flex_1()),
                )
                // **The subject and its date belong to the docked column, not
                // to the popover.** The two seats are two gestures: one comes
                // to the popover to *pick* a branch, and picks it by name, so
                // forty rows two lines deep put half of them off screen for a
                // line read on none of them; the column beside the graph is
                // where a branch is read at length, and there the same line is
                // what says whether it is the one. In the popover it is the
                // row's tooltip.
                .when(
                    !row.detail.is_empty() && matches!(mode, Mode::Docked),
                    |el| {
                        el.child(
                            div()
                                .w_full()
                                .min_w_0()
                                .truncate()
                                .text_xs()
                                .text_color(look.muted)
                                .child(row.detail.clone()),
                        )
                    },
                ),
        )
        .child(
            div()
                .flex_none()
                .when(!armed, |el| {
                    el.opacity(0.).group_hover(group, |s| s.opacity(1.))
                })
                .child(
                    Button::new(("branch-actions", index))
                        .ghost()
                        .small()
                        .icon(icon("ellipsis"))
                        .tooltip(tr!("branch-actions"))
                        .on_click(move |_, _window, cx| {
                            // The row is clickable too, and its click checks
                            // out: without this the `…` would switch branch on
                            // its way to the menu.
                            cx.stop_propagation();
                            for_menu.update(cx, |this, cx| {
                                this.step = Step::Actions(opened.clone());
                                cx.notify();
                            });
                        }),
                ),
        )
        .into_any_element()
}

fn separator(look: Look) -> impl IntoElement {
    div().w_full().my_0p5().h(px(1.)).bg(look.border)
}

/// The worktree a branch is already checked out in.
///
/// The glyph is what makes it legible without a word: a name alone in a chip
/// says nothing about *what* is named, and "checked out in" spelled out is
/// three words on a line that has two already.
///
/// **Which glyph is which gesture**, and that is the point of `travels`: the
/// arrow is the one the multiplexer's tiles carry for "take me to this
/// checkout", so where the row's click does that, the chip wears it and reads
/// as a destination. Where it does not — the docked column, whose click points
/// the log at the branch — the folder stays, and states a fact.
fn worktree_chip(name: SharedString, colour: Hsla, travels: bool) -> impl IntoElement {
    crate::ui::theme::chip_base(colour)
        .child(icon(if travels { "arrow-right" } else { "folder" }).xsmall())
        .child(name)
}

impl ClaudhubApp {
    /// Pull and push, beside the branch they are about — **and only when there
    /// is something to pull or to push**.
    ///
    /// The same two gestures live in the changes panel's bar, where they are
    /// always painted because that bar is about the working tree and one goes to
    /// it in order to act. The title bar is not that: it is on screen at every
    /// moment, the hours where the branch is level with its remote included, and
    /// a button that does nothing for hours is a button one stops seeing. Here
    /// they appear because there is work to do, which is what makes them worth a
    /// glance.
    ///
    /// **The glyph alone, no count.** The picker's trigger sits one pixel to the
    /// left and already says how far behind and how far ahead; the same two
    /// numbers again would read as four things. The colours are that trigger's —
    /// warning for what is owed, success for what is gained — so the eye joins a
    /// count to its button without a word, and the gloss says the number for the
    /// hand that wants it.
    pub(super) fn render_sync_buttons(&self, cx: &mut Context<Self>) -> Vec<gpui::AnyElement> {
        let Some((ahead, behind)) = self
            .active_review()
            .map(|review| (review.status.ahead, review.status.behind))
        else {
            return Vec::new();
        };
        let (pulling, pushing) = (
            self.active_running(Action::Pull),
            self.active_running(Action::Push),
        );
        let (warning, success) = (cx.theme().warning, cx.theme().success);
        let mut out: Vec<gpui::AnyElement> = Vec::new();
        // Behind before ahead, the list's order: what has to be integrated comes
        // before what can be sent.
        if behind > 0 {
            out.push(
                Button::new("topbar-pull")
                    .ghost()
                    .small()
                    .icon(icon("arrow-down-to-line").text_color(warning))
                    .tooltip(tr!("action-pull-behind", { count: behind }))
                    .loading(pulling)
                    .disabled(pulling)
                    .on_click(cx.listener(|this, _, _, cx| {
                        if let Some(worktree) = this.active.clone() {
                            let cmd = Cmd::Pull {
                                worktree: worktree.clone(),
                            };
                            this.start(Some(worktree), Action::Pull, cmd, cx);
                        }
                    }))
                    .into_any_element(),
            );
        }
        if ahead > 0 {
            out.push(
                Button::new("topbar-push")
                    .ghost()
                    .small()
                    .icon(icon("arrow-up-from-line").text_color(success))
                    .tooltip(tr!("action-push-ahead", { count: ahead }))
                    .loading(pushing)
                    .disabled(pushing)
                    .on_click(cx.listener(|this, _, _, cx| {
                        if let Some(worktree) = this.active.clone() {
                            let cmd = Cmd::Push {
                                worktree: worktree.clone(),
                                force_with_lease: false,
                            };
                            this.start(Some(worktree), Action::Push, cmd, cx);
                        }
                    }))
                    .into_any_element(),
            );
        }
        out
    }

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
        // The same two chips as in the list below it: the lead and the lag are
        // read in the same glance whether one is looking at the bar or at the
        // branch it names, and two spellings of one thing are two things.
        let (warning, success) = (cx.theme().warning, cx.theme().success);
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
                            .when(behind > 0, |el| {
                                el.child(crate::ui::theme::chip(
                                    SharedString::from(format!("↓{behind}")),
                                    warning,
                                ))
                            })
                            .when(ahead > 0, |el| {
                                el.child(crate::ui::theme::chip(
                                    SharedString::from(format!("↑{ahead}")),
                                    success,
                                ))
                            })
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
                Row::Group {
                    kind: BranchKind::Local,
                    ..
                } => "== locales".into(),
                Row::Group {
                    kind: BranchKind::Remote,
                    ..
                } => "== distantes".into(),
                Row::Scope(scope) => format!("{scope:?}"),
                Row::Branch(row) => row.name.clone(),
            })
            .collect()
    }

    /// A folded group keeps its heading: that heading **is** the fold, and
    /// removing it too would leave no way back.
    #[test]
    fn a_folded_group_keeps_its_heading_and_loses_its_branches() {
        let rows = vec![
            Row::Group {
                kind: BranchKind::Local,
                count: 1,
            },
            branch("main", BranchKind::Local),
            Row::Group {
                kind: BranchKind::Remote,
                count: 1,
            },
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
