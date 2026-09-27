//! A board's tabs, and its home: the digest of a worktree in one screen —
//! its git, its review, its principal note, its to-do list, its agents —
//! a card each, in `focus::View::ALL`'s order.
//!
//! A card is read at a glance and pressed to go further: the press opens
//! its tab, the whole of it. What is done in one gesture is done on the
//! card — a task ticked —, the rest is the tab's.

use std::path::Path;

use gpui_kit::component::{
    button::{Button, ButtonVariants as _},
    checkbox::Checkbox,
    h_flex,
    menu::DropdownMenu as _,
    v_flex, ActiveTheme, Sizable as _,
};
use gpui_kit::{div, prelude::*, px, AnyElement, Context, SharedString};

use crate::tr;
use crate::ui::app::ClaudhubApp;
use crate::ui::canvas_view::Hang;
use crate::ui::focus::View;
use crate::ui::focus_view::view_name;
use crate::ui::icons::icon;
use crate::ui::overview::{self, Doing, Node};

/// The least width of a card of the home: under it, the cards wrap.
const CARD_BASIS: f32 = 340.;
/// The open tasks a card lists; the tab has the rest.
const HOME_TASKS: usize = 8;
/// The lines of the principal note a card shows.
const HOME_NOTE_LINES: usize = 8;
/// The commits the git card lists.
const HOME_COMMITS: usize = 5;

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

    /// The home: a card per section, side by side as the width allows.
    pub(super) fn render_home_view(
        &mut self,
        path: &Path,
        at_work: &overview::AtWork,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let cards: Vec<AnyElement> = View::ALL
            .into_iter()
            .filter(|section| *section != View::Home)
            .map(|section| {
                let body = match section {
                    View::Git => self.home_git(path, cx),
                    View::Review => self.home_review(path, cx),
                    View::Notes => self.home_note(path, cx),
                    View::Todo => self.home_todo(path, cx),
                    _ => self.home_agents(path, at_work, cx),
                };
                self.render_home_card(path, section, body, cx)
            })
            .collect();
        v_flex()
            .id(SharedString::from(format!("focus-home-{}", path.display())))
            .size_full()
            .overflow_y_scroll()
            .child(
                h_flex()
                    .w_full()
                    .flex_wrap()
                    .items_start()
                    .gap_3()
                    .children(cards),
            )
            .into_any_element()
    }

    /// A card of the home: its title, and what it says of the worktree. A
    /// press opens its tab.
    fn render_home_card(
        &self,
        path: &Path,
        section: View,
        body: AnyElement,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let theme = cx.theme().clone();
        let (glyph, title) = view_name(section);
        let board = path.to_path_buf();
        v_flex()
            .id(SharedString::from(format!("focus-home-{section:?}")))
            .flex_grow(1.)
            .flex_basis(px(CARD_BASIS))
            .p_3()
            .gap_2()
            .rounded(theme.radius_lg)
            .border_1()
            .border_color(theme.border)
            .bg(theme.background)
            .cursor_pointer()
            .hover(|style| style.border_color(theme.ring.opacity(0.6)))
            .on_click(cx.listener(move |this, _, _, cx| {
                this.show_board_view(&board, section, cx);
            }))
            .child(
                h_flex()
                    .gap_1p5()
                    .items_center()
                    .child(icon(glyph).xsmall().text_color(theme.muted_foreground))
                    .child(
                        div()
                            .flex_1()
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
            .child(body)
            .into_any_element()
    }

    /// Git: the branch and how far from its remote, the commits ahead of
    /// its base, what waits for a commit, the last commits.
    fn home_git(&self, path: &Path, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme().clone();
        let branch = self
            .repos
            .worktree(path)
            .and_then(|worktree| worktree.branch.clone())
            .map(SharedString::from)
            .unwrap_or_else(|| tr!("overview-detached"));
        let outline = self.outlines.get(path);
        let upstream = outline
            .and_then(|outline| outline.upstream)
            .and_then(|(ahead, behind)| {
                let mut parts = Vec::new();
                if ahead > 0 {
                    parts.push(format!("↑{ahead}"));
                }
                if behind > 0 {
                    parts.push(format!("↓{behind}"));
                }
                (!parts.is_empty()).then(|| SharedString::from(parts.join(" ")))
            });
        let commits: Vec<_> = outline
            .map(|outline| outline.commits.iter().take(HOME_COMMITS).cloned().collect())
            .unwrap_or_default();
        let ahead = outline.and_then(|outline| {
            let base = outline.base.clone()?;
            (outline.ahead_of_base > 0)
                .then(|| tr!("overview-ahead", { count: outline.ahead_of_base, base: base }))
        });
        v_flex()
            .gap_0p5()
            .text_sm()
            .child(
                h_flex()
                    .gap_1()
                    .child(div().min_w_0().truncate().child(branch))
                    .children(upstream.map(|text| {
                        div()
                            .flex_none()
                            .text_xs()
                            .text_color(theme.muted_foreground)
                            .child(text)
                    })),
            )
            .children(ahead.map(|text| {
                div()
                    .text_xs()
                    .text_color(theme.muted_foreground)
                    .truncate()
                    .child(text)
            }))
            .child(
                h_flex()
                    .text_xs()
                    .text_color(theme.muted_foreground)
                    .children(self.view_detail(path, View::Git, cx)),
            )
            .children(commits.into_iter().map(|commit| {
                h_flex()
                    .gap_1p5()
                    .text_xs()
                    .child(
                        div()
                            .flex_none()
                            .font_family(theme.mono_font_family.clone())
                            .text_color(theme.muted_foreground)
                            .child(SharedString::from(commit.short)),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .child(SharedString::from(commit.subject)),
                    )
            }))
            .into_any_element()
    }

    /// The review: what it compares against, the files and lines when the
    /// list has been read, the remarks still open.
    fn home_review(&self, path: &Path, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme().clone();
        let diff = super::theme::DiffColors::of(cx);
        let files = self.review.get(path).and_then(|state| {
            let range = super::review::branch_panel_range(
                state.base.as_deref(),
                state.review_point.as_ref(),
                state.since_review,
            )?;
            state.files.get(&range).map(|files| {
                (
                    files.len(),
                    files.iter().map(|file| file.added).sum::<usize>(),
                    files.iter().map(|file| file.removed).sum::<usize>(),
                )
            })
        });
        let open = self.open_findings(path).len();
        v_flex()
            .gap_0p5()
            .text_xs()
            .text_color(theme.muted_foreground)
            .children(self.view_detail(path, View::Review, cx))
            .children(files.map(|(count, added, removed)| {
                h_flex()
                    .gap_1()
                    .child(tr!("home-files", { count: count }))
                    .child(div().text_color(diff.added_fg).child(format!("+{added}")))
                    .child(
                        div()
                            .text_color(diff.removed_fg)
                            .child(format!("−{removed}")),
                    )
            }))
            .when(open > 0, |el| {
                el.child(
                    div()
                        .text_color(theme.warning)
                        .child(tr!("focus-home-remarks", { count: open })),
                )
            })
            .into_any_element()
    }

    /// The principal note: its title, and its first lines.
    fn home_note(&self, path: &Path, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme().clone();
        let Some(note) = self.principal_note(path, cx) else {
            return div()
                .text_xs()
                .text_color(theme.muted_foreground)
                .child(tr!("focus-notes-none"))
                .into_any_element();
        };
        let pinned = super::store::Store::global(cx)
            .worktrees
            .get(path)
            .and_then(|state| state.pinned_note.as_deref())
            == Some(note.as_path());
        let (_, title) = self.card_name(&Node::Note(note.clone()), cx);
        let lines: Vec<String> = self
            .canvas_entry(&note)
            .map(|(_, entry)| {
                entry
                    .node
                    .body
                    .lines()
                    .map(str::trim)
                    .filter(|line| !line.is_empty() && !line.starts_with('#'))
                    .take(HOME_NOTE_LINES)
                    .map(str::to_string)
                    .collect()
            })
            .unwrap_or_default();
        v_flex()
            .gap_0p5()
            .child(
                h_flex()
                    .gap_1()
                    .items_center()
                    .when(pinned, |el| {
                        el.child(icon("pin").xsmall().text_color(theme.ring))
                    })
                    .child(div().min_w_0().truncate().text_sm().child(title)),
            )
            .children(lines.into_iter().map(|line| {
                div()
                    .text_xs()
                    .text_color(theme.muted_foreground)
                    .truncate()
                    .child(SharedString::from(line))
            }))
            .into_any_element()
    }

    /// The to-do list: how far it has come, and its first open tasks, ticked
    /// here.
    fn home_todo(&self, path: &Path, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme().clone();
        let Some(todo) = self.review.get(path).and_then(|state| state.todo.clone()) else {
            return div()
                .text_xs()
                .text_color(theme.muted_foreground)
                .child(tr!("todo-none"))
                .into_any_element();
        };
        let open: Vec<_> = todo
            .tasks
            .iter()
            .filter(|task| !task.done)
            .take(HOME_TASKS)
            .cloned()
            .collect();
        let detail = self.view_detail(path, View::Todo, cx);
        let rows: Vec<_> = open
            .into_iter()
            .map(|task| {
                let (worktree, line) = (path.to_path_buf(), task.line);
                h_flex()
                    .gap_1p5()
                    .items_center()
                    .child(
                        div()
                            // The tick is its own gesture, not the section's.
                            .id(("focus-home-task", line))
                            .on_click(|_, _, cx| cx.stop_propagation())
                            .child(
                                Checkbox::new(("focus-home-tick", line))
                                    .checked(false)
                                    .on_click(cx.listener(move |this, _, _, cx| {
                                        cx.stop_propagation();
                                        this.toggle_task_in(worktree.clone(), line, true, cx);
                                    })),
                            ),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .text_xs()
                            .child(SharedString::from(task.label)),
                    )
            })
            .collect();
        v_flex()
            .gap_0p5()
            .child(
                div()
                    .text_xs()
                    .text_color(theme.muted_foreground)
                    .children(detail),
            )
            .children(rows)
            .into_any_element()
    }

    /// The agents: a line per terminal, and what its agent is doing.
    fn home_agents(
        &self,
        path: &Path,
        at_work: &overview::AtWork,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let theme = cx.theme().clone();
        let ids = self.board_terminals(path);
        if ids.is_empty() {
            return div()
                .text_xs()
                .text_color(theme.muted_foreground)
                .child(tr!("focus-terminals-none"))
                .into_any_element();
        }
        v_flex()
            .gap_0p5()
            .children(ids.into_iter().map(|id| {
                let (_, name) = self.card_name(&Node::Terminal(id), cx);
                let doing = at_work.terminals.get(&id).copied().unwrap_or(Doing::Rest);
                let (word, tint) = match doing {
                    Doing::Working => (Some(tr!("focus-agent-working")), theme.warning),
                    Doing::Waiting => (Some(tr!("focus-agent-waiting")), theme.danger),
                    Doing::Rest => (None, theme.muted_foreground),
                };
                h_flex()
                    .gap_1p5()
                    .items_center()
                    .child(div().flex_none().size(px(6.)).rounded_full().bg(tint))
                    .child(div().flex_1().min_w_0().truncate().text_xs().child(name))
                    .children(
                        word.map(|word| div().flex_none().text_xs().text_color(tint).child(word)),
                    )
            }))
            .into_any_element()
    }
}
