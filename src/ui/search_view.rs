//! The magnifier: searching the whole project, and reading the answer.
//!
//! This is PhpStorm's `Ctrl+Shift+F` and Doom's `SPC s p` — a word, every place
//! in the checkout that carries it, and the file itself beside the list. The
//! per-panel search (`ui::find`) filters a list Claudhub already holds; this
//! one asks git, which is the only thing that knows what the project contains.
//!
//! **A screen of its own** — see `ui::workspace`. Results and preview are read
//! *together*, so they are two panels and not two tabs: the list on the left,
//! the file on the right, exactly as the editing screen puts the tree beside
//! the editor.
//!
//! `ui::search` holds what is worth a test — which rows exist, what a fold
//! hides, where an arrow lands; here there is plumbing and painting.

use std::collections::HashSet;
use std::path::PathBuf;
use std::rc::Rc;

use gpui::{
    div, prelude::*, px, uniform_list, App, Context, Entity, FocusHandle,
    ListHorizontalSizingBehavior, Pixels, SharedString, StyledText, Window,
};
use gpui_component::{
    button::{Button, ButtonVariants as _},
    checkbox::Checkbox,
    h_flex,
    input::{Input, InputState},
    v_flex, ActiveTheme, Disableable as _, Sizable as _,
};

use crate::git::search::{Query, Results};
use crate::runtime::protocol::Cmd;
use crate::tr;
use crate::ui::app::ClaudhubApp;
use crate::ui::highlight::DocumentHighlights;
use crate::ui::icons::icon;
use crate::ui::search::{self, Row};

/// The scrollbar and wheel-smoothing key of each of the two panels.
const RESULTS_SCROLL: &str = "search-results";
const PREVIEW_SCROLL: &str = "search-preview";

/// Where the search stands.
///
/// In the application and not in the panels, like every other state here: the
/// panels carry none, and the screen one is looking at must not change what the
/// search found.
#[derive(Default)]
pub struct SearchState {
    /// The checkout the shown results belong to. Switching worktree therefore
    /// does not silently show another project's hits: the panel says the search
    /// is stale rather than pretending it is not.
    pub worktree: Option<PathBuf>,
    /// The query as it went out — never as it is being typed. It is what the
    /// list highlights its occurrences with, and highlighting a half-typed word
    /// over results that answer the previous one would be wrong twice.
    pub sent: Query,
    pub results: Rc<Results>,
    /// Why the search could not run: a bad regular expression, most often. It
    /// is shown **under the field** and not in the status bar, which the next
    /// message wipes.
    pub error: Option<String>,
    /// A search has gone out and has not come back.
    pub running: bool,
    /// Never goes back, like the SQL console's send id: one types, the earlier
    /// query is stale, and this is what tells the answer of a gesture from the
    /// answer of the gesture that replaced it.
    pub request: u64,
    /// Bumped on every keystroke, and read again when the debounce fires.
    ///
    /// That comparison is the whole of the debounce, and it is what makes it a
    /// **trailing** one: a deferred search that finds the counter has moved
    /// simply gives up, so nothing goes out until the typing stops. The flag
    /// the rest of this file uses for deferred work — the LSP's, the
    /// terminal's — fires at a fixed rate instead, which is right for
    /// something that costs a millisecond and wrong for something that costs a
    /// second.
    pub typed: u64,
    /// Whether the answer being waited for should hand the list the focus.
    ///
    /// True for a search one **asked** for — Enter, the button — and false for
    /// one the typing triggered: taking the caret out of the field one is
    /// typing in is the one thing an interactive search must not do.
    pub focus_on_answer: bool,
    /// The files whose hits are hidden.
    pub folded: HashSet<PathBuf>,
    /// The selected row, as an index into the displayed list.
    ///
    /// An index and not a path: the list is only rebuilt by a fold, and a fold
    /// is made by clicking the file, which puts the cursor on that same file —
    /// so it cannot drift. Fresh results clear it.
    pub selected: Option<usize>,
    /// The file shown on the right.
    pub preview: Option<Preview>,
}

