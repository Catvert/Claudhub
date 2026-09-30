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
use crate::ui::focus::{self, GitFace, View};
use crate::ui::focus_view::{tab_name, view_name};
use crate::ui::icons::icon;
use crate::ui::overview::{self, Doing, Node};
use crate::ui::overview_view::heard;

/// The least width of the home: its two columns side by side, and the
/// gap between them.
pub(super) const HOME_LEAST: f32 = HOME_LEFT_MIN + 12. + HOME_RIGHT_MIN;
const HOME_LEFT_MIN: f32 = 300.;
const HOME_RIGHT_MIN: f32 = 360.;
/// Where the divider between the cards and the terminals starts.
const HOME_START: f32 = 460.;
/// The open tasks the home lists; the tab has the rest.
const HOME_TASKS: usize = 12;
/// The lines of the principal note the home shows.
const HOME_NOTE_LINES: usize = 14;
/// The commits the branch card lists.
const HOME_COMMITS: usize = 5;
/// The files waiting for a commit the home lists.
const HOME_FILES: usize = 8;
/// The height the review's list of files scrolls within.
const HOME_REVIEW_HEIGHT: f32 = 240.;

/// A tab of a board — a view, or a terminal under the home's —: lit, it
/// stands on the card's ground in its border; unlit, muted, lit under the
/// pointer.
fn tab_pill(
    tab: gpui_kit::Stateful<gpui_kit::Div>,
    lit: bool,
    theme: &gpui_kit::component::Theme,
) -> gpui_kit::Stateful<gpui_kit::Div> {
    tab.gap_1p5()
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
}

