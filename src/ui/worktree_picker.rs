//! The worktree picker: the repositories, their checkouts, and what one asks of
//! each.
//!
//! The same two-step surface as `ui::branch_picker`, for the same two reasons —
//! a `PopupMenu` has no filter field, and a popup opened from a row inside a
//! scrolling menu is clipped by the scroll it needs. Step one is the filtered
//! list of checkouts, grouped under their repository; step two is one
//! worktree's actions.
//!
//! Clicking a row **selects** the worktree, which is the gesture that drives
//! every other panel and has to stay one click. The `…` at the row's end opens
//! the actions, which are `ClaudhubApp::worktree_actions` — the very table the
//! top bar's `…` folds into a menu. One table, two renderings: two lists would
//! have drifted at the first addition.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
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

use crate::tr;
use crate::ui::app::ClaudhubApp;
use crate::ui::icons::icon;
use crate::ui::worktrees::{self, Item, Row};

/// How wide the surface is. Narrower than the branch picker's: what a row
/// carries here is a folder name and a branch, not a commit subject.
const WIDTH: gpui::Pixels = px(360.);
const LIST_HEIGHT: gpui::Pixels = px(320.);

enum Step {
    List,
    /// One worktree's actions. Its name comes along so the header can say which
    /// worktree one is standing on without walking the list again.
    Actions {
        main: PathBuf,
        worktree: PathBuf,
        label: String,
    },
}

#[derive(Clone, Copy)]
struct Look {
    accent: Hsla,
    accent_foreground: Hsla,
    muted: Hsla,
    border: Hsla,
    warning: Hsla,
    success: Hsla,
    /// A checkout's row: two lines of text and next to nothing around them.
    row: gpui::Pixels,
    /// The same row with `wt`'s line under the branch: three lines. The list
    /// is a `v_virtual_list` and every row says its own height, so a worktree
    /// `wt` knows grows and the others do not.
    tall: gpui::Pixels,
    /// A repository's heading: one line, and shorter than a row — it is a rule
    /// with a name on it, not an entry.
    head: gpui::Pixels,
}

impl Look {
    fn of(cx: &App) -> Self {
        let unit = crate::ui::theme::row_height(cx);
        Self {
            accent: cx.theme().accent,
            accent_foreground: cx.theme().primary,
            muted: cx.theme().muted_foreground,
            border: cx.theme().border,
            warning: cx.theme().warning,
            success: cx.theme().success,
            // Two lines and a hair, not two rows: a list where every entry is
            // twice as tall as it needs to be shows four of them where it could
            // show seven, and what one comes here to do is compare.
            row: unit * 1.45,
            tall: unit * 2.0,
            head: unit * 0.95,
        }
    }
}

pub(super) struct WorktreePicker {
    app: WeakEntity<ClaudhubApp>,
    query: Entity<InputState>,
    step: Step,
    scroll: gpui_component::VirtualListScrollHandle,
    cursor: usize,
    /// The rows on screen, kept between frames — see `rows`.
    rows: Rc<Vec<Row>>,
    /// The list has to be laid out again. Set by the three things that change
    /// it: the filter, a fold, and the application itself.
    stale: bool,
    /// The repositories one has closed.
    ///
    /// What is **folded** and not what is open, the polarity of the review tree:
    /// a picker one opens to change project shows its projects, so the exception
    /// to remember is the one that has been shut. It does not outlive the
    /// window — a fold here is a reading posture, not a preference.
    folded: HashSet<PathBuf>,
    popover: Option<Entity<PopoverState>>,
}