/// The file beside the list.
pub struct Preview {
    pub worktree: PathBuf,
    pub path: PathBuf,
    /// The lines, cut once on arrival: the render closure runs for every
    /// visible row of every frame and must not split a file there.
    pub lines: Rc<Vec<SharedString>>,
    /// Their syntax colouring, computed once for the same reason.
    pub highlights: Rc<DocumentHighlights>,
    /// The line the selected hit is on, one-based. What the view scrolls to and
    /// paints.
    pub line: u32,
    /// Why there is nothing to show — a file over `files::MAX_LINES`, a read
    /// that failed. Under the preview, where the question was asked.
    pub error: Option<String>,
}

impl ClaudhubApp {
    /// Gives the search field the focus, opening its screen on the way.
    ///
    /// The whole of `Ctrl+Shift+F`: a shortcut that landed on the screen without
    /// putting the caret in the field would leave the gesture half done.
    pub(super) fn open_search(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.enter_workspace(crate::ui::workspace::Workspace::Search, window, cx);
        self.set_panel_visible(crate::ui::panels::SearchPanel::NAME, true, cx);
        let handle = gpui::Focusable::focus_handle(&self.search_input, cx);
        handle.focus(window, cx);
        cx.notify();
    }

    /// The query as the two fields describe it.
    fn search_query(&self, cx: &App) -> Query {
        Query {
            text: self.search_input.read(cx).value().to_string(),
            regex: self.search_regex,
            whole_word: self.search_whole_word,
            include: self.search_glob_input.read(cx).value().to_string(),
        }
    }

    /// A keystroke: the search goes out once the typing stops.
    ///
    /// **Interactive, and therefore debounced.** `git grep` over a monorepo
    /// costs a second: firing on every letter would keep the worker busy on
    /// queries nobody will read, and the answers would arrive out of order for
    /// words nobody typed. So the counter is bumped here and read again when
    /// the timer fires — anything the typing has overtaken gives up. The send
    /// id and the single worker (`runtime::is_search`) catch whatever slips
    /// through.
    ///
    /// **Under `MIN_AUTO` characters, nothing goes out by itself.** One letter
    /// matches half the project: the answer is two thousand hits, capped, and
    /// nobody asked for it. `Enter` searches anyway — it is a gesture, and a
    /// gesture is allowed to be expensive.
    pub(super) fn search_typed(&mut self, cx: &mut Context<Self>) {
        self.search.typed = self.search.typed.wrapping_add(1);
        let at = self.search.typed;
        cx.spawn(async move |this, cx| {
            cx.background_executor().timer(DEBOUNCE).await;
            let _ = this.update(cx, |this, cx| {
                if this.search.typed != at {
                    return; // the typing went on: this query is already stale
                }
                let query = this.search_query(cx);
                // An emptied field empties the list: leaving the last answer
                // under an empty field would read as a search that found it.
                if query.is_empty() {
                    this.run_search(false, cx);
                    return;
                }
                if query.text.trim().chars().count() < MIN_AUTO {
                    return;
                }
                // The same query as the one on screen: nothing to ask again.
                // A checkbox that changes nothing, a glob one retyped
                // identically — both come through here.
                if query == this.search.sent && this.search.error.is_none() {
                    return;
                }
                this.run_search(false, cx);
            });
        })
        .detach();
    }

    /// Sends the search.
    ///
    /// `hand` is true for a search one asked for — `Enter`, the button — which
    /// is what decides whether the answer takes the focus.
    pub(super) fn run_search(&mut self, hand: bool, cx: &mut Context<Self>) {
        self.search.focus_on_answer = hand;
        let Some(worktree) = self.active.clone() else {
            return;
        };
        let query = self.search_query(cx);
        if query.is_empty() {
            self.search.results = Rc::new(Results::default());
            self.search.error = None;
            self.search.running = false;
            self.search.selected = None;
            cx.notify();
            return;
        }
        self.search.request += 1;
        self.search.running = true;
        self.search.error = None;
        self.search.worktree = Some(worktree.clone());
        self.search.sent = query.clone();
        self.git.send(Cmd::Search {
            worktree,
            query,
            request: self.search.request,
        });
        cx.notify();
    }

