//! The history panel and its graph.
//!
//! The graph is drawn, not written in characters: a curve makes a branch's
//! attachment readable at a glance where a `|/` has to be deciphered. Each row
//! paints its own portion — the lines crossing it, those closing on it, those
//! leaving it — which is what lets the list stay virtualised: a row draws
//! without knowing anything about the ones out of sight.

use std::path::{Path, PathBuf};
use std::rc::Rc;

use gpui::{
    canvas, div, prelude::*, px, uniform_list, Bounds, Context, Entity, Hsla, PathBuilder, Pixels,
    Point, SharedString, Size, Window,
};
use gpui_component::{
    button::{Button, ButtonVariants},
    h_flex,
    menu::{ContextMenuExt as _, PopupMenu},
    resizable::{h_resizable, resizable_panel, v_resizable},
    v_flex, ActiveTheme, Selectable, Sizable,
};

use crate::git::{DiffRange, GraphRow, LogRange};
use crate::runtime::Cmd;
use crate::tr;
use crate::ui::app::{ClaudhubApp, History};
use crate::ui::icons::icon;

/// Width of one graph column.
const LANE: Pixels = px(14.);
// A row's height follows from the text size (`theme::row_height`): the list
// reserves the height of a single element and measures nothing, so a row that
// is too tall covers the next one as soon as the font grows.
const DOT: Pixels = px(7.);
const STROKE: Pixels = px(1.5);
/// Number of commits asked for. Beyond that, history is read by search, not by
/// scrolling — and an unbounded `git log` on an old repository costs seconds
/// for rows nobody will reach.
const LIMIT: usize = 2_000;
/// Number of commits asked for in a line history. Far lower, and for a reason
/// of its own: `git log -L` reconstructs the file at every commit it walks, and
/// each answer carries its patch — a range of lines has a handful of authors,
/// not two thousand.
const LINE_LIMIT: usize = 200;

/// How many commits a range is worth asking for.
fn limit_of(range: &LogRange) -> usize {
    match range {
        LogRange::Lines { .. } => LINE_LIMIT,
        _ => LIMIT,
    }
}

/// A column's colour. The hues rotate so two neighbouring branches are not
/// confused; they have no other meaning, git having no notion of branch
/// identity at commit level.
fn lane_color(column: usize, cx: &gpui::App) -> Hsla {
    const HUES: [f32; 6] = [0.58, 0.35, 0.08, 0.78, 0.14, 0.95];
    let theme = cx.theme();
    Hsla {
        h: HUES[column % HUES.len()],
        s: 0.55,
        l: if theme.mode.is_dark() { 0.62 } else { 0.42 },
        a: 1.0,
    }
}

impl ClaudhubApp {
    /// Loads the current worktree's history if it is not loaded already.
    pub(super) fn ensure_history(&mut self, cx: &mut Context<Self>) {
        let Some(worktree) = self.active.clone() else {
            return;
        };
        let Some(state) = self.review.get_mut(&worktree) else {
            return;
        };
        if state.history.is_some() || state.history_pending {
            return;
        }
        state.history_pending = true;
        let range = state.history_range.clone();
        self.git.send(Cmd::LoadHistory {
            worktree,
            limit: limit_of(&range),
            range,
        });
        cx.notify();
    }

    /// Reloads the histories a git write has made stale — a pull, a merge, a
    /// commit… Every worktree of the repository is concerned, not only the one
    /// the write ran in: the graph decorates its commits with the branches and
    /// tags of the whole repository. Only histories already loaded are asked
    /// again, and the reload is silent — the list stays on screen until the
    /// fresh one arrives, unlike a range change, where keeping it would
    /// suggest the button did nothing.
    pub(super) fn refresh_repo_history(&mut self, worktree: &Path, cx: &mut Context<Self>) {
        let Some(main) = self.main_of(worktree) else {
            return;
        };
        let stale: Vec<PathBuf> = self
            .review
            .keys()
            .filter(|w| self.main_of(w).as_deref() == Some(main.as_path()))
            .cloned()
            .collect();
        for worktree in stale {
            let Some(state) = self.review.get_mut(&worktree) else {
                continue;
            };
            // A line history stays as it is: it answers a gesture — its line
            // numbers were mapped onto the HEAD of that moment — and its
            // arrival opens the first commit, which would steal the view.
            if state.history.is_none() || matches!(state.history_range, LogRange::Lines { .. }) {
                continue;
            }
            state.history_pending = true;
            let range = state.history_range.clone();
            self.git.send(Cmd::LoadHistory {
                worktree,
                limit: limit_of(&range),
                range,
            });
        }
        cx.notify();
    }

    pub(super) fn set_history_range(&mut self, range: LogRange, cx: &mut Context<Self>) {
        let Some(worktree) = self.active.clone() else {
            return;
        };
        let Some(state) = self.review.get_mut(&worktree) else {
            return;
        };
        if state.history_range == range {
            return;
        }
        state.history_range = range.clone();
        // The previous history is thrown away at once: keeping it while loading
        // would give the impression that the button did nothing, then that the
        // list changes by itself.
        state.history = None;
        state.history_pending = true;
        self.git.send(Cmd::LoadHistory {
            worktree,
            limit: limit_of(&range),
            range,
        });
        cx.notify();
    }