impl WorktreePicker {
    pub(super) fn new(window: &mut Window, cx: &mut Context<ClaudhubApp>) -> Entity<Self> {
        let owner = cx.entity();
        let app = owner.downgrade();
        let query = cx.new(|cx| InputState::new(window, cx).placeholder(tr!("worktree-filter")));
        cx.new(|cx| {
            cx.subscribe(
                &query,
                |this: &mut Self, _, _event: &gpui_component::input::InputEvent, cx| {
                    this.stale = true;
                    cx.notify();
                },
            )
            .detach();
            // The list is a projection of the application's repositories, and
            // they move under it — a worktree created, a summary read, an agent
            // that starts working. Nothing else would tell the prepared list to
            // let go.
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
                folded: HashSet::new(),
                popover: None,
            }
        })
    }

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

    /// The rows on screen, headings included.
    ///
    /// **Kept between frames.** It was laid out again on every frame of the
    /// popover — and it built every checkout's row, its summary, its agent and
    /// its `wt` state read one by one, *before* the filter had a say. What is
    /// listed is `worktrees::rows_for`, which is free of gpui and tested; what
    /// is here is where the data comes from.
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
        // Read once for the whole list rather than once per row: the pins are a
        // single vector in a global, and a row's closure would borrow it again
        // for every checkout of every repository.
        let pinned = &crate::ui::store::Store::global(cx).pinned;
        let repos: Vec<worktrees::Repository> = app
            .repos
            .iter()
            .map(|repo| worktrees::Repository {
                main: repo.main.clone(),
                name: repo.name.clone(),
                checkouts: repo
                    .worktrees
                    .iter()
                    .map(|w| worktrees::Checkout {
                        path: w.path.clone(),
                        label: w.label(),
                        branch: w.branch.clone(),
                        is_main: w.is_main,
                    })
                    .collect(),
            })
            .collect();
        let gone: Vec<worktrees::Gone> = app
            .repos
            .missing()
            .iter()
            .map(|repo| worktrees::Gone {
                path: repo.path.clone(),
                name: repo
                    .path
                    .file_name()
                    .map(|name| name.to_string_lossy().into_owned())
                    .unwrap_or_else(|| repo.path.display().to_string()),
                message: repo.message.clone(),
            })
            .collect();
        let query = self.query.read(cx).value();
        worktrees::rows_for(&repos, &gone, &query, &self.folded, |repo, checkout| Item {
            main: repo.main.clone(),
            path: checkout.path.clone(),
            label: checkout.label.clone(),
            branch: checkout.branch.clone(),
            is_main: checkout.is_main,
            summary: app.summaries.get(&checkout.path).copied(),
            agent: app.agents.get(&checkout.path).cloned(),
            up: app.wt_state(&checkout.path).and_then(|state| state.up),
            detail: app.wt_state(&checkout.path).and_then(worktrees::detail),
            pinned: pinned.contains(&checkout.path),
        })
    }

    fn select(&mut self, path: PathBuf, window: &mut Window, cx: &mut Context<Self>) {
        self.act(window, cx, move |app, window, cx| {
            app.select_worktree(path, window, cx)
        });
    }

    /// Moves the keyboard cursor, stepping over the headings and the dead
    /// repositories: neither is somewhere Enter could take one.
    fn step_cursor(&mut self, delta: isize, cx: &mut Context<Self>) {
        let rows = self.rows(cx);
        let Some(next) = crate::ui::picker::step_cursor(
            rows.len(),
            |ix| matches!(rows[ix], Row::Worktree(_)),
            self.cursor,
            delta,
        ) else {
            return;
        };
        self.cursor = next;
        self.scroll.scroll_to_item(next, ScrollStrategy::Top);
        cx.notify();
    }

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
                if let Some(Row::Worktree(item)) = rows.get(self.cursor).cloned() {
                    self.select(item.path, window, cx);
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
        let active = self
            .app
            .upgrade()
            .and_then(|app| app.read(cx).active_path());
        // A heading is not as tall as an entry, so the list is a
        // `v_virtual_list` and not a `uniform_list` — the same swap the diff's
        // wrapping and the merge view make, and for the same reason.
        let sizes = Rc::new(
            rows.iter()
                .map(|row| match row {
                    Row::Repo { .. } => gpui::size(px(0.), look.head),
                    Row::Worktree(item) if item.detail.is_some() => gpui::size(px(0.), look.tall),
                    _ => gpui::size(px(0.), look.row),
                })
                .collect::<Vec<_>>(),
        );
        let entity = cx.entity();
        let build = {
            let rows = rows.clone();
            move |ix: usize, cx: &mut App| match &rows[ix] {
                Row::Repo { main, name, folded } => {
                    repo_heading(&entity, ix, main, name, *folded, look)
                }
                Row::Worktree(item) => worktree_row(
                    &entity,
                    ix,
                    item,
                    active.as_deref() == Some(item.path.as_path()),
                    ix == cursor,
                    look,
                    cx,
                ),
                Row::Missing {
                    path,
                    name,
                    message,
                } => missing_row(&entity, ix, path, name, message, look),
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
                    .child(tr!("worktree-none"))
                    .into_any_element()
            } else {
                crate::ui::scroll::vertical(
                    "worktree-list",
                    &self.scroll,
                    v_virtual_list(
                        cx.entity(),
                        "worktree-rows",
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
            .child(
                h_flex()
                    .w_full()
                    .px_1()
                    .py_0p5()
                    .items_center()
                    .border_t_1()
                    .border_color(look.border)
                    .child(
                        Button::new("repo-open")
                            .ghost()
                            .xsmall()
                            .icon(icon("folder-plus"))
                            .label(tr!("repo-open"))
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.act(window, cx, |app, window, cx| {
                                    app.prompt_open_repository(window, cx)
                                });
                            })),
                    ),
            )
            .into_any_element()
    }

    fn render_actions(
        &mut self,
        main: &Path,
        worktree: &Path,
        label: &str,
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        let look = Look::of(cx);
        let Some(app) = self.app.upgrade() else {
            return div().into_any_element();
        };
        // Asked of the application here and not kept in the step: the project's
        // `wt.toml` arrives asynchronously, and a list frozen when the `…` was
        // pressed would be the one from before the read.
        let actions = app.update(cx, |app, cx| {
            app.worktree_actions(main.to_path_buf(), worktree.to_path_buf(), cx)
        });
        let mut list = v_flex().w_full().p_1().gap_0p5();
        for action in actions {
            if action.group {
                list = list.child(div().w_full().my_0p5().h(px(1.)).bg(look.border));
            }
            let run = action.run.clone();
            list = list.child(
                h_flex()
                    .id(gpui::ElementId::Name(action.id.clone()))
                    .w_full()
                    .px_2()
                    .py_1()
                    .gap_2()
                    .items_center()
                    .rounded(cx.theme().radius)
                    .cursor_pointer()
                    .hover(|s| s.bg(look.accent.opacity(0.4)))
                    .child(icon(action.icon).xsmall().text_color(look.muted))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .text_sm()
                            .child(action.label.clone()),
                    )
                    .on_click(cx.listener(move |this, _, window, cx| {
                        let run = run.clone();
                        this.act(window, cx, move |app, window, cx| run(app, window, cx));
                    })),
            );
        }
        v_flex()
            .w_full()
            .child(
                h_flex()
                    .w_full()
                    .p_1()
                    .gap_1()
                    .items_center()
                    .border_b_1()
                    .border_color(look.border)
                    .child(
                        Button::new("worktree-back")
                            .ghost()
                            .xsmall()
                            .icon(icon("arrow-left"))
                            .tooltip(tr!("branch-back"))
                            .on_click(cx.listener(|this, _, _window, cx| {
                                this.step = Step::List;
                                cx.notify();
                            })),
                    )
                    .child(icon("folder").xsmall().text_color(look.muted))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .text_sm()
                            .font_semibold()
                            .child(SharedString::from(label.to_string())),
                    ),
            )
            .child(list)
            .into_any_element()
    }
}