    /// A search has answered.
    ///
    /// An answer that is not the last one asked for is **dropped**: one types,
    /// and the results of a query already replaced would flash on screen and
    /// then be replaced in turn.
    pub(super) fn search_done(
        &mut self,
        request: u64,
        result: Result<Results, String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if request != self.search.request {
            return;
        }
        self.search.running = false;
        self.search.folded.clear();
        match result {
            Ok(results) => {
                self.search.results = Rc::new(results);
                self.search.error = None;
                // The first hit is selected, and its file previewed: a list one
                // has to click before seeing anything is a list one clicks
                // through.
                let rows = search::rows(&self.search.results, &self.search.folded);
                self.search.selected = search::first_hit(&rows);
                self.sync_search_preview(window, cx);
                // **A search one asked for hands the list the focus.** The
                // bare arrows belong to whoever holds it, and `InputState`
                // binds them itself — deeper in the context stack than the
                // panel, so it wins; a result list one has to click before
                // walking it is a list one walks with the mouse. But only for
                // `Enter` and the button: the answers that arrive while one is
                // typing must leave the caret exactly where it is. Coming back
                // to the field is `Ctrl+Shift+F`, which is also how one got
                // here.
                if self.search.selected.is_some() && self.search.focus_on_answer {
                    window.focus(&self.search_focus, cx);
                }
            }
            Err(message) => {
                self.search.results = Rc::new(Results::default());
                self.search.selected = None;
                self.search.preview = None;
                self.search.error = Some(message);
            }
        }
        cx.notify();
    }

    /// Selects a row and shows what it points at.
    pub(super) fn select_search_row(
        &mut self,
        index: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        // A click in the list gives it the focus, as a click in the review's
        // does: without it the arrows would keep walking whatever had it
        // before — the search field, most often, where they move a caret.
        window.focus(&self.search_focus, cx);
        self.search.selected = Some(index);
        self.sync_search_preview(window, cx);
        cx.notify();
    }

    /// Moves the cursor, and reveals it.
    pub(super) fn step_search(
        &mut self,
        delta: isize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let rows = search::rows(&self.search.results, &self.search.folded);
        let Some(next) = search::step(&rows, self.search.selected, delta) else {
            return;
        };
        self.search.selected = Some(next);
        self.search_scroll
            .scroll_to_item(next, gpui::ScrollStrategy::Top);
        self.sync_search_preview(window, cx);
        cx.notify();
    }

    /// Folds or unfolds a file, and puts the cursor on it.
    ///
    /// The cursor moves with the fold rather than staying where it was: the rows
    /// below have just changed meaning, and an index left in place would point
    /// at another hit.
    pub(super) fn toggle_search_fold(
        &mut self,
        file: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(path) = self.search.results.files.get(file).map(|f| f.path.clone()) else {
            return;
        };
        if !self.search.folded.remove(&path) {
            self.search.folded.insert(path);
        }
        let rows = search::rows(&self.search.results, &self.search.folded);
        self.search.selected = rows.iter().position(|row| row == &Row::File { file });
        self.sync_search_preview(window, cx);
        cx.notify();
    }

    /// What the selected row points at: `Enter`, or a double click.
    ///
    /// It goes through `jump_to`, which is the funnel every opening uses: the
    /// editing screen comes up, the caret lands on the line, and the trail
    /// records where one came from — so `Ctrl+O` comes straight back to the
    /// result list's file.
    pub(super) fn open_search_row(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(row) = self.selected_search_row() else {
            return;
        };
        if let Row::File { file } = row {
            self.toggle_search_fold(file, window, cx);
            return;
        }
        let Some(path) = search::path_of(&self.search.results, row).map(PathBuf::from) else {
            return;
        };
        // A hit found in a worktree one has since left belongs to a tree the
        // rest of the window is no longer showing — and `jump_to` opens in the
        // **selected** one, so it would read another file of the same name or
        // none at all. Said out loud: a button that does nothing reads as a
        // broken button.
        if self.search.worktree.as_deref() != self.active.as_deref() {
            self.announce(tr!("search-other-worktree"), cx);
            return;
        }
        let line = search::line_of(&self.search.results, row).saturating_sub(1);
        self.jump_to(
            path,
            crate::ui::explorer::Landing::Position { line, character: 0 },
            window,
            cx,
        );
    }