    /// The history of the lines selected in a file — PhpStorm's "Show History
    /// for Selection".
    ///
    /// The gesture is made on a **buffer**, and git only knows the file it has:
    /// the line numbers are mapped back onto HEAD's before they are asked
    /// about, otherwise an edited file returns the history of other lines
    /// without anything saying so.
    pub(super) fn show_line_history(
        &mut self,
        path: PathBuf,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(worktree) = self.active.clone() else {
            return;
        };
        // **The editor names its files relative to the checkout** — it is what
        // `repo::head_blob` reads as `HEAD:./<path>`, and it is what `-L` wants,
        // taking no pathspec. An absolute path is accepted anyway, and one that
        // is not under the worktree belongs to no history here: a file outside
        // every checkout is edited in the same panel.
        let relative = match path.strip_prefix(&worktree) {
            Ok(relative) => relative.to_path_buf(),
            Err(_) if path.is_absolute() => return,
            Err(_) => path.clone(),
        };
        let Some(editing) = self.editing_at(&path) else {
            return;
        };
        let dirty = editing.dirty;
        let base = editing.base.clone();
        let (rows, text) = {
            let state = editing.input.read(cx);
            let selection = state.selected_range();
            let text = state.value();
            let first = crate::ui::surface::line_at(state.text(), selection.start);
            let mut last = crate::ui::surface::line_at(state.text(), selection.end);
            // A selection made by dragging down whole lines ends at the start of
            // the one below, which is not part of what was pointed at. An empty
            // selection is the caret's line, as it is in every editor offering
            // this gesture: one right-clicks a line to ask about it.
            if last > first && text.as_bytes().get(selection.end.wrapping_sub(1)) == Some(&b'\n') {
                last -= 1;
            }
            (first..last + 1, text)
        };

        let rows = match base.as_deref() {
            Some(base) if dirty => match crate::ui::hunks::to_base(base, &text, rows) {
                Some(rows) => rows,
                None => {
                    self.notify(
                        crate::ui::notify::Notice {
                            title: "history-lines-untracked",
                            body: String::new(),
                            hidden: 0,
                            level: crate::ui::notify::Level::Error,
                        },
                        window,
                        cx,
                    );
                    return;
                }
            },
            _ => rows,
        };
        // git counts from one, both ends included: the exclusive end of a
        // zero-based range is already the last line's number.
        let range = LogRange::Lines {
            path: relative,
            start: rows.start + 1,
            end: rows.end.max(rows.start + 1),
        };

        // The screen is called up before the answer: the list takes a `git log`
        // to arrive, and staying in the file until then reads as the menu
        // having done nothing. **And the step is written down**, the screen
        // having been taken rather than asked for — which is `travel_reveal`,
        // step and all, rather than the two written out here.
        self.travel_to_panel(crate::ui::panels::DiffPanel::NAME, window, cx);
        self.set_history_range(range, cx);
    }

    /// Shows a commit's diff.
    pub(super) fn open_commit(&mut self, index: usize, cx: &mut Context<Self>) {
        // Read before the state is borrowed: the highlighting depends on the
        // theme, and `cx.theme()` borrows `cx`.
        let theme = cx.theme().highlight_theme.clone();
        let Some(worktree) = self.active.clone() else {
            return;
        };
        let Some(state) = self.review.get_mut(&worktree) else {
            return;
        };
        let Some(history) = state.history.clone() else {
            return;
        };
        let Some(commit) = history.commits.get(index) else {
            return;
        };

        state.commit = Some(commit.id.clone());
        state.commit_detail = None;
        // The first parent: it is the comparison a reviewer expects in front of
        // a merge, the one showing what the merge brought in.
        let range = DiffRange::Commit {
            id: commit.id.clone(),
            parent: commit.parents.first().cloned(),
        };
        state.range = range.clone();
        state.selected = None;
        state.diff = None;
        state.diff_selection = None;
        // Another commit's diffs are of no further use: keeping them would
        // swell the state by one range per commit looked at.
        state
            .files
            .retain(|kept, _| !matches!(kept, DiffRange::Commit { .. }));
        state
            .pending_files
            .retain(|kept| !matches!(kept, DiffRange::Commit { .. }));
        // The block above the diff wants the full message, which the history's
        // one-line format does not carry. Asked for in the lines case too: the
        // restricted patch shown there is captioned all the same.
        self.git.send(Cmd::LoadCommitDetail {
            worktree: worktree.clone(),
            id: commit.id.clone(),
        });

        // In a line history, the restricted patch is already here: it came with
        // the list, from the same `git log -L`. Showing it costs no command —
        // and the whole commit stays one click away, on the toggle.
        let lines = match &state.history_range {
            LogRange::Lines { path, .. } if self.history_lines_only => Some(path.clone()),
            _ => None,
        };
        if let (Some(path), Some(patch)) = (lines, history.patches.get(index)) {
            state.selected = Some(path.clone());
            state.diff = Some(std::rc::Rc::new(crate::ui::diff_view::Rendered::new(
                &path,
                patch.clone(),
                &theme,
            )));
            cx.notify();
            return;
        }
        self.ensure_files(range, cx);
        cx.notify();
    }

