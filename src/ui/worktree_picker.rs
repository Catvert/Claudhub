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

use gpui_kit::component::{
    button::{Button, ButtonVariants as _},
    h_flex,
    input::InputState,
    popover::Popover,
    v_flex, ActiveTheme, Sizable as _, StyledExt as _,
};
use gpui_kit::{
    div, prelude::*, px, App, Context, Entity, Focusable as _, Hsla, SharedString, Window,
};

use crate::tr;
use crate::ui::app::ClaudhubApp;
use crate::ui::icons::icon;
use crate::ui::picker::{Pick, PickerCore, Step};
use crate::ui::worktrees::{self, Item, Row};

/// How wide the surface is. Narrower than the branch picker's: what a row
/// carries here is a folder name and a branch, not a commit subject.
const WIDTH: gpui_kit::Pixels = px(360.);

/// The worktree whose actions the second step shows. Its name comes along so
/// the header can say which worktree one is standing on without walking the
/// list again.
pub(super) struct Opened {
    main: PathBuf,
    worktree: PathBuf,
    label: String,
}

#[derive(Clone, Copy)]
struct Look {
    accent: Hsla,
    /// Where one stands: the rule down the current checkout's left edge, its
    /// "here", and the pin one has ticked.
    primary: Hsla,
    muted: Hsla,
    border: Hsla,
    warning: Hsla,
    success: Hsla,
    /// A checkout's row: two lines of text and next to nothing around them.
    row: gpui_kit::Pixels,
    /// A repository's heading: one line, and shorter than a row — it is a rule
    /// with a name on it, not an entry.
    head: gpui_kit::Pixels,
}

impl Look {
    fn of(cx: &App) -> Self {
        let unit = crate::ui::theme::row_height(cx);
        Self {
            accent: cx.theme().accent,
            primary: cx.theme().primary,
            muted: cx.theme().muted_foreground,
            border: cx.theme().border,
            warning: cx.theme().warning,
            success: cx.theme().success,
            // Two lines and a hair, not two rows: a list where every entry is
            // twice as tall as it needs to be shows four of them where it could
            // show seven, and what one comes here to do is compare.
            row: unit * 1.45,
            head: unit * 0.95,
        }
    }
}

pub(super) struct WorktreePicker {
    core: PickerCore<Self>,
    /// The repositories one has closed.
    ///
    /// What is **folded** and not what is open, the polarity of the review tree:
    /// a picker one opens to change project shows its projects, so the exception
    /// to remember is the one that has been shut. It does not outlive the
    /// window — a fold here is a reading posture, not a preference.
    folded: HashSet<PathBuf>,
}

impl WorktreePicker {
    pub(super) fn new(window: &mut Window, cx: &mut Context<ClaudhubApp>) -> Entity<Self> {
        let owner = cx.entity();
        let query = cx.new(|cx| InputState::new(window, cx).placeholder(tr!("worktree-filter")));
        cx.new(|cx| Self {
            core: PickerCore::new(&owner, query, cx),
            folded: HashSet::new(),
        })
    }

    fn select(&mut self, path: PathBuf, window: &mut Window, cx: &mut Context<Self>) {
        self.act(window, cx, move |app, window, cx| {
            app.select_worktree(path, window, cx)
        });
    }

    fn render_list(&mut self, window: &Window, cx: &mut Context<Self>) -> gpui_kit::AnyElement {
        let look = Look::of(cx);
        let rows = self.rows(cx);
        let cursor = self.core.cursor;
        let active = self
            .core
            .app
            .upgrade()
            .and_then(|app| app.read(cx).active_path());
        let sizes = rows
            .iter()
            .map(|row| match row {
                Row::Repo { .. } => gpui_kit::size(px(0.), look.head),
                _ => gpui_kit::size(px(0.), look.row),
            })
            .collect();
        let entity = cx.entity();
        let build = move |ix: usize, cx: &mut App| match &rows[ix] {
            Row::Repo {
                main,
                name,
                folded,
                count,
            } => repo_heading(&entity, ix, main, name, *count, *folded, look),
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
        };
        let footer = h_flex()
            .w_full()
            .px_1()
            .py_0p5()
            .items_center()
            .border_t_1()
            .border_color(look.border)
            .child(
                Button::new("repo-open")
                    .ghost()
                    .small()
                    .icon(icon("folder-plus"))
                    .label(tr!("repo-open"))
                    .on_click(cx.listener(|this, _, window, cx| {
                        this.act(window, cx, |app, window, cx| {
                            app.prompt_open_repository(window, cx)
                        });
                    })),
            )
            .into_any_element();
        crate::ui::picker::render_list(
            self,
            crate::ui::picker::ListFrame {
                id: "worktree",
                sizes,
                empty: tr!("worktree-none"),
                muted: look.muted,
                docked: false,
                footer,
            },
            build,
            window,
            cx,
        )
    }