    fn selected_search_row(&self) -> Option<Row> {
        let rows = search::rows(&self.search.results, &self.search.folded);
        rows.get(self.search.selected?).copied()
    }

    /// Asks for the file the cursor points at, unless it is already there.
    ///
    /// Walking the hits of one file must not re-read it at every arrow: only
    /// the line to reveal changes.
    fn sync_search_preview(&mut self, _window: &mut Window, cx: &mut Context<Self>) {
        let (Some(row), Some(worktree)) =
            (self.selected_search_row(), self.search.worktree.clone())
        else {
            return;
        };
        let Some(path) = search::path_of(&self.search.results, row).map(PathBuf::from) else {
            return;
        };
        let line = search::line_of(&self.search.results, row);
        match self.search.preview.as_mut() {
            Some(preview) if preview.path == path && preview.worktree == worktree => {
                preview.line = line;
            }
            _ => {
                self.search.preview = None;
                self.pending_preview = Some((worktree.clone(), path.clone(), line));
                self.git.send(Cmd::ReadPreview { worktree, path });
            }
        }
        self.reveal_preview_line(cx);
    }

    /// Brings the previewed line into view, a third of the way down.
    ///
    /// `Top` and not the default: a hit revealed at the very bottom of the
    /// panel shows none of what follows it, which is what one reads a preview
    /// for. The strategy is strict, so it moves even when the line is already
    /// on screen — the point is to put it *there*, not merely to make it
    /// visible.
    fn reveal_preview_line(&mut self, _cx: &mut Context<Self>) {
        let Some(preview) = self.search.preview.as_ref() else {
            return;
        };
        let line = preview.line.saturating_sub(1) as usize;
        let above = PREVIEW_CONTEXT.min(line);
        // **Strict**, so it moves even when the line is already on screen:
        // `scroll_to_item` does nothing in that case, which is right for an
        // arrow stepping one row and wrong here — walking to the next hit of a
        // file already shown would leave the preview exactly where it was, and
        // a key that moves nothing visible reads as a dead key.
        self.search_preview_scroll
            .scroll_to_item_strict(line - above, gpui::ScrollStrategy::Top);
    }

    /// A previewed file has arrived.
    pub(super) fn preview_arrived(
        &mut self,
        worktree: PathBuf,
        path: PathBuf,
        content: Result<crate::files::Content, String>,
        cx: &mut Context<Self>,
    ) {
        // The answer to a preview one has already left. Dropped rather than
        // shown: the cursor has moved, and the file under it is another.
        let Some((wanted_worktree, wanted_path, line)) = self.pending_preview.clone() else {
            return;
        };
        if (wanted_worktree.as_path(), wanted_path.as_path())
            != (worktree.as_path(), path.as_path())
        {
            return;
        }
        self.pending_preview = None;
        // The line is re-read from where the cursor stands **now**: one walks
        // two hits of the same file faster than the file is read, and the line
        // asked for a round trip ago is not the one under the cursor.
        let line = self
            .selected_search_row()
            .filter(|row| search::path_of(&self.search.results, *row) == Some(path.as_path()))
            .map(|row| search::line_of(&self.search.results, row))
            .unwrap_or(line);
        let (lines, highlights, error) = match content {
            Ok(content) => {
                let theme = cx.theme().highlight_theme.clone();
                let highlights = DocumentHighlights::compute(&path, &content.text, &theme);
                (
                    content
                        .text
                        .split('\n')
                        .map(|line| SharedString::from(line.to_string()))
                        .collect(),
                    highlights,
                    None,
                )
            }
            Err(message) => (Vec::new(), DocumentHighlights::default(), Some(message)),
        };
        self.search.preview = Some(Preview {
            worktree,
            path,
            lines: Rc::new(lines),
            highlights: Rc::new(highlights),
            line,
            error,
        });
        self.reveal_preview_line(cx);
        cx.notify();
    }

    // — Painting ————————————————————————————————————————————————————