    /// Does a commit match the query? Its subject, its author, its short hash:
    /// the three things a commit is found by.
    pub(super) fn commit_matches(commit: &crate::git::Commit, query: &str) -> bool {
        crate::ui::find::matches(query, &commit.summary)
            || crate::ui::find::matches(query, &commit.author)
            || crate::ui::find::matches(query, &commit.short)
    }

    /// Selects the next matching commit, and brings it into view.
    ///
    /// The graph forbids filtering: its lines join a row to its neighbours, and
    /// removing a row from the middle would make every one of them point at the
    /// wrong commit. The search therefore dims what does not match, and this key
    /// goes from one find to the next.
    pub(super) fn step_history_match(&mut self, delta: isize, cx: &mut Context<Self>) {
        let query = self.query(crate::ui::find::Pane::History, cx);
        if query.trim().is_empty() {
            return;
        }
        let Some(state) = self.active_review() else {
            return;
        };
        let Some(history) = state.history.clone() else {
            return;
        };
        let selected = state.commit.clone();
        let count = history.commits.len();
        if count == 0 {
            return;
        }
        let from = selected
            .and_then(|id| history.commits.iter().position(|c| c.id == id))
            .map(|index| index as isize)
            .unwrap_or(if delta > 0 { -1 } else { count as isize });
        // The search wraps: the point is to go round what was found, not to hit
        // the end of the list.
        for step in 1..=count as isize {
            let index = (from + delta * step).rem_euclid(count as isize) as usize;
            if Self::commit_matches(&history.commits[index], &query) {
                self.open_commit(index, cx);
                self.history_scroll
                    .scroll_to_item(index, gpui::ScrollStrategy::Center);
                return;
            }
        }
    }