    fn render_actions(
        &mut self,
        main: &Path,
        worktree: &Path,
        label: &str,
        cx: &mut Context<Self>,
    ) -> gpui_kit::AnyElement {
        let look = Look::of(cx);
        let Some(app) = self.core.app.upgrade() else {
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
                    .id(gpui_kit::ElementId::Name(action.id.clone()))
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
                            .small()
                            .icon(icon("arrow-left"))
                            .tooltip(tr!("branch-back"))
                            .on_click(cx.listener(|this, _, _window, cx| {
                                this.core.step = Step::List;
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

impl Pick for WorktreePicker {
    type Row = Row;
    type Actions = Opened;
    /// The worker's answers — worktrees, summaries, `wt` states, agents all
    /// arrive as events — then the pins, and the repositories opened, closed
    /// or forgotten by a gesture here.
    type Sig = (u64, Vec<PathBuf>, Vec<PathBuf>, Vec<PathBuf>);

    fn core(&self) -> &PickerCore<Self> {
        &self.core
    }

    fn core_mut(&mut self) -> &mut PickerCore<Self> {
        &mut self.core
    }

    fn signature(app: &ClaudhubApp, cx: &App) -> Self::Sig {
        (
            app.events_seen,
            crate::ui::store::Store::global(cx).pinned.clone(),
            app.repos.iter().map(|repo| repo.main.clone()).collect(),
            app.repos
                .missing()
                .iter()
                .map(|repo| repo.path.clone())
                .collect(),
        )
    }

    /// The rows on screen, headings included — **kept between frames** (see
    /// [`Pick::rows`]): it built every checkout's row, its summary, its agent
    /// and its `wt` state read one by one, *before* the filter had a say.
    /// What is listed is `worktrees::rows_for`, which is free of gpui and
    /// tested; what is here is where the data comes from.
    fn build_rows(&self, cx: &App) -> Vec<Row> {
        let Some(app) = self.core.app.upgrade() else {
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
        let query = self.core.query.read(cx).value();
        worktrees::rows_for(&repos, &gone, &query, &self.folded, |repo, checkout| Item {
            main: repo.main.clone(),
            path: checkout.path.clone(),
            label: checkout.label.clone(),
            branch: checkout.branch.clone(),
            is_main: checkout.is_main,
            summary: app.summaries.get(&checkout.path).copied(),
            up: app.wt_state(&checkout.path).and_then(|state| state.up),
            detail: app.wt_state(&checkout.path).and_then(worktrees::detail),
            pinned: pinned.contains(&checkout.path),
            agent: app.agents.get(&checkout.path).cloned(),
        })
    }

    /// The headings and the dead repositories are stepped over: neither is
    /// somewhere Enter could take one.
    fn landable(row: &Row) -> bool {
        matches!(row, Row::Worktree(_))
    }

    fn enter(&mut self, row: Row, window: &mut Window, cx: &mut Context<Self>) {
        if let Row::Worktree(item) = row {
            self.select(item.path, window, cx);
        }
    }
}

impl Render for WorktreePicker {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let body = match &self.core.step {
            Step::List => self.render_list(window, cx),
            Step::Actions(Opened {
                main,
                worktree,
                label,
            }) => {
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
    count: usize,
    folded: bool,
    look: Look,
) -> gpui_kit::AnyElement {
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
                this.core.stale = true;
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
                .child(SharedString::from(name.to_string())),
        )
        // How many checkouts are under it, beside the name and not at the far
        // right: it qualifies the word, so it belongs against it. On a folded
        // repository it is all that is left of them.
        .child(
            div()
                .flex_none()
                .text_xs()
                .text_color(look.muted)
                .child(count.to_string()),
        )
        .child(div().flex_1())
        .child(
            Button::new(("new-worktree", index))
                .ghost()
                .small()
                .icon(icon("plus"))
                // **The word, and not the glyph alone.** A `+` on a repository's
                // line is read as "add a repository" as readily as "add a
                // worktree", and the one gesture this window exists for should
                // not need a hover to say which it is. The gloss stays: it is
                // what says *which* repository is being added to.
                .label(tr!("worktree-new"))
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
) -> gpui_kit::AnyElement {
    let (for_click, for_menu, for_pin) = (picker.clone(), picker.clone(), picker.clone());
    let target = item.path.clone();
    let pin_target = item.path.clone();
    let opened = item.clone();
    // The `…` comes out on the row under the pointer and on the row one is
    // standing in, and stays out of the way everywhere else — the branch
    // picker's rule, and the two are one surface twice over. The pin follows
    // it, with one exception written where it is applied: a pin that is *set*
    // stays lit, being a state one reads down the list and not an action one
    // aims at.
    let group = SharedString::from(format!("worktree-row-{index}"));
    let armed = at_cursor || selected;

    h_flex()
        .id(("worktree-row", index))
        .group(group.clone())
        .h(look.row)
        .w_full()
        // **What `wt` knows is a tooltip and no longer a third line.** The
        // options chosen, the ports, the project's `[status.info]`: read when
        // two checkouts of one branch have to be told apart, which is not what
        // one does on every glance — and a list where every row carried it was
        // three lines deep per checkout, so five of them filled the popover.
        // A picker answers *which one*; the rest is one hover away.
        .when_some(item.detail.clone(), |el, detail| {
            el.tooltip({
                let full = SharedString::from(detail.full);
                move |window, cx| {
                    gpui_kit::component::tooltip::Tooltip::new(full.clone()).build(window, cx)
                }
            })
        })
        // The rule down the left edge of the checkout one is in: the mark every
        // editor puts on the file it has open, and what makes that row findable
        // without reading a word of it. **Every row carries the border, and all
        // but one carry it in nothing** — on a single row it would move that
        // row's text two pixels right of its neighbours', which reads as a
        // misalignment rather than as a mark.
        .border_l_2()
        .border_color(match selected {
            true => look.primary,
            false => gpui_kit::transparent_black(),
        })
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
                    h_flex()
                        .w_full()
                        .min_w_0()
                        .gap_1()
                        .items_center()
                        // The name takes what it needs and no more, so what
                        // qualifies it sits at its shoulder rather than at the
                        // far edge — the branch picker's line, written again.
                        .child(
                            div()
                                .min_w_0()
                                .truncate()
                                .text_sm()
                                .when(selected, |el| el.font_semibold())
                                .child(SharedString::from(item.label.clone())),
                        )
                        // The same word as the branch picker's, and it is the
                        // same question: bold alone says "this one" only to
                        // somebody comparing two rows.
                        .when(selected, |el| {
                            el.child(crate::ui::theme::chip(tr!("branch-here"), look.primary))
                        })
                        .child(div().flex_1()),
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
        // Who is working in it — and, when the agent said so itself, that it
        // has finished or is waiting for an answer: the reason one opens this
        // list with five agents running.
        .when_some(item.agent.as_ref(), |el, agent| {
            el.child(crate::ui::topbar::activity_badge(agent, px(120.), cx))
        })
        .when_some(
            item.summary.filter(|summary| !summary.is_empty()),
            |el, summary| el.child(crate::ui::topbar::volume(summary, cx)),
        )
        // The pin, before the `…`. **Its slot is on every row, its glyph only
        // where it says something**: lit on the checkouts one has pinned, and
        // on the row under the pointer, which is where one aims to pin. A grey
        // pin on all forty rows was a column of them read before the names —
        // and hiding the slot as well would move the volume beside it every
        // time the pointer crossed a row. It is the same toggle as the entry in
        // the actions, brought out where the eye already is: pinning is decided
        // while scanning the list, not after opening a menu about one row.
        .child(
            div()
                .flex_none()
                .when(!item.pinned && !armed, |el| {
                    el.opacity(0.).group_hover(group.clone(), |s| s.opacity(1.))
                })
                .child(
                    Button::new(("worktree-pin", index))
                        .ghost()
                        .small()
                        .icon(
                            icon(if item.pinned { "pin-off" } else { "pin" }).text_color(
                                if item.pinned {
                                    look.primary
                                } else {
                                    look.muted.opacity(0.6)
                                },
                            ),
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
                                if let Some(app) = this.core.app.upgrade() {
                                    app.update(cx, |app, cx| app.toggle_pin(&target, cx));
                                }
                                cx.notify();
                            });
                        }),
                ),
        )
        .child(
            div()
                .flex_none()
                .when(!armed, |el| {
                    el.opacity(0.).group_hover(group, |s| s.opacity(1.))
                })
                .child(
                    Button::new(("worktree-actions", index))
                        .ghost()
                        .small()
                        .icon(icon("ellipsis"))
                        .tooltip(tr!("worktree-actions"))
                        .on_click(move |_, _window, cx| {
                            // The row selects; without this the `…` would change worktree
                            // on its way to the actions.
                            cx.stop_propagation();
                            let opened = opened.clone();
                            for_menu.update(cx, |this, cx| {
                                this.core.step = Step::Actions(Opened {
                                    main: opened.main.clone(),
                                    worktree: opened.path.clone(),
                                    label: opened.label.clone(),
                                });
                                cx.notify();
                            });
                        }),
                ),
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
) -> gpui_kit::AnyElement {
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
                .small()
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
        let focus = picker.read(cx).core.query.read(cx).focus_handle(cx);
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
                picker.update(cx, |this, _| this.core.popover = Some(popover));
                picker.clone()
            })
            .appearance(true)
            .p_0()
    }
}