    pub(super) fn render_search(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let bar = self.render_search_bar(cx);
        let status = self.render_search_status(cx);
        let body = if self.search.results.files.is_empty() {
            self.render_search_empty(cx).into_any_element()
        } else {
            self.render_search_rows(window, cx).into_any_element()
        };
        v_flex()
            .size_full()
            .track_focus(&self.search_focus)
            .key_context(crate::ui::shortcuts::search_context(
                crate::ui::settings::Settings::global(cx).vim_mode,
            ))
            .child(bar)
            .children(status)
            .child(div().flex_1().min_h_0().child(body))
    }

    /// The field, the two options, and the glob.
    fn render_search_bar(&mut self, cx: &mut Context<Self>) -> impl IntoElement {
        let running = self.search.running;
        v_flex()
            .w_full()
            .p_1()
            .gap_1()
            .border_b_1()
            .border_color(cx.theme().border)
            .child(
                h_flex()
                    .w_full()
                    .gap_1()
                    .items_center()
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .child(Input::new(&self.search_input).small()),
                    )
                    .child(
                        Button::new("search-run")
                            .primary()
                            .xsmall()
                            .icon(icon("search"))
                            .tooltip(tr!("search-run"))
                            .disabled(running)
                            .on_click(
                                cx.listener(|this, _, _window, cx| this.run_search(true, cx)),
                            ),
                    ),
            )
            .child(
                h_flex()
                    .w_full()
                    .gap_2()
                    .items_center()
                    .child(
                        Checkbox::new("search-regex")
                            .label(tr!("search-regex"))
                            .checked(self.search_regex)
                            .on_click(cx.listener(|this, checked: &bool, _window, cx| {
                                this.search_regex = *checked;
                                // An option is a question asked again: leaving
                                // the previous answer under a changed switch
                                // would say the switch does nothing.
                                this.search_typed(cx);
                                cx.notify();
                            })),
                    )
                    .child(
                        Checkbox::new("search-word")
                            .label(tr!("search-whole-word"))
                            .checked(self.search_whole_word)
                            .on_click(cx.listener(|this, checked: &bool, _window, cx| {
                                this.search_whole_word = *checked;
                                this.search_typed(cx);
                                cx.notify();
                            })),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .child(Input::new(&self.search_glob_input).small()),
                    ),
            )
    }

    /// One line saying what came back, and nothing when nothing has been asked.
    ///
    /// The cap is **said** here: a list that stops at two thousand hits without
    /// a word reads as a project with two thousand hits.
    fn render_search_status(&mut self, cx: &mut Context<Self>) -> Option<impl IntoElement> {
        let (muted, danger) = (cx.theme().muted_foreground, cx.theme().danger);
        // Results belonging to a checkout one has left are kept — a search is
        // worth minutes and switching worktrees to glance at something should
        // not spend it — but they are **labelled**: silently showing another
        // project's hits under this project's name is the one thing this panel
        // must not do.
        let stale = self.search.worktree.is_some()
            && self.search.worktree.as_deref() != self.active.as_deref();
        let (text, colour) = if let Some(error) = self.search.error.clone() {
            (SharedString::from(error), danger)
        } else if self.search.running {
            (tr!("search-running"), muted)
        } else if self.search.results.total == 0 {
            return None;
        } else {
            let counted = tr!("search-count", {
                hits: self.search.results.total,
                files: self.search.results.files.len()
            });
            let mut text = counted.to_string();
            if self.search.results.truncated {
                text = format!("{text} · {}", tr!("search-truncated"));
            }
            if stale {
                text = format!("{text} · {}", tr!("search-other-worktree"));
            }
            (SharedString::from(text), if stale { danger } else { muted })
        };
        Some(
            div()
                .w_full()
                .px_2()
                .py_0p5()
                .text_xs()
                .text_color(colour)
                .child(text),
        )
    }

    fn render_search_empty(&mut self, cx: &mut App) -> impl IntoElement {
        let message = if self.search.error.is_some() {
            tr!("search-failed")
        } else if self.search.sent.is_empty() {
            tr!("search-prompt")
        } else if self.search.running {
            tr!("search-running")
        } else {
            tr!("find-no-match")
        };
        v_flex()
            .size_full()
            .items_center()
            .justify_center()
            .gap_2()
            .text_color(cx.theme().muted_foreground)
            .child(icon("search"))
            .child(div().text_sm().px_4().text_center().child(message))
    }

    fn render_search_rows(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let results = self.search.results.clone();
        let rows = Rc::new(search::rows(&results, &self.search.folded));
        let folded = Rc::new(self.search.folded.clone());
        let selected = self.search.selected;
        let query = self.search.sent.clone();
        let look = Look::of(cx);
        let entity = cx.entity();
        let count = rows.len();
        let handle = self.search_scroll.clone();
        let build = move |index: usize, cx: &mut App| {
            let Some(row) = rows.get(index).copied() else {
                return div().into_any_element();
            };
            render_row(
                index,
                row,
                &results,
                &folded,
                selected == Some(index),
                &query,
                &look,
                &entity,
                cx,
            )
        };
        self.scrolled(
            RESULTS_SCROLL,
            &handle.clone(),
            crate::ui::motion::Axes::Vertical,
            window,
            uniform_list("search-rows", count, move |range, _window, cx| {
                range.map(|index| build(index, cx)).collect::<Vec<_>>()
            })
            .size_full()
            .px_1()
            .track_scroll(&handle),
            cx,
        )
    }

    /// The file beside the list.
    pub(super) fn render_search_preview(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let Some(preview) = self.search.preview.as_ref() else {
            return v_flex()
                .size_full()
                .items_center()
                .justify_center()
                .gap_2()
                .text_color(cx.theme().muted_foreground)
                .child(icon("file-code"))
                .child(div().text_sm().child(tr!("search-preview-empty")))
                .into_any_element();
        };
        let header = h_flex()
            .h(crate::ui::theme::bar_height(cx))
            .w_full()
            .px_2()
            .gap_1()
            .items_center()
            .border_b_1()
            .border_color(cx.theme().border)
            .child(crate::ui::file_icons::file_icon(&preview.path, cx))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .text_xs()
                    .child(SharedString::from(preview.path.display().to_string())),
            )
            .child(
                Button::new("search-preview-open")
                    .ghost()
                    .xsmall()
                    .icon(icon("pencil"))
                    .tooltip(tr!("search-open"))
                    .on_click(cx.listener(|this, _, window, cx| this.open_search_row(window, cx))),
            );
        if let Some(error) = preview.error.clone() {
            return v_flex()
                .size_full()
                .child(header)
                .child(
                    div()
                        .flex_1()
                        .p_4()
                        .text_sm()
                        .text_color(cx.theme().danger)
                        .child(SharedString::from(error)),
                )
                .into_any_element();
        }

        let mono = cx.theme().mono_font_family.clone();
        let font_size = px(crate::ui::settings::Settings::global(cx).diff_font_size);
        let line_height = crate::ui::diff_view::line_height(font_size);
        let look = Look::of(cx);
        let lines = preview.lines.clone();
        let highlights = preview.highlights.clone();
        let marked = preview.line.saturating_sub(1) as usize;
        let query = self.search.sent.clone();
        let digits = digits_of(lines.len());
        let gutter = font_size * 0.62 * digits as f32 + px(8.);
        let count = lines.len();
        let handle = self.search_preview_scroll.clone();
        let widest = widest_line(&lines);
        let body = self.scrolled(
            PREVIEW_SCROLL,
            &handle.clone(),
            // Both axes: the lines are unconstrained, so a long one is reached
            // by scrolling sideways, exactly as in the diff.
            crate::ui::motion::Axes::Both,
            window,
            uniform_list("search-preview-lines", count, move |range, _window, cx| {
                range
                    .map(|index| {
                        render_preview_line(
                            index,
                            &lines,
                            &highlights,
                            index == marked,
                            &query,
                            gutter,
                            line_height,
                            &look,
                            cx,
                        )
                    })
                    .collect::<Vec<_>>()
            })
            .size_full()
            .font_family(mono)
            .text_size(font_size)
            // Without `Unconstrained` the lines are clipped to the view's width
            // and there is nothing left to scroll horizontally to; the width
            // comes from the single item named here — the longest line, without
            // which scrolling stops at the width of the first.
            .with_horizontal_sizing_behavior(ListHorizontalSizingBehavior::Unconstrained)
            .with_width_from_item(Some(widest))
            .track_scroll(&handle),
            cx,
        );
        v_flex()
            .size_full()
            .child(header)
            .child(div().flex_1().min_h_0().child(body))
            .into_any_element()
    }
}