    pub(super) fn render_history(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let history_scroll = self.history_scroll.clone();
        let find = self.render_find(crate::ui::find::Pane::History, cx);
        let query = self.query(crate::ui::find::Pane::History, cx);
        let Some(state) = self.active_review() else {
            return div().into_any_element();
        };
        let range = state.history_range.clone();
        let selected = state.commit.clone();
        let history = state.history.clone();

        let row_height = crate::ui::theme::row_height(cx);
        // What the scope says, and what leaves it. A line history is a filter
        // on the list, and a list that is short without saying why reads as a
        // repository with no history.
        let scope = match &range {
            LogRange::Lines { path, start, end } => Some(SharedString::from(format!(
                "{}:{start}\u{2013}{end}",
                path.display()
            ))),
            // The branch the log was pointed at, from the column beside it. It
            // is said here too, and not only by the highlighted row: the column
            // goes away when the panel narrows, and a list that shows one
            // branch without saying which is a list one mistrusts.
            LogRange::Ref { name } => Some(SharedString::from(name.clone())),
            _ => None,
        };
        let lines_scope = matches!(range, LogRange::Lines { .. });
        let lines_only = self.history_lines_only;
        let show_graph = crate::ui::settings::Settings::global(cx).history_graph;
        // The arrangement, decided once: the header reads it to know whether
        // the branches have a column to be shown in, and the body to build it.
        let roomy = Shape::of(self.history_shape) == Shape::Full;
        let want_branches = crate::ui::settings::Settings::global(cx).history_branches;
        let shape = if want_branches {
            Shape::of(self.history_shape)
        } else {
            Shape::of(self.history_shape).without_branches()
        };
        let header = h_flex()
            .h(crate::ui::theme::bar_height(cx))
            .w_full()
            .px_1()
            .gap_1()
            .items_center()
            .border_b_1()
            .border_color(cx.theme().border)
            // **Only where the column of branches is not on screen.** The two
            // are the same two rows that column puts above the branches, and
            // saying it twice would be two controls for one answer, one of
            // which does not show the branch one is actually reading.
            .when(shape != Shape::Full, |header| {
                header.children(
                    [
                        // The current branch first: it is the default, and the
                        // default reads left to right.
                        (LogRange::Head, tr!("history-head")),
                        (LogRange::All, tr!("history-all")),
                    ]
                    .into_iter()
                    .enumerate()
                    .map(|(ix, (target, label))| {
                        let selected = range == target;
                        Button::new(("history-range", ix))
                            .ghost()
                            .small()
                            .label(label)
                            .selected(selected)
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.set_history_range(target.clone(), cx);
                            }))
                    }),
                )
            })
            .when_some(scope, |header, scope| {
                header
                    .child(
                        div()
                            .px_1p5()
                            .rounded(px(8.))
                            .bg(cx.theme().secondary)
                            .text_xs()
                            .text_color(cx.theme().secondary_foreground)
                            // The path is the long part, and the bar is narrow:
                            // it is cut at the start, the file's name being what
                            // says which file this is.
                            .truncate()
                            .child(scope),
                    )
                    .child(
                        Button::new("history-lines-close")
                            .ghost()
                            .small()
                            .icon(crate::ui::icons::icon("x"))
                            .tooltip(tr!("history-lines-close"))
                            // Back to where each came from: a restricted patch
                            // widens to every reference, a branch narrows back
                            // to the checkout one is on.
                            .on_click(cx.listener(move |this, _, _, cx| {
                                let back = if lines_scope {
                                    LogRange::All
                                } else {
                                    LogRange::Head
                                };
                                this.set_history_range(back, cx);
                            })),
                    )
                    // The toggle: the patch restricted to those lines, or the
                    // commit whole. Both are one click away because both are
                    // read — the first says what happened to this code, the
                    // second says what it was part of. A branch has no such
                    // question: it is whole commits either way.
                    .when(lines_scope, |header| {
                        header.child(
                            Button::new("history-lines-scope")
                                .ghost()
                                .small()
                                .label(if lines_only {
                                    tr!("history-lines-only")
                                } else {
                                    tr!("history-lines-whole")
                                })
                                .tooltip(tr!("history-lines-toggle"))
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.history_lines_only = !this.history_lines_only;
                                    // The commit on screen was opened under the old
                                    // answer: re-opening it is what makes the toggle
                                    // show its effect rather than announce it.
                                    let index = this.active_review().and_then(|state| {
                                        let history = state.history.as_ref()?;
                                        let id = state.commit.as_deref()?;
                                        history.commits.iter().position(|c| c.id == id)
                                    });
                                    if let Some(index) = index {
                                        this.open_commit(index, cx);
                                    }
                                    cx.notify();
                                })),
                        )
                    })
            })
            .child(div().flex_1())
            // At the far end because it changes how the list is drawn, not
            // what it shows — the same seat as the review's tree toggle.
            // **No button where there is no room for the column**, the rule
            // the rails follow for a situational view: a target that never
            // answers is one the eye learns to skip. In a side column the
            // branches are the top bar's picker, which is where they were.
            .when(roomy, |header| {
                header.child(
                    Button::new("history-branches")
                        .ghost()
                        .small()
                        .icon(crate::ui::icons::icon("git-branch"))
                        .selected(want_branches)
                        .tooltip(if want_branches {
                            tr!("history-branches-hide")
                        } else {
                            tr!("history-branches-show")
                        })
                        .on_click(cx.listener(|_this, _, _, cx| {
                            crate::ui::settings::Settings::update_global(cx, |s| {
                                s.history_branches = !s.history_branches;
                            });
                            cx.notify();
                        })),
                )
            })
            .child(
                Button::new("history-graph")
                    .ghost()
                    .small()
                    .icon(crate::ui::icons::icon("git-merge"))
                    .selected(show_graph)
                    .tooltip(if show_graph {
                        tr!("history-graph-hide")
                    } else {
                        tr!("history-graph-show")
                    })
                    .on_click(cx.listener(|_this, _, _, cx| {
                        crate::ui::settings::Settings::update_global(cx, |s| {
                            s.history_graph = !s.history_graph;
                        });
                        cx.notify();
                    })),
            );

        let Some(history) = history else {
            return v_flex()
                .size_full()
                .child(header)
                .children(find)
                .child(
                    div()
                        .p_3()
                        .text_xs()
                        .text_color(cx.theme().muted_foreground)
                        .child(tr!("history-loading")),
                )
                .into_any_element();
        };
        if history.commits.is_empty() {
            return v_flex()
                .size_full()
                .child(header)
                .children(find)
                .child(
                    div()
                        .p_3()
                        .text_xs()
                        .text_color(cx.theme().muted_foreground)
                        .child(tr!("history-empty")),
                )
                .into_any_element();
        }

        let entity = cx.entity();
        // Put aside before the list's closure takes the handle: the measuring
        // canvas is built after it, and there is nothing left to clone by then.
        let measured = entity.clone();
        let count = history.commits.len();
        // Without the graph the rows keep a sliver of left padding, not a
        // gutter: the lanes are what took the width, and the width is the
        // summary's.
        let gutter = if show_graph {
            LANE * history.width as f32 + px(6.)
        } else {
            px(6.)
        };

        // Which of the row's fixed columns fit beside the summary. The summary
        // is what a history is read by — PhpStorm sheds its columns the same
        // way — so it is the metadata that gives way: without this, the hash,
        // author and date columns are incompressible and a narrow panel
        // truncated the summary to nothing. The measured width is the previous
        // frame's (see the canvas below), `px(0.)` on the very first: starting
        // bare and gaining columns a frame later beats starting full and
        // losing them.
        //
        // What is measured is the room **beside the graph**: the gutter is
        // sized by the widest point of the whole graph, and on an all-branches
        // history it eats a hundred pixels the thresholds used to count as if
        // the summary could use them.
        let width = (self.history_laid_out - gutter).max(px(0.));
        let columns = Columns {
            graph: show_graph,
            hash: width >= px(560.),
            author: width >= px(440.),
            date: width >= px(300.),
            // The chips give way with the author: a `origin/master` chip on a
            // narrow panel was squeezing the one column a history is read by.
            chip_cap: if width >= px(440.) { px(140.) } else { px(90.) },
        };

        // The file list under the graph says what else a commit touched. In a
        // line history read line by line there is nothing else: the patch on
        // screen is the answer, and the list would be a second one for a
        // question nobody asked.
        let commit_range = self
            .active_review()
            .and_then(|state| state.commit.as_ref().map(|_| state.range.clone()))
            .filter(|range| matches!(range, DiffRange::Commit { .. }))
            .filter(|_| !(lines_only && matches!(range, LogRange::Lines { .. })));

        let graph = v_flex().size_full().child(header).children(find).child(
            div()
                .relative()
                .flex_1()
                .min_h_0()
                // The width the columns were chosen for is always the previous
                // frame's — the `diff_laid_out` mechanism: measure after
                // layout, and ask for one more frame when it has moved, which
                // settles as soon as the resize stops.
                .child(
                    canvas(
                        {
                            let entity = entity.clone();
                            move |bounds: Bounds<Pixels>, window, cx| {
                                entity.update(cx, |this, _| {
                                    if (bounds.size.width - this.history_laid_out).abs() > px(0.5) {
                                        this.history_laid_out = bounds.size.width;
                                        window.request_animation_frame();
                                    }
                                });
                            }
                        },
                        |_, _, _, _| {},
                    )
                    .absolute()
                    .size_full(),
                )
                .child(
                    self.scrolled(
                        "history-bar",
                        &history_scroll,
                        crate::ui::motion::Axes::Vertical,
                        window,
                        uniform_list("history", count, move |visible, _window, cx| {
                            visible
                                .map(|ix| {
                                    // Filtering is impossible here: the graph's
                                    // lines join a row to its neighbours, and
                                    // removing one from the middle would make every
                                    // one of them point at the wrong commit. What
                                    // does not match is dimmed, and stays in place.
                                    let dimmed = !query.is_empty()
                                        && !history.commits.get(ix).is_some_and(|c| {
                                            ClaudhubApp::commit_matches(c, &query)
                                        });
                                    render_commit(
                                        &history,
                                        ix,
                                        gutter,
                                        selected.as_deref(),
                                        row_height,
                                        dimmed,
                                        columns,
                                        &entity,
                                        cx,
                                    )
                                })
                                .collect::<Vec<_>>()
                        })
                        .size_full()
                        .track_scroll(&self.history_scroll.clone()),
                        cx,
                    ),
                ),
        );

        // The panel's own shape, measured the way the graph's width is and for
        // a question of its own: which way the split below runs. The width
        // above is the room left beside the gutter, and says nothing about the
        // room a second column would have.
        let measure = canvas(
            {
                move |bounds: Bounds<Pixels>, window, cx| {
                    measured.update(cx, |this, _| {
                        let shape = this.history_shape;
                        if (bounds.size.width - shape.width).abs() > px(0.5)
                            || (bounds.size.height - shape.height).abs() > px(0.5)
                        {
                            this.history_shape = bounds.size;
                            window.request_animation_frame();
                        }
                    });
                }
            },
            |_, _, _, _| {},
        )
        .absolute()
        .size_full();

        // The graph alone does not say what a commit touched: the list of its
        // files goes with it, otherwise selecting a commit opens only its
        // first file and the others stay invisible. Beside it where the panel
        // is wide, under it where it is a column.
        let files =
            commit_range.map(|range| self.render_file_list(range, window, cx).into_any_element());
        let body = match (files, shape) {
            // No commit chosen: the log has its column to itself.
            (None, _) => graph.into_any_element(),
            (Some(files), Shape::Column) => v_resizable("claudhub-history-split")
                .with_state(&self.history_split)
                .child(resizable_panel().size(px(420.)).child(graph))
                .child(resizable_panel().child(files))
                .into_any_element(),
            // **The files are the pane given a size**, never the graph: what
            // one widens the zone for is the summary column, so the surplus
            // has to fall to the slot no size fixes.
            (Some(files), _) => h_resizable("claudhub-history-split-side")
                .with_state(&self.history_split_side)
                .child(resizable_panel().child(graph))
                .child(resizable_panel().size(px(440.)).child(files))
                .into_any_element(),
        };
        // And the branches, where there is a column for them. **Nested rather
        // than a third slot**: dragging this divider would otherwise move the
        // one between the graph and the files with it.
        let body = match shape {
            Shape::Full => h_resizable("claudhub-history-branches")
                .with_state(&self.history_split_branches)
                .child(
                    resizable_panel()
                        .size(px(260.))
                        .child(self.render_history_branches(cx)),
                )
                .child(resizable_panel().child(body))
                .into_any_element(),
            _ => body,
        };
        div()
            .relative()
            .size_full()
            .child(measure)
            .child(body)
            .into_any_element()
    }

    /// The branches, as the history's leftmost column.
    ///
    /// The same surface as the top bar's picker, and the one the tool window
    /// held before the two were merged — see `branch_picker::Mode`. It has a
    /// filter field of its own, so the click is filed under its own pane:
    /// `Ctrl+F` standing here must aim at that field and not at the log's bar.
    /// **Captured after** the panel's own, which runs first — capture goes
    /// outside in — and files everything under the history.
    fn render_history_branches(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let app = cx.entity();
        div()
            .size_full()
            .border_r_1()
            .border_color(cx.theme().border)
            .child(self.branches_dock.clone())
            .capture_any_mouse_down(move |_, _window, cx| {
                app.update(cx, |app, cx| {
                    app.touch_pane(crate::ui::find::Pane::Branches, cx)
                });
            })
    }
}

