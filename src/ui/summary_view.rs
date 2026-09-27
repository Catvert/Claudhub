//! A board's tabs, and its home: the digest of a worktree in one screen.
//!
//! Two columns: on the left the state of things — the branch, its pull
//! request and CI, what waits for a commit, the review, the to-do list, the
//! principal note, what runs —; on the right **the terminals themselves**,
//! a sub-tab each, bare — no window round them: an agent is worked with,
//! not read about. There was a strip of what waited for the hand over the
//! columns; it said again what the cards under it said.
//!
//! A card's title opens its tab, the whole of it; what is done in one
//! gesture is done on the card — a task ticked or added, a recipe started,
//! a pull. An empty card says what fills it, and has the button for it.

use std::path::Path;

use gpui_kit::component::{
    button::{Button, ButtonVariants as _},
    checkbox::Checkbox,
    h_flex,
    input::Input,
    menu::DropdownMenu as _,
    v_flex, ActiveTheme, Disableable as _, Sizable as _,
};
use gpui_kit::{div, prelude::*, px, AnyElement, Context, Focusable as _, SharedString, Window};

use crate::tr;
use crate::ui::app::ClaudhubApp;
use crate::ui::canvas_view::Hang;
use crate::ui::focus::{self, View};
use crate::ui::focus_view::view_name;
use crate::ui::icons::icon;
use crate::ui::overview::{self, Doing, Node};