/// The review card's tree, for the range and the folds it was built under.
/// Built once per file list — `ReviewState::rows_changed` drops it — and per
/// fold: every board's card copied the list out and sorted it into a tree at
/// every frame, thirty a second while an agent works.
pub(crate) struct ReviewCard {
    range: crate::git::DiffRange,
    toggled: std::collections::HashSet<std::path::PathBuf>,
    rows: std::rc::Rc<Vec<focus::ReviewRow>>,
    files: usize,
    added: usize,
    removed: usize,
}

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
                .filter_map(|id| at_work.terminals.get(id).copied())
                .chain(self.chats_of(path).map(|chat| chat.doing)),
        );
        let tabs: Vec<AnyElement> = View::ALL
            .into_iter()
            .map(|tab| {
                let (glyph, title) = tab_name(tab);
                let lit = tab == view;
                let count = match tab {
                    View::Git => self
                        .summaries
                        .get(path)
                        .filter(|summary| !summary.is_empty())
                        .map(|summary| summary.files),
                    // The notes tab counts its to-do list's open tasks.
                    View::Notes | View::Todo => self
                        .review
                        .get(path)
                        .and_then(|state| state.todo.as_ref())
                        .map(|todo| todo.tasks.len() - todo.done())
                        .filter(|open| *open > 0),
                    View::Terminals => {
                        Some(self.board_terminals(path).len() + self.chats_of(path).count())
                            .filter(|n| *n > 0)
                    }
                    // The tests in red, as the last run left them.
                    View::Tests => self
                        .pest
                        .get(path)
                        .map(|state| {
                            state
                                .statuses
                                .iter()
                                .filter(|status| **status == Some(crate::suite::Status::Failed))
                                .count()
                        })
                        .filter(|failed| *failed > 0),
                    // The review and the PR are faces of the git tab.
                    View::Home | View::Review | View::Pr => None,
                };
                // The agents' tab wears the loudest of them.
                let signal = (tab == View::Terminals)
                    .then(|| super::theme::doing_color(loudest, &theme))
                    .flatten();
                let board = path.to_path_buf();
                tab_pill(
                    h_flex()
                        .id(SharedString::from(format!("focus-tab-{tab:?}")))
                        .flex_none()
                        .h(super::theme::bar_height(cx))
                        .px_3(),
                    lit,
                    &theme,
                )
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
                .children(signal.map(|tint| div().flex_none().size(px(6.)).rounded_full().bg(tint)))
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
            // A narrow board scrolls its tabs rather than push the board
            // wider than its view.
            .child(
                h_flex()
                    .id("focus-tabs")
                    .flex_shrink(1.)
                    .min_w_0()
                    .gap_1()
                    .overflow_x_scroll()
                    .children(tabs),
            )
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
            .size_full()
            .gap_3()
            .overflow_y_scroll()
            .child(self.home_branch(path, cx))
            .child(self.home_pr(path, cx))
            .child(self.home_to_commit(path, cx))
            .child(self.home_review(path, cx))
            .child(self.home_tasks(path, cx))
            .child(self.home_note(path, cx))
            .children(self.home_run(path, cx))
            .into_any_element();
        let right = self.home_terminals(path, at_work, window, cx);
        // The divider between the cards and the terminals is the hand's.
        let sides = self.two_sides(path, "home", HOME_START, left, right, cx);
        div()
            .id(SharedString::from(format!("focus-home-{}", path.display())))
            .size_full()
            .child(sides)
            .into_any_element()
    }

    /// A file pressed on a card: the git tab on `face` — the changes or the
    /// review —, the diff on that file. The worktree becomes the one on
    /// show: the editor's panels those faces are speak only of it.
    fn open_from_home(
        &mut self,
        path: &Path,
        file: std::path::PathBuf,
        face: GitFace,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.active.as_deref() != Some(path) {
            self.select_worktree(path.to_path_buf(), window, cx);
        }
        let range = match face {
            GitFace::Changes => Some(crate::git::DiffRange::Working),
            GitFace::Review => self.review.get(path).and_then(|state| {
                super::review::branch_panel_range(
                    state.base.as_deref(),
                    state.review_point.as_ref(),
                    state.since_review,
                )
            }),
            GitFace::History | GitFace::Pr => None,
        };
        if let Some(range) = range {
            self.open_file(path.to_path_buf(), file, range, cx);
        }
        self.show_git_face(path, face, cx);
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
                                // A card of the git tab opens it on what waits for a commit,
                                // whatever face it was left on.
                                if tab == View::Git {
                                    this.show_git_face(&board, GitFace::Changes, cx);
                                } else {
                                    this.show_board_view(&board, tab, cx);
                                }
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
        if let Some(view) = self.terminal(id).map(|terminal| terminal.view.clone()) {
            super::dialogs::focus_field(&view, window, cx);
        } else if let Some(view) = self.chat_by_id(id).map(|chat| chat.view.clone()) {
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
            .terminals_of(path)
            .map(|terminal| (terminal.view.entity_id().as_u64(), terminal.session.clone()))
            .collect();
        let waiting: Vec<(u64, bool)> = terminals
            .iter()
            .map(|(id, _)| (*id, at_work.terminals.get(id) == Some(&Doing::Waiting)))
            // The chats are sub-tabs too, after the shells: one waiting on a
            // permission comes forward like a terminal waiting on an answer.
            .chain(
                self.chats_of(path)
                    .map(|chat| (chat.id(), chat.doing == Doing::Waiting)),
            )
            .collect();
        let shown = focus::shown_terminal(self.home_terminal.get(path).copied(), &waiting);
        let mut tabs: Vec<AnyElement> = Vec::new();
        for (id, session) in &terminals {
            let (_, name) = self.card_name(&Node::Terminal(*id), cx);
            let doing = at_work.terminals.get(id).copied().unwrap_or(Doing::Rest);
            let activity = session
                .as_deref()
                .and_then(|session| self.agents.session(session));
            // A question first, from either, then work; what finished says
            // so once nothing else speaks.
            let loud = overview::loudest([doing, activity.map_or(Doing::Rest, heard)]);
            let tint = super::theme::doing_color(loud, &theme)
                .or_else(|| activity.and_then(|a| super::theme::activity_color(a, &theme)))
                .unwrap_or(theme.muted_foreground.opacity(0.5));
            let lit = shown == Some(*id);
            let (board, pressed) = (path.to_path_buf(), *id);
            tabs.push(
                tab_pill(
                    h_flex()
                        .id(("focus-home-terminal-tab", *id as usize))
                        .flex_none()
                        .max_w(px(220.))
                        .h(super::theme::bar_height(cx))
                        .px_2(),
                    lit,
                    &theme,
                )
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
                            this.ask_close_terminal(gpui_kit::EntityId::from(pressed), window, cx);
                        })),
                )
                .into_any_element(),
            );
        }
        let chats: Vec<(u64, gpui_kit::SharedString, Doing)> = self
            .chats_of(path)
            .map(|chat| (chat.id(), chat.label.clone(), chat.doing))
            .collect();
        for (id, label, doing) in chats {
            let tint = super::theme::doing_color(doing, &theme)
                .unwrap_or(theme.muted_foreground.opacity(0.5));
            let (board, pressed) = (path.to_path_buf(), id);
            tabs.push(
                tab_pill(
                    h_flex()
                        .id(("focus-home-chat-tab", id as usize))
                        .flex_none()
                        .max_w(px(220.))
                        .h(super::theme::bar_height(cx))
                        .px_2(),
                    shown == Some(id),
                    &theme,
                )
                .on_click(cx.listener(move |this, _, window, cx| {
                    this.reply_to(&board, pressed, window, cx);
                }))
                .child(div().flex_none().size(px(7.)).rounded_full().bg(tint))
                .child(super::icons::glyph("bot"))
                .child(div().min_w_0().truncate().child(label))
                .child(
                    Button::new(("focus-home-chat-close", id as usize))
                        .ghost()
                        .xsmall()
                        .icon(icon("x"))
                        .tooltip(tr!("overview-close"))
                        .on_click(cx.listener(move |this, _, window, cx| {
                            cx.stop_propagation();
                            this.close_chat(gpui_kit::EntityId::from(pressed), window, cx);
                        })),
                )
                .into_any_element(),
            );
        }
        let worktree = path.to_path_buf();
        let every = path.to_path_buf();
        let (app, hang) = (cx.entity().downgrade(), Hang::Worktree(path.to_path_buf()));
        let chat_button = self.home_chat_button(path, cx);
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
            .child(chat_button)
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
        let chat = shown.and_then(|id| self.chat_by_id(id));
        let body = match shown.and_then(|id| self.terminal(id)) {
            // A chat under its sub-tab, framed like a terminal.
            None if chat.is_some() => {
                let chat = chat.expect("matched just above");
                chat_frame(&chat.view, chat.doing, window, &theme, cx)
            }
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

    /// The button that opens a chat from a board's home, beside its `+`: a
    /// click with one agent, a menu with several — see
    /// `panels::new_chat_button`.
    fn home_chat_button(&self, path: &Path, cx: &mut Context<Self>) -> AnyElement {
        let agents = super::settings::Settings::global(cx).terminal.chat_agents();
        if agents.is_empty() {
            return div().into_any_element();
        }
        let button = Button::new("focus-home-chat")
            .ghost()
            .xsmall()
            .icon(icon("message-square-plus"))
            .tooltip(tr!("chat-new"));
        let app = cx.entity().downgrade();
        let worktree = path.to_path_buf();
        if let [agent] = agents.as_slice() {
            let agent = agent.clone();
            return button
                .on_click(move |_, window, cx| {
                    let (worktree, agent) = (worktree.clone(), agent.clone());
                    let _ = app.update(cx, |this, cx| {
                        this.open_home_chat(&worktree, agent, window, cx)
                    });
                })
                .into_any_element();
        }
        button
            .dropdown_menu(move |menu, _, _| {
                agents.iter().fold(menu, |menu, agent| {
                    let (app, worktree, agent) = (app.clone(), worktree.clone(), agent.clone());
                    menu.item(
                        gpui_kit::component::menu::PopupMenuItem::new(
                            gpui_kit::SharedString::from(agent.label().to_string()),
                        )
                        .icon(icon("bot"))
                        .on_click(move |_, window, cx| {
                            let (worktree, agent) = (worktree.clone(), agent.clone());
                            let _ = app.update(cx, |this, cx| {
                                this.open_home_chat(&worktree, agent, window, cx)
                            });
                        }),
                    )
                })
            })
            .into_any_element()
    }

    /// Opens a chat from the home and shows it under its sub-tab, the keys in
    /// it — the same path as `open_home_terminal`. Its tab in the editor is
    /// where the settings put terminals.
    pub(super) fn open_home_chat(
        &mut self,
        worktree: &Path,
        agent: crate::acp::Agent,
        window: &mut gpui_kit::Window,
        cx: &mut Context<Self>,
    ) {
        let placement = super::settings::Settings::global(cx).terminal.placement;
        self.open_chat(worktree, agent, placement, None, window, cx);
        let Some(open) = self.chats.last() else {
            return;
        };
        let (id, view) = (open.id(), open.view.clone());
        self.home_terminal.insert(worktree.to_path_buf(), id);
        if !matches!(self.board_view(worktree, cx), View::Home | View::Terminals) {
            self.show_board_view(worktree, View::Home, cx);
        }
        super::dialogs::focus_field(&view, window, cx);
        cx.notify();
    }

    /// Opens a shell from the home and shows it there, the keys in it: under
    /// its sub-tab — the board brought back to its home unless it shows its
    /// terminals already —, or on the plane, brought into view. The `+` of
    /// the sub-tabs and `Ctrl+Maj+T`: a terminal opened behind another tab
    /// was one to go looking for.
    pub(super) fn open_home_terminal(
        &mut self,
        worktree: &Path,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let before = self.terminals.len();
        self.open_terminal(worktree, super::terminal_view::Launch::shell(), window, cx);
        // A pty that would not open says so itself, and `last` would be an
        // older terminal.
        if self.terminals.len() == before {
            return;
        }
        let Some(view) = self.terminals.last().map(|terminal| terminal.view.clone()) else {
            return;
        };
        let id = view.entity_id().as_u64();
        self.home_terminal.insert(worktree.to_path_buf(), id);
        match self.home_mode {
            overview::HomeMode::Focus => {
                if !matches!(self.board_view(worktree, cx), View::Home | View::Terminals) {
                    self.show_board_view(worktree, View::Home, cx);
                }
            }
            overview::HomeMode::Canvas => self.overview_reveal = Some(Node::Terminal(id)),
        }
        super::dialogs::focus_field(&view, window, cx);
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

    /// The branch's pull request: its title, where it stands — checks,
    /// review, threads still open — and its last CI run; without one, the
    /// button that opens one. For the worktree on show, the one the GitHub
    /// state reads for.
    fn home_pr(&mut self, path: &Path, cx: &mut Context<Self>) -> AnyElement {
        let (glyph, title) = view_name(View::Pr);
        let on_show = self.active.as_deref() == Some(path);
        // Without a pull request, the button that opens one stands at the
        // head of the card, where every card keeps what acts on it.
        let can_open = on_show
            && self.branch_pr().is_none()
            && !self.github.pr_loading
            && self.github.error.is_none();
        let actions: Vec<AnyElement> = can_open
            .then(|| {
                let board = path.to_path_buf();
                Button::new("focus-home-create-pr")
                    .xsmall()
                    .ghost()
                    .icon(icon("git-pull-request"))
                    .label(tr!("focus-home-create-pr"))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.show_board_view(&board, View::Pr, cx);
                    }))
                    .into_any_element()
            })
            .into_iter()
            .collect();
        let body = if on_show {
            if let Some(number) = self.branch_pr().map(|pr| pr.number) {
                self.ensure_pr_threads(number);
            }
            self.home_github(path, cx)
        } else {
            div()
                .text_xs()
                .text_color(cx.theme().muted_foreground)
                .child(tr!("focus-review-idle"))
                .into_any_element()
        };
        self.home_card(path, glyph, title, View::Pr, None, actions, body, cx)
    }

    fn home_github(&self, path: &Path, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme().clone();
        let muted = theme.muted_foreground;
        let pr = self.branch_pr().cloned();
        let run = self.github.runs.first().cloned();
        if pr.is_none() {
            if self.github.pr_loading {
                return div()
                    .text_xs()
                    .text_color(muted)
                    .child(tr!("github-pr-loading"))
                    .into_any_element();
            }
            if let Some(error) = self.github.error.clone() {
                return div()
                    .text_xs()
                    .text_color(muted)
                    .child(error)
                    .into_any_element();
            }
        }
        let board = path.to_path_buf();
        let open_threads = pr
            .as_ref()
            .filter(|pr| self.github.view_threads_for == Some(pr.number))
            .map(|_| crate::github::unresolved(&self.github.view_threads).len())
            .unwrap_or(0);
        let pr_line = match pr {
            Some(pr) => {
                let checks = pr.checks();
                h_flex()
                    .id("focus-home-pr")
                    .gap_1p5()
                    .items_center()
                    .text_xs()
                    .cursor_pointer()
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.show_board_view(&board, View::Pr, cx);
                    }))
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
        let threads_line = (open_threads > 0).then(|| {
            div()
                .text_xs()
                .text_color(theme.warning)
                .child(tr!("pr-threads-open", { count: open_threads }))
        });
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
        v_flex()
            .gap_1()
            .child(pr_line)
            .children(threads_line)
            .children(run_line)
            .into_any_element()
    }

    /// What waits for a commit: its files, the first few, and the way to
    /// the git tab where it is committed.
    fn home_to_commit(&mut self, path: &Path, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme().clone();
        // Borrowed: the card shows a handful, and the status can list
        // thousands.
        let files: &[crate::git::FileStatus] = match self.review.get(path) {
            Some(state) if self.has_changes(path) => &state.status.files,
            _ => &[],
        };
        let body = if files.is_empty() {
            div()
                .text_xs()
                .text_color(theme.muted_foreground)
                .child(tr!("home-clean"))
                .into_any_element()
        } else {
            let more = files.len().saturating_sub(HOME_FILES);
            let rows: Vec<AnyElement> = files
                .iter()
                .take(HOME_FILES)
                .enumerate()
                .map(|(index, file)| {
                    let tint = if file.is_untracked() {
                        crate::git::StatusCode::Untracked
                    } else if file.is_unstaged() {
                        file.worktree
                    } else {
                        file.index
                    };
                    let (board, pressed) = (path.to_path_buf(), file.path.clone());
                    h_flex()
                        .id(("focus-home-change", index))
                        .gap_1p5()
                        .items_center()
                        .text_xs()
                        .rounded(theme.radius)
                        .cursor_pointer()
                        .hover(|style| style.bg(theme.list_hover))
                        .on_click(cx.listener(move |this, _, window, cx| {
                            this.open_from_home(
                                &board,
                                pressed.clone(),
                                GitFace::Changes,
                                window,
                                cx,
                            );
                        }))
                        .child(crate::ui::file_icons::file_icon(&file.path, cx))
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .truncate()
                                .text_color(super::theme::status_color(tint, cx))
                                .child(SharedString::from(file.path.display().to_string())),
                        )
                        .into_any_element()
                })
                .collect();
            v_flex()
                .gap_0p5()
                .children(rows)
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
            .label(tr!("focus-home-commit"))
            .disabled(!self.has_changes(path))
            .on_click(cx.listener(move |this, _, _, cx| {
                this.show_git_face(&worktree, GitFace::Changes, cx);
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

    /// The review: its size, its files by their place in the project, when
    /// it was last read and what remarks are still open.
    fn home_review(&mut self, path: &Path, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme().clone();
        let muted = theme.muted_foreground;
        let diff = super::theme::DiffColors::of(cx);
        let card = self.review_card(path);
        let point = self
            .review
            .get(path)
            .and_then(|state| state.review_point.as_ref().map(|point| point.at));
        let open = self.open_finding_count(path);
        let mut rows: Vec<AnyElement> = Vec::new();
        match card {
            Some((_, 0, _, _)) => rows.push(
                div()
                    .text_xs()
                    .text_color(muted)
                    .child(tr!("review-clean"))
                    .into_any_element(),
            ),
            Some((listed, files, added, removed)) => {
                rows.push(
                    h_flex()
                        .gap_1p5()
                        .text_xs()
                        .child(tr!("home-files", { count: files }))
                        .children(super::theme::volume(added, removed, &diff))
                        .into_any_element(),
                );
                // Every file, as a tree: what the card says of a branch is
                // where its work went. A press on a file opens its review, on
                // a folder folds it. **Virtual**: a branch can carry well over
                // a thousand files, and every one of them laid out on every
                // frame slowed the whole window.
                let count = listed.len();
                let row = super::theme::row_height(cx);
                let guide = super::theme::indent_guide(cx);
                let (entity, board) = (cx.entity(), path.to_path_buf());
                let (hover, radius) = (theme.list_hover, theme.radius);
                let counts =
                    move |added: usize, removed: usize| super::theme::volume(added, removed, &diff);
                rows.push(
                    gpui_kit::uniform_list(
                        "focus-home-review-files",
                        count,
                        move |range, _, cx| {
                            range
                                .map(|index| {
                                    let (app, board) = (entity.clone(), board.clone());
                                    let line = h_flex()
                                        .id(("focus-home-review-row", index))
                                        .h(row)
                                        .gap_1()
                                        .items_center()
                                        .text_xs()
                                        .rounded(radius)
                                        .cursor_pointer()
                                        .hover(|style| style.bg(hover));
                                    match &listed[index] {
                                        focus::ReviewRow::Dir {
                                            path,
                                            label,
                                            depth,
                                            collapsed,
                                            files,
                                            added,
                                            removed,
                                        } => {
                                            let folder = path.clone();
                                            line.on_click(move |_, _, cx| {
                                                app.update(cx, |this, cx| {
                                                    let toggled = this
                                                        .home_review_toggled
                                                        .entry(board.clone())
                                                        .or_default();
                                                    if !toggled.remove(&folder) {
                                                        toggled.insert(folder.clone());
                                                    }
                                                    cx.notify();
                                                });
                                            })
                                            .children(super::theme::indent_guides(*depth, guide))
                                            .child(
                                                icon(if *collapsed {
                                                    "chevron-right"
                                                } else {
                                                    "chevron-down"
                                                })
                                                .xsmall()
                                                .text_color(muted),
                                            )
                                            .child(
                                                super::icons::glyph(if *collapsed {
                                                    "folder"
                                                } else {
                                                    "folder-open"
                                                })
                                                .text_color(muted),
                                            )
                                            .child(
                                                div()
                                                    .flex_1()
                                                    .min_w_0()
                                                    .truncate()
                                                    .child(SharedString::from(label.clone())),
                                            )
                                            .child(
                                                div()
                                                    .flex_none()
                                                    .text_color(muted)
                                                    .child(SharedString::from(files.to_string())),
                                            )
                                            .children(counts(*added, *removed))
                                            .into_any_element()
                                        }
                                        focus::ReviewRow::File {
                                            path,
                                            depth,
                                            added,
                                            removed,
                                        } => {
                                            let name = path
                                                .file_name()
                                                .map(|n| n.to_string_lossy().into_owned())
                                                .unwrap_or_default();
                                            let pressed = path.clone();
                                            line.on_click(move |_, window, cx| {
                                                app.update(cx, |this, cx| {
                                                    this.open_from_home(
                                                        &board,
                                                        pressed.clone(),
                                                        GitFace::Review,
                                                        window,
                                                        cx,
                                                    );
                                                });
                                            })
                                            .children(super::theme::indent_guides(*depth, guide))
                                            .child(super::theme::chevron_space())
                                            .child(super::file_icons::file_icon(path, cx))
                                            .child(
                                                div()
                                                    .flex_1()
                                                    .min_w_0()
                                                    .truncate()
                                                    .child(SharedString::from(name)),
                                            )
                                            .children(counts(*added, *removed))
                                            .into_any_element()
                                        }
                                    }
                                })
                                .collect()
                        },
                    )
                    // A virtual list measures nothing: its height is said.
                    .h(px(HOME_REVIEW_HEIGHT).min(row * count as f32))
                    .into_any_element(),
                );
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

    /// The review card's tree and its totals — files, lines added and
    /// removed —, from its cache: see `ReviewCard`. `None` while the
    /// branch's list is not in hand.
    fn review_card(
        &mut self,
        path: &Path,
    ) -> Option<(std::rc::Rc<Vec<focus::ReviewRow>>, usize, usize, usize)> {
        let state = self.review.get_mut(path)?;
        let range = super::review::branch_panel_range(
            state.base.as_deref(),
            state.review_point.as_ref(),
            state.since_review,
        )?;
        let files = state.files.get(&range)?;
        let empty = std::collections::HashSet::new();
        let toggled = self.home_review_toggled.get(path).unwrap_or(&empty);
        let fresh = state
            .home_review
            .as_ref()
            .is_some_and(|card| card.range == range && card.toggled == *toggled);
        if !fresh {
            let listed: Vec<(std::path::PathBuf, usize, usize)> = files
                .iter()
                .map(|file| (file.path.clone(), file.added, file.removed))
                .collect();
            state.home_review = Some(ReviewCard {
                rows: std::rc::Rc::new(focus::review_rows(&listed, toggled)),
                files: listed.len(),
                added: listed.iter().map(|file| file.1).sum(),
                removed: listed.iter().map(|file| file.2).sum(),
                toggled: toggled.clone(),
                range,
            });
        }
        let card = state.home_review.as_ref()?;
        Some((card.rows.clone(), card.files, card.added, card.removed))
    }

    /// What runs: the environment and the recipes running now, each with
    /// its ↻ and ■. `None` when nothing runs — what can be started is the
    /// board title's widget, and a card listing thirty recipes said less
    /// than its selector.
    fn home_run(&mut self, path: &Path, cx: &mut Context<Self>) -> Option<AnyElement> {
        let configs: Vec<_> = self
            .run_configs(path)
            .into_iter()
            .filter(|config| self.runs(path, config))
            .collect();
        if configs.is_empty() {
            return None;
        }
        let theme = cx.theme().clone();
        let rows: Vec<AnyElement> = configs
            .into_iter()
            .enumerate()
            .map(|(index, config)| {
                let running = self.runs(path, &config);
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

/// A chat framed as a terminal is on the boards: rounded, bordered, and
/// outlined by what its agent is doing. **Cached**, for the terminals' reason:
/// the chat notifies at every chunk, and only it must redraw.
pub(super) fn chat_frame(
    view: &gpui_kit::Entity<super::chat_view::ChatView>,
    doing: Doing,
    window: &gpui_kit::Window,
    theme: &gpui_kit::component::Theme,
    cx: &gpui_kit::App,
) -> AnyElement {
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
                .child(
                    v_flex().flex_1().min_h_0().child(
                        view.clone()
                            .cached(gpui_kit::StyleRefinement::default().size_full()),
                    ),
                ),
        )
        .child(super::overview_view::tile_outline(
            doing, focused, 1., theme,
        ))
        .into_any_element()
}