/// How the panel lays out what it holds — from its own shape, and nothing else.
///
/// The same view has to read in a tall narrow column and in a wide short zone,
/// which is the whole point of a panel one can drag anywhere; an arrangement
/// fixed one way is a panel that belongs on one edge only. It is the rule the
/// row's own columns already follow, one level up: what there is no room for
/// is not drawn.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Shape {
    /// A tall narrow column. The graph, and the chosen commit's files under it.
    Column,
    /// Wide and short. The files move beside the graph.
    Wide,
    /// Room for a third column: the branches join them, on the left. It is the
    /// arrangement of PhpStorm's Git window, and what a bottom zone gives.
    Full,
}

impl Shape {
    /// **Wider than tall** is what tells a bottom zone from a side one; the two
    /// floors are what keeps every column able to carry a path. Below them a
    /// panel four hundred pixels across is wider than it is tall as soon as it
    /// is short, and splitting it would leave columns too narrow to read.
    ///
    /// The first floor is twice the right zone's own width — the narrowest
    /// column the window already asks a file list to live in. The second adds
    /// the branches' own column to it.
    fn of(size: Size<Pixels>) -> Self {
        if size.width <= size.height {
            return Self::Column;
        }
        if size.width >= px(900.) {
            Self::Full
        } else if size.width >= px(640.) {
            Self::Wide
        } else {
            Self::Column
        }
    }