/// The open tasks the home lists; the tab has the rest.
const HOME_TASKS: usize = 12;
/// The lines of the principal note the home shows.
const HOME_NOTE_LINES: usize = 14;
/// The commits the branch card lists.
const HOME_COMMITS: usize = 5;
/// The files waiting for a commit the home lists.
const HOME_FILES: usize = 8;
/// The review's heaviest files the home draws a bar for.
const HOME_HEAVIEST: usize = 6;
impl ClaudhubApp {
    /// A board's tabs — the home first —, each saying what waits in it,
    /// and at the right what adds one more.
    pub(super) fn render_board_tabs(
        &mut self,
        path: &Path,
        view: View,
        at_work: &overview::AtWork,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        // Its to-do list and notes are read with the worktree's review
        // state, which a worktree nobody opened does not have yet.
        self.ensure_review(path, cx);
        let theme = cx.theme().clone();
        let loudest = overview::loudest(
            self.board_terminals(path)
                .iter()
                .filter_map(|id| at_work.terminals.get(id).copied()),
        );
        let tabs: Vec<AnyElement> = View::ALL
            .into_iter()
            .map(|tab| {
                let (glyph, title) = view_name(tab);
                let lit = tab == view;
                let count = match tab {
                    View::Git => self
                        .summaries
                        .get(path)
                        .filter(|summary| !summary.is_empty())
                        .map(|summary| summary.files),
                    View::Todo => self
                        .review
                        .get(path)
                        .and_then(|state| state.todo.as_ref())
                        .map(|todo| todo.tasks.len() - todo.done())
                        .filter(|open| *open > 0),
                    View::Terminals => Some(self.board_terminals(path).len()).filter(|n| *n > 0),
                    View::Home | View::Review | View::Notes => None,
                };
                // The agents' tab wears the loudest of them.
                let signal = (tab == View::Terminals)
                    .then(|| match loudest {
                        Doing::Working => Some(theme.warning),
                        Doing::Waiting => Some(theme.danger),
                        Doing::Rest => None,
                    })
                    .flatten();
                let board = path.to_path_buf();
                h_flex()
                    .id(SharedString::from(format!("focus-tab-{tab:?}")))
                    .flex_none()
                    .h(super::theme::bar_height(cx))
                    .px_3()
                    .gap_1p5()
                    .items_center()
                    .cursor_pointer()
                    .rounded(theme.radius)
                    .text_sm()
                    .when(lit, |el| {
                        el.bg(theme.background)
                            .border_1()
                            .border_color(theme.border)
                            .text_color(theme.foreground)
                    })
                    .when(!lit, |el| {
                        el.text_color(theme.muted_foreground)
                            .hover(|style| style.bg(theme.list_hover))
                    })
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.show_board_view(&board, tab, cx);
                    }))
                    .child(
                        icon(glyph)
                            .xsmall()
                            .when(lit, |icon| icon.text_color(theme.ring)),
                    )
                    .child(title)
                    .children(count.map(|count| {
                        div()
                            .text_xs()
                            .text_color(theme.muted_foreground)
                            .child(SharedString::from(count.to_string()))
                    }))
                    .children(
                        signal.map(|tint| div().flex_none().size(px(6.)).rounded_full().bg(tint)),
                    )
                    .into_any_element()
            })
            .collect();
        let detail = self.view_detail(path, view, cx);
        let add = match view {
            View::Terminals => {
                let worktree = path.to_path_buf();
                Some(
                    Button::new("focus-view-add")
                        .ghost()
                        .small()
                        .icon(icon("plus"))
                        .label(tr!("terminal-new"))
                        .on_click(cx.listener(move |this, _, window, cx| {
                            this.open_focus_terminal(&worktree, window, cx);
                        })),
                )
            }
            View::Notes => {
                let worktree = path.to_path_buf();
                Some(
                    Button::new("focus-view-add")
                        .ghost()
                        .small()
                        .icon(icon("plus"))
                        .label(tr!("overview-add-note"))
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.add_home_note(Hang::Worktree(worktree.clone()), cx);
                        })),
                )
            }
            _ => None,
        };
        let (app, hang) = (cx.entity().downgrade(), Hang::Worktree(path.to_path_buf()));
        h_flex()
            .flex_none()
            .w_full()
            .gap_1()
            .items_center()
            .children(tabs)
            .child(
                h_flex()
                    .flex_1()
                    .min_w_0()
                    .px_2()
                    .justify_end()
                    .overflow_hidden()
                    .text_xs()
                    .text_color(theme.muted_foreground)
                    .children(detail.filter(|_| view != View::Home)),
            )
            .children(add)
            .child(
                Button::new(SharedString::from(format!(
                    "focus-board-add-{}",
                    path.display()
                )))
                .ghost()
                .small()
                .icon(icon("chevron-down"))
                .tooltip(tr!("overview-add"))
                .dropdown_menu(move |menu, _, cx| {
                    super::overview_view::add_items(&app, &hang, true, menu, cx)
                }),
            )
            .into_any_element()
    }

    /// The home: the strip of what waits, then the two columns.
    pub(super) fn render_home_view(
        &mut self,
        path: &Path,
        at_work: &overview::AtWork,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        // The worktree on show has the editor's readings — its pull request
        // and CI, its review's list —, asked for here as their panels would.
        if self.active.as_deref() == Some(path) {
            self.ensure_github(cx);
            if let Some(range) = self.review.get(path).and_then(|state| {
                super::review::branch_panel_range(
                    state.base.as_deref(),
                    state.review_point.as_ref(),
                    state.since_review,
                )
            }) {
                self.ensure_files(range, cx);
            }
        }
        let left = v_flex()
            .id("focus-home-left")
            .flex_grow(2.)
            .flex_basis(px(0.))
            .min_w(px(300.))
            .h_full()
            .gap_3()
            .overflow_y_scroll()
            .child(self.home_branch(path, cx))
            .child(self.home_to_commit(path, cx))
            .child(self.home_review(path, cx))
            .child(self.home_tasks(path, cx))
            .child(self.home_note(path, cx))
            .children(self.home_run(path, cx));
        let right = v_flex()
            .flex_grow(3.)
            .flex_basis(px(0.))
            .min_w(px(360.))
            .h_full()
            .child(self.home_terminals(path, at_work, window, cx));
        v_flex()
            .id(SharedString::from(format!("focus-home-{}", path.display())))
            .size_full()
            .child(
                h_flex()
                    .flex_1()
                    .min_h_0()
                    .w_full()
                    .gap_3()
                    .child(left)
                    .child(right),
            )
            .into_any_element()
    }

    /// A card of the home: its title — which opens `tab` — what it says of
    /// itself beside it, what acts at its right, and its body.
    #[allow(clippy::too_many_arguments)]
    fn home_card(
        &self,
        path: &Path,
        glyph: &'static str,
        title: SharedString,
        tab: View,
        detail: Option<AnyElement>,
        actions: Vec<AnyElement>,
        body: AnyElement,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let theme = cx.theme().clone();
        let board = path.to_path_buf();
        v_flex()
            .w_full()
            .p_3()
            .gap_2()
            .rounded(theme.radius_lg)
            .border_1()
            .border_color(theme.border)
            .bg(theme.background)
            .child(
                h_flex()
                    .w_full()
                    .gap_1p5()
                    .items_center()
                    .child(
                        h_flex()
                            .id(SharedString::from(format!("focus-card-{title}")))
                            .flex_none()
                            .gap_1p5()
                            .items_center()
                            .cursor_pointer()
                            .hover(|style| style.text_color(theme.ring))
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.show_board_view(&board, tab, cx);
                            }))
                            .child(icon(glyph).xsmall().text_color(theme.muted_foreground))
                            .child(
                                div()
                                    .text_sm()
                                    .font_weight(gpui_kit::FontWeight::SEMIBOLD)
                                    .child(title),
                            )
                            .child(
                                icon("chevron-right")
                                    .xsmall()
                                    .text_color(theme.muted_foreground),
                            ),
                    )
                    .child(
                        h_flex()
                            .flex_1()
                            .min_w_0()
                            .overflow_hidden()
                            .text_xs()
                            .text_color(theme.muted_foreground)
                            .children(detail),
                    )
                    .children(actions),
            )
            .child(body)
            .into_any_element()
    }

    /// Goes to a terminal to answer it: the home, that terminal under its
    /// sub-tabs, and the keys in it.
    fn reply_to(&mut self, path: &Path, id: u64, window: &mut Window, cx: &mut Context<Self>) {
        self.home_terminal.insert(path.to_path_buf(), id);
        self.show_board_view(path, View::Home, cx);
        if let Some(view) = self
            .terminals
            .iter()
            .find(|terminal| terminal.view.entity_id().as_u64() == id)
            .map(|terminal| terminal.view.clone())
        {
            super::dialogs::focus_field(&view, window, cx);
        }
    }

    /// The terminals, live: a sub-tab each — what its agent is doing, by
    /// the dot and the word — and the one chosen under them, to type in.
    /// At the right of the sub-tabs, what opens another, and the way to
    /// them all side by side.
    fn home_terminals(
        &mut self,
        path: &Path,
        at_work: &overview::AtWork,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let theme = cx.theme().clone();
        let terminals: Vec<(u64, Option<String>)> = self
            .terminals
            .iter()
            .filter(|terminal| terminal.worktree == path)
            .map(|terminal| (terminal.view.entity_id().as_u64(), terminal.session.clone()))
            .collect();
        let waiting: Vec<(u64, bool)> = terminals
            .iter()
            .map(|(id, _)| (*id, at_work.terminals.get(id) == Some(&Doing::Waiting)))
            .collect();
        let shown = focus::shown_terminal(self.home_terminal.get(path).copied(), &waiting);
        let mut tabs: Vec<AnyElement> = Vec::new();
        for (id, session) in &terminals {
            let (_, name) = self.card_name(&Node::Terminal(*id), cx);
            let doing = at_work.terminals.get(id).copied().unwrap_or(Doing::Rest);
            let activity = session
                .as_deref()
                .and_then(|session| self.agents.session(session));
            let tint = match (activity, doing) {
                (_, Doing::Waiting) | (Some(crate::agent::Activity::Waiting(_)), _) => theme.danger,
                (_, Doing::Working) | (Some(crate::agent::Activity::Working), _) => theme.warning,
                (Some(crate::agent::Activity::Finished), _) => theme.success,
                _ => theme.muted_foreground.opacity(0.5),
            };
            let lit = shown == Some(*id);
            let (board, pressed) = (path.to_path_buf(), *id);
            tabs.push(
                h_flex()
                    .id(("focus-home-terminal-tab", *id as usize))
                    .flex_none()
                    .max_w(px(220.))
                    .h(super::theme::bar_height(cx))
                    .px_2()
                    .gap_1p5()
                    .items_center()
                    .cursor_pointer()
                    .rounded(theme.radius)
                    .text_sm()
                    .when(lit, |el| {
                        el.bg(theme.background)
                            .border_1()
                            .border_color(theme.border)
                            .text_color(theme.foreground)
                    })
                    .when(!lit, |el| {
                        el.text_color(theme.muted_foreground)
                            .hover(|style| style.bg(theme.list_hover))
                    })
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.reply_to(&board, pressed, window, cx);
                    }))
                    .child(div().flex_none().size(px(7.)).rounded_full().bg(tint))
                    .child(div().min_w_0().truncate().child(name))
                    .child(
                        Button::new(("focus-home-terminal-close", *id as usize))
                            .ghost()
                            .xsmall()
                            .icon(icon("x"))
                            .tooltip(tr!("overview-close"))
                            .on_click(cx.listener(move |this, _, window, cx| {
                                cx.stop_propagation();
                                this.ask_close_terminal(
                                    gpui_kit::EntityId::from(pressed),
                                    window,
                                    cx,
                                );
                            })),
                    )
                    .into_any_element(),
            );
        }
        let worktree = path.to_path_buf();
        let every = path.to_path_buf();
        let (app, hang) = (cx.entity().downgrade(), Hang::Worktree(path.to_path_buf()));
        let bar = h_flex()
            .flex_none()
            .w_full()
            .gap_1()
            .items_center()
            .child(
                h_flex()
                    .id("focus-home-terminal-tabs")
                    .flex_1()
                    .min_w_0()
                    .gap_1()
                    .overflow_x_scroll()
                    .children(tabs),
            )
            .child(
                Button::new("focus-home-terminal")
                    .ghost()
                    .xsmall()
                    .icon(icon("plus"))
                    .tooltip(tr!("terminal-new"))
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.open_home_terminal(&worktree, window, cx);
                    })),
            )
            .child(
                Button::new("focus-home-agent-add")
                    .ghost()
                    .xsmall()
                    .icon(icon("bot"))
                    .label(tr!("focus-home-add"))
                    .dropdown_menu(move |menu, _, cx| {
                        super::overview_view::add_items(&app, &hang, false, menu, cx)
                    }),
            )
            .child(
                Button::new("focus-home-terminals-all")
                    .ghost()
                    .xsmall()
                    .icon(icon("columns-2"))
                    .tooltip(tr!("focus-home-terminals-all"))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.show_board_view(&every, View::Terminals, cx);
                    })),
            );
        let body = match shown.and_then(|id| {
            self.terminals
                .iter()
                .find(|terminal| terminal.view.entity_id().as_u64() == id)
        }) {
            // Bare: its name is its sub-tab's, and a window round a
            // terminal that fills the column had nothing to fold or move.
            Some(terminal) => {
                let view = terminal.view.clone();
                let doing = at_work
                    .terminals
                    .get(&view.entity_id().as_u64())
                    .copied()
                    .unwrap_or(Doing::Rest);
                let focused = view.focus_handle(cx).contains_focused(window, cx);
                div()
                    .relative()
                    .size_full()
                    .child(
                        v_flex()
                            .size_full()
                            .rounded(theme.radius_lg)
                            .overflow_hidden()
                            .bg(theme.background)
                            .border_1()
                            .border_color(theme.border)
                            // `v_flex`: the terminal's `size_full` resolves
                            // against a definite height. Cached, as on the
                            // boards: see `render_tile`.
                            .child(v_flex().flex_1().min_h_0().child(
                                view.cached(gpui_kit::StyleRefinement::default().size_full()),
                            )),
                    )
                    .child(super::overview_view::tile_outline(
                        doing, focused, 1., &theme,
                    ))
                    .into_any_element()
            }
            None => {
                let worktree = path.to_path_buf();
                v_flex()
                    .size_full()
                    .gap_2()
                    .items_center()
                    .justify_center()
                    .rounded(theme.radius_lg)
                    .border_1()
                    .border_color(theme.border)
                    .bg(theme.background)
                    .child(
                        div()
                            .text_xs()
                            .text_color(theme.muted_foreground)
                            .child(tr!("focus-terminals-none")),
                    )
                    .child(
                        Button::new("focus-home-terminal-open")
                            .small()
                            .icon(icon("square-terminal"))
                            .label(tr!("terminal-new"))
                            .on_click(cx.listener(move |this, _, window, cx| {
                                this.open_home_terminal(&worktree, window, cx);
                            })),
                    )
                    .into_any_element()
            }
        };
        v_flex()
            .size_full()
            .gap_1()
            .child(bar)
            .child(div().flex_1().min_h_0().w_full().child(body))
            .into_any_element()
    }

    /// Opens a shell from the home and shows it there, under its sub-tab,
    /// the keys in it.
    fn open_home_terminal(&mut self, worktree: &Path, window: &mut Window, cx: &mut Context<Self>) {
        self.open_terminal(worktree, super::terminal_view::Launch::shell(), window, cx);
        if let Some(view) = self.terminals.last().map(|terminal| terminal.view.clone()) {
            self.home_terminal
                .insert(worktree.to_path_buf(), view.entity_id().as_u64());
            super::dialogs::focus_field(&view, window, cx);
        }
        cx.notify();
    }

    /// The to-do list: its open tasks, ticked here, how many are done, and
    /// — on the worktree on show — the field that adds one.
    fn home_tasks(&mut self, path: &Path, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme().clone();
        let todo = self.review.get(path).and_then(|state| state.todo.clone());
        let on_show = self.active.as_deref() == Some(path);
        let mut rows: Vec<AnyElement> = Vec::new();
        if let Some(todo) = &todo {
            for task in todo.tasks.iter().filter(|task| !task.done).take(HOME_TASKS) {
                let (worktree, line) = (path.to_path_buf(), task.line);
                rows.push(
                    h_flex()
                        .gap_2()
                        .items_center()
                        .pl(px(12. * task.depth.min(4) as f32))
                        .child(
                            Checkbox::new(("focus-home-tick", line))
                                .checked(false)
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    this.toggle_task_in(worktree.clone(), line, true, cx);
                                })),
                        )
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .text_sm()
                                .child(SharedString::from(task.label.clone())),
                        )
                        .into_any_element(),
                );
            }
            let done = todo.done();
            if done > 0 {
                rows.push(
                    h_flex()
                        .gap_1p5()
                        .items_center()
                        .text_xs()
                        .text_color(theme.muted_foreground)
                        .child(icon("check").xsmall())
                        .child(tr!("focus-home-done", { count: done }))
                        .into_any_element(),
                );
            }
        } else if !on_show {
            rows.push(
                div()
                    .text_xs()
                    .text_color(theme.muted_foreground)
                    .child(tr!("todo-none"))
                    .into_any_element(),
            );
        }
        if on_show {
            rows.push(
                h_flex()
                    .gap_2()
                    .items_center()
                    .child(icon("plus").xsmall().text_color(theme.muted_foreground))
                    .child(div().flex_1().child(Input::new(&self.task_input).xsmall()))
                    .into_any_element(),
            );
        }
        let detail = self.view_detail(path, View::Todo, cx);
        let (glyph, title) = view_name(View::Todo);
        let body = v_flex().gap_1p5().children(rows).into_any_element();
        self.home_card(path, glyph, title, View::Todo, detail, Vec::new(), body, cx)
    }

    /// The principal note: its title and its first lines, the headings
    /// standing out. Without a note, the button that writes one.
    fn home_note(&mut self, path: &Path, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme().clone();
        let (glyph, title) = view_name(View::Notes);
        let Some(note) = self.principal_note(path, cx) else {
            let worktree = path.to_path_buf();
            let body = v_flex()
                .gap_2()
                .items_start()
                .child(
                    div()
                        .text_xs()
                        .text_color(theme.muted_foreground)
                        .child(tr!("focus-home-note-empty")),
                )
                .child(
                    Button::new("focus-home-write")
                        .small()
                        .icon(icon("pencil"))
                        .label(tr!("focus-home-write-note"))
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.add_home_note(Hang::Worktree(worktree.clone()), cx);
                            this.show_board_view(&worktree, View::Notes, cx);
                        })),
                )
                .into_any_element();
            return self.home_card(path, glyph, title, View::Notes, None, Vec::new(), body, cx);
        };
        let pinned = super::store::Store::global(cx)
            .worktrees
            .get(path)
            .and_then(|state| state.pinned_note.as_deref())
            == Some(note.as_path());
        let (_, heading) = self.card_name(&Node::Note(note.clone()), cx);
        let lines: Vec<String> = self
            .canvas_entry(&note)
            .map(|(_, entry)| {
                entry
                    .node
                    .body
                    .lines()
                    .map(str::trim_end)
                    .filter(|line| !line.trim().is_empty())
                    .take(HOME_NOTE_LINES)
                    .map(str::to_string)
                    .collect()
            })
            .unwrap_or_default();
        let body = v_flex()
            .gap_0p5()
            .child(
                h_flex()
                    .gap_1()
                    .items_center()
                    .when(pinned, |el| {
                        el.child(icon("pin").xsmall().text_color(theme.ring))
                    })
                    .child(
                        div()
                            .min_w_0()
                            .truncate()
                            .text_sm()
                            .font_weight(gpui_kit::FontWeight::SEMIBOLD)
                            .child(heading),
                    ),
            )
            .children(lines.into_iter().map(|line| {
                let heading = line.trim_start().starts_with('#');
                let text = line.trim_start().trim_start_matches('#').trim().to_string();
                div()
                    .text_xs()
                    .when(heading, |el| {
                        el.mt_1().font_weight(gpui_kit::FontWeight::SEMIBOLD)
                    })
                    .when(!heading, |el| el.text_color(theme.muted_foreground))
                    .child(SharedString::from(if heading { text } else { line }))
            }))
            .into_any_element();
        self.home_card(path, glyph, title, View::Notes, None, Vec::new(), body, cx)
    }

    /// The branch: its name and base, how far from its remote — with pull
    /// and push —, the commits it adds and the way to merge them; on the
    /// worktree on show, its pull request and its last CI run.
    fn home_branch(&mut self, path: &Path, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme().clone();
        let muted = theme.muted_foreground;
        let worktree = self.repos.worktree(path);
        let is_main = worktree.is_some_and(|worktree| worktree.is_main);
        let branch = worktree
            .and_then(|worktree| worktree.branch.clone())
            .map(SharedString::from)
            .unwrap_or_else(|| tr!("overview-detached"));
        let outline = self.outlines.get(path).cloned();
        // The base the review compares against, which the hand may have
        // chosen; the outline's own guess otherwise.
        let base = self
            .review
            .get(path)
            .and_then(|state| state.base.clone())
            .or_else(|| outline.as_ref().and_then(|outline| outline.base.clone()));
        let (ahead, behind) = outline
            .as_ref()
            .and_then(|outline| outline.upstream)
            .unwrap_or((0, 0));
        let sync = self.sync_buttons(path, ahead, behind, gpui_kit::component::Size::XSmall, cx);
        // The count is against the outline's base: said only when it is the
        // one shown, a count against another branch being a wrong count.
        let commits_title = outline.as_ref().and_then(|outline| {
            let counted = outline.base.clone()?;
            (outline.ahead_of_base > 0 && Some(&counted) == base.as_ref())
                .then(|| tr!("overview-ahead", { count: outline.ahead_of_base, base: counted }))
        });
        let merge = outline
            .as_ref()
            .filter(|outline| !is_main && outline.ahead_of_base > 0)
            .and_then(|outline| outline.base.clone())
            .map(|merge_base| {
                let merge = path.to_path_buf();
                Button::new("focus-home-merge")
                    .ghost()
                    .xsmall()
                    .icon(icon("git-merge"))
                    .label(tr!("overview-merge", { base: merge_base }))
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.confirm_merge(&merge, window, cx);
                    }))
            });
        let now = chrono::Utc::now().timestamp();
        let commits = outline
            .map(|outline| {
                outline
                    .commits
                    .into_iter()
                    .take(HOME_COMMITS)
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        let github = (self.active.as_deref() == Some(path)).then(|| self.home_github(cx));
        let body = v_flex()
            .gap_1p5()
            .child(
                h_flex()
                    .gap_1p5()
                    .items_center()
                    .text_sm()
                    .child(
                        div()
                            .min_w_0()
                            .truncate()
                            .font_weight(gpui_kit::FontWeight::SEMIBOLD)
                            .child(branch),
                    )
                    .children(base.map(|base| {
                        div()
                            .flex_none()
                            .text_xs()
                            .text_color(muted)
                            .child(SharedString::from(format!("← {base}")))
                    }))
                    .child(div().flex_1())
                    .children(sync),
            )
            .children(commits_title.map(|title| div().text_xs().text_color(muted).child(title)))
            .children(commits.into_iter().map(|commit| {
                h_flex()
                    .gap_1p5()
                    .text_xs()
                    .child(
                        div()
                            .flex_none()
                            .font_family(theme.mono_font_family.clone())
                            .text_color(muted)
                            .child(SharedString::from(commit.short)),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .child(SharedString::from(commit.subject)),
                    )
                    .child(
                        div()
                            .flex_none()
                            .text_color(muted)
                            .child(super::overview_view::ago(now, commit.at)),
                    )
            }))
            .children(merge.map(|merge| h_flex().child(merge)))
            .children(github.flatten())
            .into_any_element();
        let (glyph, _) = view_name(View::Git);
        self.home_card(
            path,
            glyph,
            tr!("focus-home-branch"),
            View::Git,
            None,
            Vec::new(),
            body,
            cx,
        )
    }

    /// The branch's pull request and its last CI run, as the GitHub panel
    /// read them — for the worktree on show, the one it reads for.
    fn home_github(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let theme = cx.theme().clone();
        let muted = theme.muted_foreground;
        let pr = self.branch_pr().cloned();
        let run = self.github.runs.first().cloned();
        let loaded = !self.github.pr_loading && self.github.error.is_none();
        if pr.is_none() && run.is_none() && !loaded {
            return None;
        }
        let pr_line = match pr {
            Some(pr) => {
                let checks = pr.checks();
                let url = pr.url.clone();
                h_flex()
                    .id("focus-home-pr")
                    .gap_1p5()
                    .items_center()
                    .text_xs()
                    .cursor_pointer()
                    .on_click(move |_, _, cx| cx.open_url(&url))
                    .child(
                        icon("git-pull-request")
                            .xsmall()
                            .text_color(if pr.is_draft { muted } else { theme.success }),
                    )
                    .child(
                        div()
                            .flex_none()
                            .text_color(muted)
                            .child(SharedString::from(format!("#{}", pr.number))),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .child(SharedString::from(pr.title.clone())),
                    )
                    .children(
                        pr.review_note()
                            .map(|note| div().flex_none().text_color(muted).child(note)),
                    )
                    .when(checks.failed > 0, |el| {
                        el.child(
                            div()
                                .flex_none()
                                .text_color(theme.danger)
                                .child(SharedString::from(format!("✗{}", checks.failed))),
                        )
                    })
                    .when(checks.running > 0, |el| {
                        el.child(
                            div()
                                .flex_none()
                                .text_color(theme.warning)
                                .child(SharedString::from(format!("⟳{}", checks.running))),
                        )
                    })
                    .when(checks.passed > 0, |el| {
                        el.child(
                            div()
                                .flex_none()
                                .text_color(theme.success)
                                .child(SharedString::from(format!("✓{}", checks.passed))),
                        )
                    })
                    .into_any_element()
            }
            None => div()
                .text_xs()
                .text_color(muted)
                .child(tr!("focus-home-no-pr"))
                .into_any_element(),
        };
        let run_line = run.map(|run| {
            let tint = match run.stage() {
                crate::github::Stage::Passed => theme.success,
                crate::github::Stage::Failed => theme.danger,
                crate::github::Stage::Running | crate::github::Stage::Waiting => theme.warning,
                crate::github::Stage::Skipped => muted,
            };
            let url = run.url.clone();
            h_flex()
                .id("focus-home-run")
                .gap_1p5()
                .items_center()
                .text_xs()
                .cursor_pointer()
                .on_click(move |_, _, cx| cx.open_url(&url))
                .child(icon(run.glyph()).xsmall().text_color(tint))
                .child(
                    div()
                        .flex_none()
                        .text_color(muted)
                        .child(SharedString::from(run.workflow.clone())),
                )
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .truncate()
                        .child(SharedString::from(run.title.clone())),
                )
        });
        Some(
            v_flex()
                .gap_1()
                .pt_1p5()
                .border_t_1()
                .border_color(theme.border)
                .child(pr_line)
                .children(run_line)
                .into_any_element(),
        )
    }

    /// What waits for a commit: its files, the first few, and the way to
    /// the git tab where it is committed.
    fn home_to_commit(&mut self, path: &Path, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme().clone();
        let files: Vec<crate::git::FileStatus> = if self.has_changes(path) {
            self.review
                .get(path)
                .map(|state| state.status.files.clone())
                .unwrap_or_default()
        } else {
            Vec::new()
        };
        let body = if files.is_empty() {
            div()
                .text_xs()
                .text_color(theme.muted_foreground)
                .child(tr!("home-clean"))
                .into_any_element()
        } else {
            let more = files.len().saturating_sub(HOME_FILES);
            v_flex()
                .gap_0p5()
                .children(files.into_iter().take(HOME_FILES).map(|file| {
                    let tint = if file.is_untracked() {
                        crate::git::StatusCode::Untracked
                    } else if file.is_unstaged() {
                        file.worktree
                    } else {
                        file.index
                    };
                    h_flex()
                        .gap_1p5()
                        .items_center()
                        .text_xs()
                        .child(crate::ui::file_icons::file_icon(&file.path, cx))
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .truncate()
                                .text_color(super::theme::status_color(tint, cx))
                                .child(SharedString::from(file.path.display().to_string())),
                        )
                }))
                .when(more > 0, |el| {
                    el.child(
                        div()
                            .text_xs()
                            .text_color(theme.muted_foreground)
                            .child(tr!("focus-home-more", { count: more })),
                    )
                })
                .into_any_element()
        };
        let worktree = path.to_path_buf();
        let actions = vec![Button::new("focus-home-commit")
            .xsmall()
            .primary()
            .icon(icon("git-commit-horizontal"))
            .label(tr!("focus-attention-commit"))
            .disabled(!self.has_changes(path))
            .on_click(cx.listener(move |this, _, _, cx| {
                this.show_board_view(&worktree, View::Git, cx);
            }))
            .into_any_element()];
        let detail = self.view_detail(path, View::Git, cx);
        self.home_card(
            path,
            "git-commit-horizontal",
            tr!("focus-home-to-commit"),
            View::Git,
            detail,
            actions,
            body,
            cx,
        )
    }

    /// The review: its size, its heaviest files with a bar each, when it was
    /// last read and what remarks are still open.
    fn home_review(&mut self, path: &Path, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme().clone();
        let muted = theme.muted_foreground;
        let diff = super::theme::DiffColors::of(cx);
        let files: Option<Vec<(std::path::PathBuf, usize, usize)>> =
            self.review.get(path).and_then(|state| {
                let range = super::review::branch_panel_range(
                    state.base.as_deref(),
                    state.review_point.as_ref(),
                    state.since_review,
                )?;
                state.files.get(&range).map(|files| {
                    files
                        .iter()
                        .map(|file| (file.path.clone(), file.added, file.removed))
                        .collect()
                })
            });
        let point = self
            .review
            .get(path)
            .and_then(|state| state.review_point.as_ref().map(|point| point.at));
        let open = self.open_findings(path).len();
        let mut rows: Vec<AnyElement> = Vec::new();
        match &files {
            Some(files) if files.is_empty() => rows.push(
                div()
                    .text_xs()
                    .text_color(muted)
                    .child(tr!("review-clean"))
                    .into_any_element(),
            ),
            Some(files) => {
                let (added, removed) = files.iter().fold((0, 0), |(a, r), f| (a + f.1, r + f.2));
                rows.push(
                    h_flex()
                        .gap_1p5()
                        .text_xs()
                        .child(tr!("home-files", { count: files.len() }))
                        .child(div().text_color(diff.added_fg).child(format!("+{added}")))
                        .child(
                            div()
                                .text_color(diff.removed_fg)
                                .child(format!("−{removed}")),
                        )
                        .into_any_element(),
                );
                for (file, added, removed, share) in focus::heaviest(files, HOME_HEAVIEST) {
                    let name = file
                        .file_name()
                        .map(|n| n.to_string_lossy().into_owned())
                        .unwrap_or_default();
                    rows.push(
                        h_flex()
                            .gap_2()
                            .items_center()
                            .text_xs()
                            .child(
                                div()
                                    .flex_none()
                                    .w(px(64.))
                                    .h(px(6.))
                                    .rounded_full()
                                    .bg(theme.secondary)
                                    .child(
                                        div()
                                            .h_full()
                                            .rounded_full()
                                            .bg(theme.info.opacity(0.7))
                                            .w(px(64. * share.max(0.04))),
                                    ),
                            )
                            .child(
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .truncate()
                                    .child(SharedString::from(name)),
                            )
                            .child(
                                div()
                                    .flex_none()
                                    .text_color(diff.added_fg)
                                    .child(format!("+{added}")),
                            )
                            .child(
                                div()
                                    .flex_none()
                                    .text_color(diff.removed_fg)
                                    .child(format!("−{removed}")),
                            )
                            .into_any_element(),
                    );
                }
            }
            None if self.active.as_deref() != Some(path) => rows.push(
                div()
                    .text_xs()
                    .text_color(muted)
                    .child(tr!("focus-review-idle"))
                    .into_any_element(),
            ),
            None => rows.push(
                div()
                    .text_xs()
                    .text_color(muted)
                    .child(tr!("review-loading"))
                    .into_any_element(),
            ),
        }
        let mut foot: Vec<SharedString> = Vec::new();
        if let Some(at) = point {
            let when = super::overview_view::ago(chrono::Utc::now().timestamp(), at);
            foot.push(tr!("focus-home-reviewed", { when: when }));
        }
        if open > 0 {
            foot.push(tr!("focus-home-remarks", { count: open }));
        }
        if !foot.is_empty() {
            rows.push(
                div()
                    .text_xs()
                    .text_color(if open > 0 { theme.warning } else { muted })
                    .child(SharedString::from(foot.join(" · ")))
                    .into_any_element(),
            );
        }
        let detail = self.view_detail(path, View::Review, cx);
        let (glyph, title) = view_name(View::Review);
        let body = v_flex().gap_1().children(rows).into_any_element();
        self.home_card(
            path,
            glyph,
            title,
            View::Review,
            detail,
            Vec::new(),
            body,
            cx,
        )
    }

    /// What runs: the environment and the recipes running now, each with
    /// its ↻ and ■. `None` when nothing runs — what can be started is the
    /// board title's widget, and a card listing thirty recipes said less
    /// than its selector.
    fn home_run(&mut self, path: &Path, cx: &mut Context<Self>) -> Option<AnyElement> {
        let configs: Vec<_> = self
            .run_configs(path)
            .into_iter()
            .filter(|config| self.runs(path, config, cx))
            .collect();
        if configs.is_empty() {
            return None;
        }
        let theme = cx.theme().clone();
        let rows: Vec<AnyElement> = configs
            .into_iter()
            .enumerate()
            .map(|(index, config)| {
                let running = self.runs(path, &config, cx);
                let name = Self::config_name(&config);
                let is_env = matches!(config, super::run::RunConfig::Env);
                let (start_path, start_config) = (path.to_path_buf(), config.clone());
                let (stop_path, stop_config) = (path.to_path_buf(), config);
                h_flex()
                    .gap_1p5()
                    .items_center()
                    .child(
                        div()
                            .flex_none()
                            .size(px(7.))
                            .rounded_full()
                            .bg(if running { theme.success } else { theme.border }),
                    )
                    .child(
                        icon(if is_env { "zap" } else { "play" })
                            .xsmall()
                            .text_color(theme.muted_foreground),
                    )
                    .child(div().flex_1().min_w_0().truncate().text_sm().child(name))
                    .child(
                        Button::new(("focus-home-run-start", index))
                            .ghost()
                            .xsmall()
                            .icon(icon(if running && !is_env {
                                "refresh-cw"
                            } else {
                                "play"
                            }))
                            .disabled(running && is_env)
                            .on_click(cx.listener(move |this, _, window, cx| {
                                this.start_config(&start_path, &start_config, window, cx);
                            })),
                    )
                    .when(running, |el| {
                        el.child(
                            Button::new(("focus-home-run-stop", index))
                                .ghost()
                                .xsmall()
                                .icon(icon("circle-stop"))
                                .on_click(cx.listener(move |this, _, window, cx| {
                                    this.stop_config(&stop_path, &stop_config, window, cx);
                                })),
                        )
                    })
                    .into_any_element()
            })
            .collect();
        let body = v_flex().gap_1().children(rows).into_any_element();
        Some(self.home_card(
            path,
            "play",
            tr!("focus-home-run"),
            View::Terminals,
            None,
            Vec::new(),
            body,
            cx,
        ))
    }
}