/// How many lines of context are kept above the revealed one.
const PREVIEW_CONTEXT: usize = 4;

/// How long the typing has to stop before the search goes out.
///
/// Three hundred milliseconds is a pause between words and not between
/// letters: shorter, a slow typist pays a `git grep` per letter; longer, the
/// list feels like it is answering the previous question.
const DEBOUNCE: std::time::Duration = std::time::Duration::from_millis(300);

/// Below this many characters, only `Enter` searches. See `search_typed`.
const MIN_AUTO: usize = 2;

/// What the theme gives a row, read once per frame and not per row.
#[derive(Clone, Copy)]
struct Look {
    row: Pixels,
    radius: Pixels,
    muted: gpui::Hsla,
    accent: gpui::Hsla,
    text: gpui::Hsla,
    /// The colour occurrences are picked out in, the same as every other search
    /// in this window.
    hit: gpui::Hsla,
}

impl Look {
    fn of(cx: &App) -> Self {
        Self {
            row: crate::ui::theme::row_height(cx),
            radius: cx.theme().radius,
            muted: cx.theme().muted_foreground,
            accent: cx.theme().accent,
            text: cx.theme().foreground,
            hit: crate::ui::find::highlight_color(false, cx),
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn render_row(
    index: usize,
    row: Row,
    results: &Rc<Results>,
    folded: &Rc<HashSet<PathBuf>>,
    selected: bool,
    query: &Query,
    look: &Look,
    entity: &Entity<ClaudhubApp>,
    cx: &App,
) -> gpui::AnyElement {
    match row {
        Row::File { file } => {
            let Some(hits) = results.files.get(file) else {
                return div().into_any_element();
            };
            let open = !folded.contains(&hits.path);
            let entity = entity.clone();
            h_flex()
                .id(("search-file", index))
                .h(look.row)
                .w_full()
                .px_1()
                .gap_1()
                .items_center()
                .rounded(look.radius)
                .cursor_pointer()
                .when(selected, |el| el.bg(look.accent.opacity(0.4)))
                .hover(|s| s.bg(look.accent.opacity(0.3)))
                .on_click(move |_, window, cx| {
                    entity.update(cx, |this, cx| this.toggle_search_fold(file, window, cx));
                })
                .child(
                    icon(if open {
                        "chevron-down"
                    } else {
                        "chevron-right"
                    })
                    .xsmall(),
                )
                .child(crate::ui::file_icons::file_icon(&hits.path, cx))
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .truncate()
                        .text_xs()
                        .text_color(look.text)
                        .child(SharedString::from(hits.path.display().to_string())),
                )
                .child(div().flex_none().text_xs().text_color(look.muted).child(
                    SharedString::from(if hits.capped {
                        format!("{}+", hits.hits.len())
                    } else {
                        hits.hits.len().to_string()
                    }),
                ))
                .into_any_element()
        }
        Row::Hit { file, hit } => {
            let Some(line) = results.files.get(file).and_then(|file| file.hits.get(hit)) else {
                return div().into_any_element();
            };
            let entity = entity.clone();
            // The leading indentation is dropped: a hit six levels deep would
            // otherwise show nothing but its indentation in a narrow column.
            let text = line.text.trim_start();
            let marks = if query.regex {
                Vec::new()
            } else {
                crate::ui::find::find_all(&query.text, text)
            };
            let shown = SharedString::from(text.to_string());
            h_flex()
                .id(("search-hit", index))
                .h(look.row)
                .w_full()
                .px_1()
                .gap_2()
                .items_center()
                .rounded(look.radius)
                .cursor_pointer()
                .when(selected, |el| el.bg(look.accent.opacity(0.4)))
                .hover(|s| s.bg(look.accent.opacity(0.3)))
                .on_click(move |event, window, cx| {
                    let open = event.click_count() > 1;
                    entity.update(cx, |this, cx| {
                        this.select_search_row(index, window, cx);
                        if open {
                            this.open_search_row(window, cx);
                        }
                    });
                })
                .child(
                    div()
                        .w(px(44.))
                        .flex_none()
                        .text_right()
                        .text_xs()
                        .text_color(look.muted)
                        .child(SharedString::from(line.line.to_string())),
                )
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .truncate()
                        .text_xs()
                        .font_family(cx.theme().mono_font_family.clone())
                        .child(if marks.is_empty() {
                            div().child(shown).into_any_element()
                        } else {
                            StyledText::new(shown)
                                .with_highlights(marks.into_iter().map(|range| {
                                    (
                                        range,
                                        gpui::HighlightStyle {
                                            background_color: Some(look.hit),
                                            ..Default::default()
                                        },
                                    )
                                }))
                                .into_any_element()
                        }),
                )
                .into_any_element()
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn render_preview_line(
    index: usize,
    lines: &Rc<Vec<SharedString>>,
    highlights: &Rc<DocumentHighlights>,
    marked: bool,
    query: &Query,
    gutter: Pixels,
    line_height: Pixels,
    look: &Look,
    _cx: &App,
) -> gpui::AnyElement {
    let Some(text) = lines.get(index).cloned() else {
        return div().into_any_element();
    };
    let styles = highlights.line(index);
    let marks: Vec<_> = if query.regex {
        Vec::new()
    } else {
        crate::ui::find::find_all(&query.text, &text)
            .into_iter()
            .map(|range| (range, look.hit))
            .collect()
    };
    let content = if marks.is_empty() {
        if styles.is_empty() {
            div().child(text).into_any_element()
        } else {
            StyledText::new(text)
                .with_highlights(styles.iter().cloned())
                .into_any_element()
        }
    } else {
        StyledText::new(text)
            .with_highlights(crate::ui::highlight::overlay(styles, &marks))
            .into_any_element()
    };
    h_flex()
        .h(line_height)
        .items_center()
        .whitespace_nowrap()
        .when(marked, |el| el.bg(look.accent.opacity(0.35)))
        .child(
            div()
                .w(gutter)
                .flex_none()
                .text_right()
                .pr_1()
                .text_color(look.muted)
                .child(SharedString::from((index + 1).to_string())),
        )
        .child(content)
        .into_any_element()
}

fn digits_of(lines: usize) -> usize {
    lines.max(1).to_string().len()
}

/// The longest line, which is what the horizontal scroll is sized on.
fn widest_line(lines: &[SharedString]) -> usize {
    let mut widest = 0usize;
    let mut best = 0usize;
    for (index, line) in lines.iter().enumerate() {
        if line.len() > widest {
            widest = line.len();
            best = index;
        }
    }
    best
}

/// The fields the search screen owns, handed to the constructor in one piece.
pub(super) struct SearchInputs {
    pub text: Entity<InputState>,
    pub glob: Entity<InputState>,
    pub focus: FocusHandle,
}

impl SearchInputs {
    pub fn new(window: &mut Window, cx: &mut Context<ClaudhubApp>) -> Self {
        let text = cx.new(|cx| InputState::new(window, cx).placeholder(tr!("search-placeholder")));
        // Typing searches, once it stops (`search_typed`); `Enter` searches
        // now and hands the list the focus — it is the key that says "I have
        // finished typing, I am going to read".
        fn watch(
            this: &mut ClaudhubApp,
            event: &gpui_component::input::InputEvent,
            cx: &mut Context<ClaudhubApp>,
        ) {
            use gpui_component::input::InputEvent;
            match event {
                InputEvent::Change => this.search_typed(cx),
                InputEvent::PressEnter { .. } => this.run_search(true, cx),
                _ => {}
            }
        }
        cx.subscribe(&text, |this, _, event, cx| watch(this, event, cx))
            .detach();
        let glob = cx.new(|cx| InputState::new(window, cx).placeholder(tr!("search-glob")));
        cx.subscribe(&glob, |this, _, event, cx| watch(this, event, cx))
            .detach();
        Self {
            text,
            glob,
            focus: cx.focus_handle(),
        }
    }
}