    /// The same shape with the branches column given up — what the header's
    /// toggle asks for. It cannot turn a column into anything.
    fn without_branches(self) -> Self {
        match self {
            Self::Full => Self::Wide,
            other => other,
        }
    }
}

/// A row's texts, built once when the history arrives.
///
/// `SharedString` is an `Arc`: the list's closure hands these out for every
/// visible row on every frame, and cloning the `String`s of the commit copied
/// four of them per row.
pub struct CommitText {
    pub short: SharedString,
    pub summary: SharedString,
    pub author: SharedString,
    pub date: SharedString,
    /// The two the row shows, and no more: the rest would never be painted.
    pub refs: Vec<SharedString>,
}

pub fn commit_texts(commits: &[crate::git::Commit]) -> Vec<CommitText> {
    commits
        .iter()
        .map(|commit| CommitText {
            short: commit.short.clone().into(),
            summary: commit.summary.clone().into(),
            author: commit.author.clone().into(),
            date: commit.date.clone().into(),
            refs: commit
                .refs
                .iter()
                .take(2)
                .map(|reference| reference.clone().into())
                .collect(),
        })
        .collect()
}

/// Which fixed columns fit beside the summary, and how wide a ref chip may
/// grow — see `render_history`. The summary is what a history is read by, so
/// everything here gives way before it does.
#[derive(Clone, Copy)]
struct Columns {
    graph: bool,
    hash: bool,
    author: bool,
    date: bool,
    chip_cap: Pixels,
}

