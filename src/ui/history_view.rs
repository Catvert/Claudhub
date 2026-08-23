//! The history panel and its graph.
//!
//! The graph is drawn, not written in characters: a curve makes a branch's
//! attachment readable at a glance where a `|/` has to be deciphered. Each row
//! paints its own portion — the lines crossing it, those closing on it, those
//! leaving it — which is what lets the list stay virtualised: a row draws
//! without knowing anything about the ones out of sight.

use std::path::PathBuf;
use std::rc::Rc;

use gpui::{
    canvas, div, prelude::*, px, uniform_list, Bounds, Context, Entity, Hsla, PathBuilder, Pixels,
    Point, SharedString, Window,
};
use gpui_component::{
    button::{Button, ButtonVariants},
    h_flex,
    resizable::{resizable_panel, v_resizable},
    v_flex, ActiveTheme, Selectable, Sizable,
};

use crate::git::{DiffRange, GraphRow, LogRange};
use crate::runtime::Cmd;
use crate::tr;
use crate::ui::app::{ClaudhubApp, History};

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
        // is not under the worktree belongs to no history here: a plugin's
        // script is edited in the same panel and lives outside every checkout.
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
                        Some(crate::ui::notify::Notice {
                            title: "history-lines-untracked",
                            body: String::new(),
                            level: crate::ui::notify::Level::Error,
                        }),
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

        let from = self.here(cx);
        self.record_step(
            from,
            crate::ui::jumps::Place::Screen(crate::ui::workspace::Workspace::Git),
            cx,
        );
        // The screen is called up before the answer: the list takes a `git log`
        // to arrive, and staying in the file until then reads as the menu
        // having done nothing.
        self.enter_workspace(crate::ui::workspace::Workspace::Git, window, cx);
        self.set_panel_visible(crate::ui::panels::HistoryPanel::NAME, true, cx);
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
            _ => None,
        };
        let lines_only = self.history_lines_only;
        let header = h_flex()
            .h(crate::ui::theme::bar_height(cx))
            .w_full()
            .px_1()
            .gap_1()
            .items_center()
            .border_b_1()
            .border_color(cx.theme().border)
            .children(
                [
                    (LogRange::All, tr!("history-all")),
                    (LogRange::Head, tr!("history-head")),
                ]
                .into_iter()
                .enumerate()
                .map(|(ix, (target, label))| {
                    let selected = range == target;
                    Button::new(("history-range", ix))
                        .ghost()
                        .xsmall()
                        .label(label)
                        .selected(selected)
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.set_history_range(target.clone(), cx);
                        }))
                }),
            )
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
                            .xsmall()
                            .icon(crate::ui::icons::icon("x"))
                            .tooltip(tr!("history-lines-close"))
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.set_history_range(LogRange::All, cx);
                            })),
                    )
                    // The toggle: the patch restricted to those lines, or the
                    // commit whole. Both are one click away because both are
                    // read — the first says what happened to this code, the
                    // second says what it was part of.
                    .child(
                        Button::new("history-lines-scope")
                            .ghost()
                            .xsmall()
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
            });

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
        let count = history.commits.len();
        let gutter = LANE * history.width as f32 + px(6.);

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
            div().flex_1().min_h_0().child(
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
                                    && !history
                                        .commits
                                        .get(ix)
                                        .is_some_and(|c| ClaudhubApp::commit_matches(c, &query));
                                render_commit(
                                    &history,
                                    ix,
                                    gutter,
                                    selected.as_deref(),
                                    row_height,
                                    dimmed,
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

        // The graph alone does not say what a commit touched: the list of its
        // files goes underneath, otherwise selecting a commit opens only its
        // first file and the others stay invisible.
        let Some(range) = commit_range else {
            return graph.into_any_element();
        };
        v_resizable("claudhub-history-split")
            .with_state(&self.history_split)
            .child(resizable_panel().size(px(420.)).child(graph))
            .child(
                resizable_panel()
                    .child(self.render_file_list(range, window, cx).into_any_element()),
            )
            .into_any_element()
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

#[allow(clippy::too_many_arguments)]
fn render_commit(
    history: &Rc<History>,
    index: usize,
    gutter: Pixels,
    selected: Option<&str>,
    row_height: Pixels,
    // The commit does not match the running search.
    dimmed: bool,
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
    let entity = entity.clone();
    let graph = canvas(
        move |_, _, _| {},
        move |bounds, _, window, cx| {
            if let Some(row) = for_graph.graph.get(index) {
                paint_graph(row, bounds, window, cx);
            }
        },
    )
    .w(gutter)
    .h(row_height);

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
        .child(graph)
        .child(
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
                .px_1()
                .rounded(cx.theme().radius)
                .bg(dot_color.opacity(0.18))
                .text_xs()
                .text_color(dot_color)
                .child(reference.clone())
        }))
        .child(
            div()
                .flex_none()
                .max_w(px(110.))
                .truncate()
                .text_xs()
                .text_color(muted)
                .child(text.author.clone()),
        )
        .child(
            div()
                .flex_none()
                .w(px(88.))
                .text_right()
                .text_xs()
                .text_color(muted)
                .child(text.date.clone()),
        )
        .into_any_element()
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