impl Render for WorktreePicker {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let body = match &self.step {
            Step::List => self.render_list(cx),
            Step::Actions {
                main,
                worktree,
                label,
            } => {
                let (main, worktree, label) = (main.clone(), worktree.clone(), label.clone());
                self.render_actions(&main, &worktree, &label, cx)
            }
        };
        v_flex()
            .w(WIDTH)
            .min_h_0()
            .capture_key_down(cx.listener(Self::on_key))
            .child(body)
    }
}

/// A repository's heading: the fold, the name, and the one action that belongs
/// to it.
///
/// **The whole line folds**, not the chevron alone: a chevron is eight pixels
/// wide, and what one is aiming at is the repository. The `+` consumes its
/// click — the sidebar's `+` came up here when the sidebar went away, and a
/// gesture that only exists in a panel one has hidden is a gesture one no longer
/// has.
fn repo_heading(
    picker: &Entity<WorktreePicker>,
    index: usize,
    main: &Path,
    name: &str,
    folded: bool,
    look: Look,
) -> gpui::AnyElement {
    let (for_fold, for_new) = (picker.clone(), picker.clone());
    let (fold_main, new_main) = (main.to_path_buf(), main.to_path_buf());
    h_flex()
        .id(("repo-heading", index))
        .h(look.head)
        .w_full()
        .pl_1()
        .pr(crate::ui::theme::scroll_gutter())
        .gap_1()
        .items_center()
        .cursor_pointer()
        .hover(|s| s.bg(look.accent.opacity(0.3)))
        .on_click(move |_, _window, cx| {
            let main = fold_main.clone();
            for_fold.update(cx, |this, cx| {
                if !this.folded.remove(&main) {
                    this.folded.insert(main);
                }
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
                .child(SharedString::from(name.to_string())),
        )
        .child(
            Button::new(("new-worktree", index))
                .ghost()
                .xsmall()
                .icon(icon("plus"))
                .tooltip(tr!("worktree-new"))
                .on_click(move |_, window, cx| {
                    // The heading folds on click: without this the `+` would
                    // shut the repository it is about to add to.
                    cx.stop_propagation();
                    let main = new_main.clone();
                    for_new.update(cx, |this, cx| {
                        this.act(window, cx, move |app, window, cx| {
                            app.prompt_new_worktree(main, window, cx)
                        });
                    });
                }),
        )
        .into_any_element()
}

/// One checkout: what it is called, what is happening in it, and the way into
/// its actions.
#[allow(clippy::too_many_arguments)]
fn worktree_row(
    picker: &Entity<WorktreePicker>,
    index: usize,
    item: &Item,
    selected: bool,
    at_cursor: bool,
    look: Look,
    cx: &mut App,
) -> gpui::AnyElement {
    let (for_click, for_menu, for_pin) = (picker.clone(), picker.clone(), picker.clone());
    let target = item.path.clone();
    let pin_target = item.path.clone();
    let opened = item.clone();
    let height = if item.detail.is_some() {
        look.tall
    } else {
        look.row
    };
    h_flex()
        .id(("worktree-row", index))
        .h(height)
        .w_full()
        // Set in from the heading: a flat column under a title reads as a list
        // that happens to have a title above it, not as the repository's
        // checkouts. It is the chevron's own width, so the folder icon lands
        // where the chevron is.
        .pl_5()
        .pr(crate::ui::theme::scroll_gutter())
        .gap_1()
        .items_center()
        .cursor_pointer()
        .when(at_cursor, |el| el.bg(look.accent.opacity(0.5)))
        .hover(|s| s.bg(look.accent.opacity(0.4)))
        .on_click(move |_, window, cx| {
            let target = target.clone();
            for_click.update(cx, |this, cx| this.select(target, window, cx));
        })
        .child(
            icon(if item.is_main { "folder" } else { "git-branch" })
                .xsmall()
                .text_color(look.muted),
        )
        .child(
            v_flex()
                .flex_1()
                .min_w_0()
                .justify_center()
                .child(
                    div()
                        .w_full()
                        .truncate()
                        .text_sm()
                        .when(selected, |el| el.font_semibold())
                        .child(SharedString::from(item.label.clone())),
                )
                .when_some(item.branch.clone(), |el, branch| {
                    el.child(
                        div()
                            .w_full()
                            .truncate()
                            .text_xs()
                            .text_color(look.muted)
                            .child(branch),
                    )
                })
                // What `wt` knows beyond started/stopped — the options chosen,
                // the ports, the project's `[status.info]` — dimmer than the
                // branch: it is read when two worktrees of one branch have to
                // be told apart, not on every glance. Cut to the width, whole
                // in the tooltip.
                .when_some(item.detail.clone(), |el, detail| {
                    el.child(
                        div()
                            .id(("worktree-detail", index))
                            .w_full()
                            .truncate()
                            .text_xs()
                            .text_color(look.muted.opacity(0.75))
                            .tooltip({
                                let full = SharedString::from(detail.full);
                                move |window, cx| {
                                    gpui_component::tooltip::Tooltip::new(full.clone())
                                        .build(window, cx)
                                }
                            })
                            .child(SharedString::from(detail.line)),
                    )
                }),
        )
        // What the project says of it — started or not — then who is working in
        // it, then how much is in progress. Three things read out of the corner
        // of the eye, and the three one chooses a worktree on.
        .when_some(item.up, |el, up| {
            el.child(
                div()
                    .flex_none()
                    .size(px(7.))
                    .rounded_full()
                    .when(up, |el| el.bg(look.success))
                    .when(!up, |el| {
                        el.border_1().border_color(look.muted.opacity(0.8))
                    }),
            )
        })
        .when_some(item.agent.as_ref(), |el, agent| {
            el.child(crate::ui::topbar::agent_badge(agent, cx))
        })
        .when_some(
            item.summary.filter(|summary| !summary.is_empty()),
            |el, summary| el.child(crate::ui::topbar::volume(summary, cx)),
        )
        // The pin, before the `…` and **painted on every row**, ticked or not:
        // a control that appears under the pointer moves what is beside it, and
        // what is beside it here is the volume one was reading. It is the same
        // toggle as the entry in the actions — a table read twice — brought out
        // where the eye already is, because pinning is decided while scanning
        // the list, not after opening a menu about one row.
        .child(
            Button::new(("worktree-pin", index))
                .ghost()
                .xsmall()
                .icon(
                    icon(if item.pinned { "pin-off" } else { "pin" }).text_color(if item.pinned {
                        look.accent_foreground
                    } else {
                        look.muted.opacity(0.6)
                    }),
                )
                .tooltip(if item.pinned {
                    tr!("worktree-unpin")
                } else {
                    tr!("worktree-pin")
                })
                .on_click(move |_, _window, cx| {
                    // The row selects; without this, pinning would change
                    // worktree on its way.
                    cx.stop_propagation();
                    let target = pin_target.clone();
                    // The popover **stays open**, unlike every other gesture
                    // here: pinning is not going somewhere, and one pins two or
                    // three in a row while looking at the same list.
                    for_pin.update(cx, |this, cx| {
                        if let Some(app) = this.app.upgrade() {
                            app.update(cx, |app, cx| app.toggle_pin(&target, cx));
                        }
                        cx.notify();
                    });
                }),
        )
        .child(
            Button::new(("worktree-actions", index))
                .ghost()
                .xsmall()
                .icon(icon("ellipsis"))
                .tooltip(tr!("worktree-actions"))
                .on_click(move |_, _window, cx| {
                    // The row selects; without this the `…` would change worktree
                    // on its way to the actions.
                    cx.stop_propagation();
                    let opened = opened.clone();
                    for_menu.update(cx, |this, cx| {
                        this.step = Step::Actions {
                            main: opened.main.clone(),
                            worktree: opened.path.clone(),
                            label: opened.label.clone(),
                        };
                        cx.notify();
                    });
                }),
        )
        .into_any_element()
}

/// A repository that no longer opens, and the button that forgets it.
///
/// It stays on the list because a repository that appears nowhere cannot be
/// removed either: a moved folder, an erased clone, an unmounted partition left
/// two warnings in the log at every start and no way out but editing the
/// settings file by hand. What git answered is shown, and not just "not found":
/// it is what says whether the folder moved or the disk is missing.
fn missing_row(
    picker: &Entity<WorktreePicker>,
    index: usize,
    path: &Path,
    name: &str,
    message: &str,
    look: Look,
) -> gpui::AnyElement {
    let (picker, path) = (picker.clone(), path.to_path_buf());
    h_flex()
        .id(("missing-repo", index))
        .h(look.row)
        .w_full()
        .pl_2()
        .pr(crate::ui::theme::scroll_gutter())
        .gap_2()
        .items_center()
        .child(icon("triangle-alert").xsmall().text_color(look.warning))
        .child(
            v_flex()
                .flex_1()
                .min_w_0()
                .justify_center()
                .child(
                    div()
                        .w_full()
                        .truncate()
                        .text_sm()
                        .text_color(look.muted)
                        .child(SharedString::from(name.to_string())),
                )
                .child(
                    div()
                        .w_full()
                        .truncate()
                        .text_xs()
                        .text_color(look.warning)
                        .child(SharedString::from(message.to_string())),
                ),
        )
        .child(
            Button::new(("forget-repo", index))
                .ghost()
                .xsmall()
                .icon(icon("x"))
                .tooltip(tr!("repo-forget"))
                .on_click(move |_, window, cx| {
                    let path = path.clone();
                    picker.update(cx, |this, cx| {
                        this.act(window, cx, move |app, window, cx| {
                            app.forget_repository(path, window, cx)
                        });
                    });
                }),
        )
        .into_any_element()
}

impl ClaudhubApp {
    /// The worktree picker's button, and the surface it opens.
    ///
    /// It says the same thing the sidebar's selection used to, and it is not a
    /// duplicate: the sidebar was a panel one could hide, drag or replace with a
    /// terminal, and what drives every view of the window cannot go away with
    /// it.
    pub(super) fn render_worktree_picker(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let label = self
            .active_worktree()
            .map(|w| w.label())
            .unwrap_or_else(|| tr!("no-worktree").to_string());
        // The repository's name in front, greyed: two worktrees called `main` in
        // two repositories is the normal case, not the exception.
        let repo = self
            .active_path()
            .and_then(|path| self.repo_of(&path))
            .map(|repo| repo.name.clone());
        let muted = cx.theme().muted_foreground;
        let picker = self.worktree_picker.clone();
        let focus = picker.read(cx).query.read(cx).focus_handle(cx);
        let for_open = picker.clone();
        Popover::new("worktree-picker")
            .track_focus(&focus)
            .trigger(
                Button::new("worktree-picker-trigger")
                    .ghost()
                    .small()
                    .child(
                        h_flex()
                            .gap_1()
                            .items_center()
                            .child(icon("folder").xsmall().text_color(muted))
                            .when_some(repo, |el, name| {
                                el.child(div().text_sm().text_color(muted).child(name))
                                    .child(
                                        div().text_sm().text_color(muted.opacity(0.5)).child("/"),
                                    )
                            })
                            .child(
                                div()
                                    .max_w(px(240.))
                                    .truncate()
                                    .text_sm()
                                    .font_semibold()
                                    .child(label),
                            )
                            .child(icon("chevron-down").xsmall().text_color(muted)),
                    ),
            )
            .on_open_change(move |open, window, cx| {
                if *open {
                    for_open.update(cx, |this, cx| this.reset(window, cx));
                }
            })
            .content(move |_state, _window, cx| {
                let popover = cx.entity();
                picker.update(cx, |this, _| this.popover = Some(popover));
                picker.clone()
            })
            .appearance(true)
            .p_0()
    }
}