#[allow(clippy::too_many_arguments)]
fn render_commit(
    history: &Rc<History>,
    index: usize,
    gutter: Pixels,
    selected: Option<&str>,
    row_height: Pixels,
    // The commit does not match the running search.
    dimmed: bool,
    columns: Columns,
    entity: &Entity<ClaudhubApp>,
    cx: &mut gpui::App,
) -> gpui::AnyElement {
    let (Some(commit), Some(row), Some(text)) = (
        history.commits.get(index),
        history.graph.get(index),
        history.texts.get(index),
    ) else {
        return div().into_any_element();
    };
    let is_selected = selected == Some(commit.id.as_str());
    let dot_color = lane_color(row.column, cx);
    let muted = cx.theme().muted_foreground;
    let accent = cx.theme().accent;

    // The history is captured, not the row: an `Rc` clone where copying the
    // row's three vectors of lanes was an allocation per visible row and per
    // frame.
    let for_graph = history.clone();
    let menu_entity = entity.clone();
    let entity = entity.clone();
    // Hidden, the gutter is a sliver of padding and there is nothing to
    // paint in it — the canvas would draw lanes across the summary.
    let graph = columns.graph.then(|| {
        canvas(
            move |_, _, _| {},
            move |bounds, _, window, cx| {
                if let Some(row) = for_graph.graph.get(index) {
                    paint_graph(row, bounds, window, cx);
                }
            },
        )
        .w(gutter)
        .h(row_height)
    });

    h_flex()
        .id(("commit", index))
        .h(row_height)
        .w_full()
        .items_center()
        .gap_2()
        .pr(crate::ui::theme::scroll_gutter())
        // A history row fits on one line: without this, a slightly long sha or
        // author name wraps, exceeds the height the virtualised list reserved
        // and covers the commit's summary.
        .overflow_hidden()
        .whitespace_nowrap()
        // Dimmed rather than hidden: the row keeps its place, so the graph keeps
        // its lines.
        .when(dimmed, |el| el.opacity(0.35))
        .cursor_pointer()
        .when(is_selected, |el| el.bg(accent))
        .hover(|s| s.bg(accent.opacity(0.5)))
        .on_click(move |_, _window, cx| {
            entity.update(cx, |this, cx| this.open_commit(index, cx));
        })
        .children(graph)
        .when(!columns.graph, |el| el.pl(px(6.)))
        .when(columns.hash, |el| {
            el.child(
                div()
                    .flex_none()
                    // Ten fixed-pitch characters: the length git gives `%h` on a
                    // repository this size, plus a margin.
                    .w(px(84.))
                    .font_family(cx.theme().mono_font_family.clone())
                    .text_xs()
                    .text_color(dot_color)
                    .child(text.short.clone()),
            )
        })
        .child(
            div()
                .flex_1()
                .min_w_0()
                .truncate()
                .text_sm()
                .child(text.summary.clone()),
        )
        .children(text.refs.iter().map(|reference| {
            div()
                .flex_none()
                // Capped for the summary's sake: a `origin/feature/…` chip
                // left whole would squeeze the one column a history is read by.
                .max_w(columns.chip_cap)
                .truncate()
                .px_1()
                .rounded(cx.theme().radius)
                .bg(dot_color.opacity(0.18))
                .text_xs()
                .text_color(dot_color)
                .child(reference.clone())
        }))
        .when(columns.author, |el| {
            el.child(
                div()
                    .flex_none()
                    .max_w(px(110.))
                    .truncate()
                    .text_xs()
                    .text_color(muted)
                    .child(text.author.clone()),
            )
        })
        .when(columns.date, |el| {
            el.child(
                div()
                    .flex_none()
                    .w(px(88.))
                    .text_right()
                    .text_xs()
                    .text_color(muted)
                    .child(text.date.clone()),
            )
        })
        // **After the click and not before**: a context menu is not an
        // interactive element, so there is nothing left to hang a click on once
        // it wraps the row. The terminals' tabs learned this first.
        .context_menu(commit_menu(commit, index, &menu_entity))
        .into_any_element()
}

/// What a right click offers on a commit.
///
/// A history is read to find one commit and then to *say* which — in a message,
/// an issue, a `git` one types beside. Copying its reference is therefore the
/// gesture, and everything else here is one the row already knew how to do and
/// had no way of being asked for.
fn commit_menu(
    commit: &crate::git::Commit,
    index: usize,
    entity: &Entity<ClaudhubApp>,
) -> impl Fn(PopupMenu, &mut Window, &mut Context<PopupMenu>) -> PopupMenu + 'static {
    use gpui_component::menu::PopupMenuItem;

    let entity = entity.clone();
    // The three strings, taken now: the closure outlives the row, and the
    // history it came from is replaced whole on every reload.
    let (id, short, summary) = (
        commit.id.clone(),
        commit.short.clone(),
        commit.summary.clone(),
    );
    move |menu, _window, _cx| {
        let copy = |text: String| {
            move |_: &gpui::ClickEvent, _window: &mut Window, cx: &mut gpui::App| {
                cx.write_to_clipboard(gpui::ClipboardItem::new_string(text.clone()));
            }
        };
        let (for_base, for_tag) = (entity.clone(), entity.clone());
        let at = id.clone();
        menu.item(
            PopupMenuItem::new(tr!("commit-copy-ref"))
                .icon(icon("copy"))
                .on_click(copy(id.clone())),
        )
        .item(PopupMenuItem::new(tr!("commit-copy-short")).on_click(copy(short.clone())))
        .item(PopupMenuItem::new(tr!("commit-copy-summary")).on_click(copy(summary.clone())))
        .separator()
        // What the review compares against. A commit read in the history is
        // very often the answer to "since when", which is the one question the
        // base picker exists for.
        .item(
            PopupMenuItem::new(tr!("commit-set-base"))
                .icon(icon("file-diff"))
                .on_click({
                    let (entity, base) = (for_base.clone(), at.clone());
                    move |_, _window, cx| {
                        let base = base.clone();
                        entity.update(cx, |this, cx| this.set_base(base, cx));
                    }
                }),
        )
        // The tag dialog marks the commit the history has selected, so the
        // selection is the whole of what this has to arrange.
        .item(
            PopupMenuItem::new(tr!("commit-tag-here"))
                .icon(icon("tags"))
                .on_click({
                    let entity = for_tag.clone();
                    move |_, window, cx| {
                        entity.update(cx, |this, cx| {
                            this.open_commit(index, cx);
                            this.prompt_new_tag(window, cx);
                        });
                    }
                }),
        )
    }
}

/// Paints a row's portion of the graph.
///
/// The lines are drawn before the bullet so it covers them: a curve arriving on
/// a commit must disappear under it, not cross it.
fn paint_graph(row: &GraphRow, bounds: Bounds<Pixels>, window: &mut Window, cx: &mut gpui::App) {
    let x = |column: usize| bounds.origin.x + LANE * column as f32 + LANE / 2.;
    let top = bounds.origin.y;
    let middle = top + bounds.size.height / 2.;
    let bottom = top + bounds.size.height;

    let mut line = |from: Point<Pixels>, to: Point<Pixels>, ctrl: Option<Point<Pixels>>, color| {
        let mut path = PathBuilder::stroke(STROKE);
        path.move_to(from);
        match ctrl {
            // A curve whose control point is directly above the start: the line
            // leaves its column vertically then bends, which gives the soft
            // join of history viewers rather than a sharp angle.
            Some(ctrl) => path.curve_to(to, ctrl),
            None => path.line_to(to),
        }
        if let Ok(path) = path.build() {
            window.paint_path(path, color);
        }
    };

    // The branches that only pass through.
    for &column in &row.through {
        let color = lane_color(column, cx);
        line(
            gpui::point(x(column), top),
            gpui::point(x(column), bottom),
            None,
            color,
        );
    }

    // The commit's own vertical line: from the top if it has a child above,
    // towards the bottom if it has a parent below. Both are drawn in every
    // case: the first and the last row simply get half a segment, which is
    // exactly what we want to see at the ends.
    let own = lane_color(row.column, cx);
    line(
        gpui::point(x(row.column), top),
        gpui::point(x(row.column), bottom),
        None,
        own,
    );

    // The lanes that close on this commit: they come down from their column and
    // join the bullet.
    for &column in &row.incoming {
        let color = lane_color(column, cx);
        line(
            gpui::point(x(column), top),
            gpui::point(x(row.column), middle),
            Some(gpui::point(x(column), middle)),
            color,
        );
    }

    // The parents placed elsewhere: the line goes from the bullet to their column.
    for &column in &row.outgoing {
        let color = lane_color(column, cx);
        line(
            gpui::point(x(row.column), middle),
            gpui::point(x(column), bottom),
            Some(gpui::point(x(column), middle)),
            color,
        );
    }

    // The bullet, last so it covers the lines reaching it.
    let radius = DOT / 2.;
    window.paint_quad(gpui::fill(
        Bounds::new(
            gpui::point(x(row.column) - radius, middle - radius),
            gpui::size(DOT, DOT),
        ),
        own,
    ));
}

#[cfg(test)]
mod tests {
    use super::*;

    fn shape(width: f32, height: f32) -> Shape {
        Shape::of(gpui::size(px(width), px(height)))
    }

    /// The side zones are tall and narrow: everything stacks, which is the only
    /// way what is left keeps a readable width.
    #[test]
    fn a_side_zone_stacks_them() {
        assert_eq!(shape(320., 760.), Shape::Column);
        assert_eq!(shape(460., 900.), Shape::Column);
    }

    /// Wide and short, but with room for two columns only: the commit's files
    /// move beside the graph, the branches stay in the top bar's picker.
    #[test]
    fn a_narrow_bottom_zone_lays_out_two_columns() {
        assert_eq!(shape(700., 300.), Shape::Wide);
        assert_eq!(shape(899., 240.), Shape::Wide);
    }

    /// A real bottom zone: branches, graph and files, which is the arrangement
    /// this exists for.
    #[test]
    fn a_wide_bottom_zone_lays_out_three() {
        assert_eq!(shape(1900., 240.), Shape::Full);
        assert_eq!(shape(900., 400.), Shape::Full);
    }

    /// Wider than tall is not enough on its own: a short **narrow** panel split
    /// in two leaves two columns too narrow to carry a path.
    #[test]
    fn a_short_narrow_panel_still_stacks_them() {
        assert_eq!(shape(400., 200.), Shape::Column);
        assert_eq!(shape(639., 120.), Shape::Column);
    }

    /// Nothing measured yet — the first frame, before the canvas has run. It
    /// starts stacked, which is what the panel was before any of this.
    #[test]
    fn an_unmeasured_panel_stacks_them() {
        assert_eq!(shape(0., 0.), Shape::Column);
    }

    /// Giving up the branches column falls back to the arrangement without
    /// them, and cannot make a column into anything.
    #[test]
    fn giving_up_the_branches_falls_back_one_step() {
        assert_eq!(Shape::Full.without_branches(), Shape::Wide);
        assert_eq!(Shape::Wide.without_branches(), Shape::Wide);
        assert_eq!(Shape::Column.without_branches(), Shape::Column);
    }
}
