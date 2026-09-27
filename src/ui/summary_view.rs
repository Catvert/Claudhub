//! A board's home: the first column of a worktree's board, a digest of
//! everything it holds — its git, its review, its principal note, its
//! to-do list, its agents — one section each, in `focus::View::ALL`'s order.
//!
//! A section is read at a glance and pressed to go further: the press puts
//! its view on the right of the board, in the place of the one there, and
//! the section of the view on show is lit. What is done in one gesture is
//! done here — a task ticked —, the rest is the view's.
//!
//! It folds to a rail of one glyph per section, the agents' wearing the
//! signal of the loudest of them — the sidebar's edge, which says the same
//! thing of a worktree.

use std::path::Path;

use gpui_kit::component::{
    button::{Button, ButtonVariants as _},
    checkbox::Checkbox,
    h_flex,
    menu::DropdownMenu as _,
    v_flex, ActiveTheme, Selectable as _, Sizable as _,
};
use gpui_kit::{div, prelude::*, px, AnyElement, App, Context, SharedString};

use crate::tr;
use crate::ui::app::ClaudhubApp;
use crate::ui::canvas_view::Hang;
use crate::ui::focus::View;
use crate::ui::focus_view::{edge_signal, view_name};
use crate::ui::icons::icon;
use crate::ui::overview::{self, Doing, Node};

/// The home's width, and folded to its rail.
const HOME_WIDTH: f32 = 280.;
const HOME_RAIL: f32 = 44.;
/// The open tasks the home lists; the view has the rest.
const HOME_TASKS: usize = 5;
/// The lines of the principal note the home shows.
const HOME_NOTE_LINES: usize = 4;

impl ClaudhubApp {
    /// How wide a board's home is drawn.
    pub(super) fn summary_width(&self, path: &Path, cx: &App) -> f32 {
        if home_folded(path, cx) {
            HOME_RAIL
        } else {
            HOME_WIDTH
        }
    }

    fn toggle_home(&mut self, path: &Path, cx: &mut Context<Self>) {
        super::store::Store::update_global(cx, |store| {
            let state = store.worktrees.entry(path.to_path_buf()).or_default();
            state.focus_summary_folded = !state.focus_summary_folded;
        });
        cx.notify();
    }

    /// A board's home, or its rail; `view` the one on show.
    pub(super) fn render_board_summary(
        &mut self,
        path: &Path,
        view: View,
        at_work: &overview::AtWork,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        // Its to-do list and notes are read with the worktree's review
        // state, which a worktree nobody opened does not have yet.
        self.ensure_review(path, cx);
        if home_folded(path, cx) {
            return self.render_home_rail(path, view, at_work, cx);
        }
        let theme = cx.theme().clone();
        let sections: Vec<AnyElement> = View::ALL
            .into_iter()
            .map(|section| {
                let body = match section {
                    View::Git => self.home_git(path, cx),
                    View::Review => self.home_review(path, cx),
                    View::Notes => self.home_note(path, cx),
                    View::Todo => self.home_todo(path, cx),
                    View::Terminals => self.home_agents(path, at_work, cx),
                };
                self.render_home_section(path, section, section == view, body, cx)
            })
            .collect();
        let (app, hang) = (cx.entity().downgrade(), Hang::Worktree(path.to_path_buf()));
        let fold = {
            let path = path.to_path_buf();
            Button::new(SharedString::from(format!(
                "focus-home-fold-{}",
                path.display()
            )))
            .ghost()
            .xsmall()
            .icon(icon("panel-left-close"))
            .tooltip(tr!("focus-home-fold"))
            .on_click(cx.listener(move |this, _, _, cx| this.toggle_home(&path, cx)))
        };
        v_flex()
            .flex_none()
            .w(px(HOME_WIDTH))
            .h_full()
            .overflow_hidden()
            .rounded(theme.radius_lg)
            .border_1()
            .border_color(theme.border)
            .bg(theme.background)
            .child(
                h_flex()
                    .flex_none()
                    .h(super::theme::bar_height(cx))
                    .pl_3()
                    .pr_1()
                    .gap_1()
                    .items_center()
                    .border_b_1()
                    .border_color(theme.border)
                    .child(icon("house").xsmall().text_color(theme.muted_foreground))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .text_sm()
                            .font_weight(gpui_kit::FontWeight::SEMIBOLD)
                            .child(tr!("focus-home")),
                    )
                    .child(
                        Button::new(SharedString::from(format!(
                            "focus-home-add-{}",
                            path.display()
                        )))
                        .ghost()
                        .xsmall()
                        .icon(icon("plus"))
                        .tooltip(tr!("overview-add"))
                        .dropdown_menu(move |menu, _, cx| {
                            super::overview_view::add_items(&app, &hang, true, menu, cx)
                        }),
                    )
                    .child(fold),
            )
            .child(
                v_flex()
                    .id(SharedString::from(format!("focus-home-{}", path.display())))
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .children(sections),
            )
            .into_any_element()
    }

    /// A section: its title — lit while its view is on show — and what it
    /// says of the worktree. A press puts its view on the right.
    fn render_home_section(
        &self,
        path: &Path,
        section: View,
        lit: bool,
        body: AnyElement,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let theme = cx.theme().clone();
        let (glyph, title) = view_name(section);
        let board = path.to_path_buf();
        v_flex()
            .id(SharedString::from(format!("focus-home-{section:?}")))
            .relative()
            .w_full()
            .pl_3()
            .pr_2()
            .py_2()
            .gap_1()
            .cursor_pointer()
            .border_b_1()
            .border_color(theme.border.opacity(0.6))
            .when(lit, |el| el.bg(theme.list_active))
            .when(!lit, |el| el.hover(|style| style.bg(theme.list_hover)))
            .on_click(cx.listener(move |this, _, _, cx| {
                this.show_board_view(&board, section, cx);
            }))
            // The selection is a band with a bar at its edge, as the
            // sidebar's is.
            .when(lit, |el| {
                el.child(
                    div()
                        .absolute()
                        .left_0()
                        .top_0()
                        .bottom_0()
                        .w(px(2.))
                        .bg(theme.ring),
                )
            })
            .child(
                h_flex()
                    .gap_1p5()
                    .items_center()
                    .child(icon(glyph).xsmall().text_color(if lit {
                        theme.ring
                    } else {
                        theme.muted_foreground
                    }))
                    .child(
                        div()
                            .text_xs()
                            .font_weight(gpui_kit::FontWeight::SEMIBOLD)
                            .text_color(theme.muted_foreground)
                            .child(title),
                    ),
            )
            .child(body)
            .into_any_element()
    }

    /// Git: the branch and how far from its remote, the commits ahead of
    /// its base, what waits for a commit.
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

    /// The home folded: a glyph per section, lit for the view on show — a
    /// press puts its view there — and the unfold at the top.
    fn render_home_rail(
        &mut self,
        path: &Path,
        view: View,
        at_work: &overview::AtWork,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let theme = cx.theme().clone();
        let loudest = overview::loudest(
            self.board_terminals(path)
                .iter()
                .filter_map(|id| at_work.terminals.get(id).copied()),
        );
        let glyphs: Vec<AnyElement> = View::ALL
            .into_iter()
            .map(|section| {
                let (glyph, title) = view_name(section);
                let board = path.to_path_buf();
                div()
                    .relative()
                    .w_full()
                    .flex()
                    .justify_center()
                    .child(
                        Button::new(SharedString::from(format!("focus-rail-{section:?}")))
                            .ghost()
                            .small()
                            .icon(icon(glyph))
                            .selected(section == view)
                            .tooltip(title)
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.show_board_view(&board, section, cx);
                            })),
                    )
                    .when(section == View::Terminals, |el| {
                        el.children(edge_signal(Some(&loudest), 2., &theme))
                    })
                    .into_any_element()
            })
            .collect();
        let unfold = {
            let path = path.to_path_buf();
            Button::new(SharedString::from(format!(
                "focus-home-unfold-{}",
                path.display()
            )))
            .ghost()
            .xsmall()
            .icon(icon("panel-left-open"))
            .tooltip(tr!("focus-home-unfold"))
            .on_click(cx.listener(move |this, _, _, cx| this.toggle_home(&path, cx)))
        };
        v_flex()
            .flex_none()
            .w(px(HOME_RAIL))
            .h_full()
            .items_center()
            .gap_1()
            .overflow_hidden()
            .rounded(theme.radius_lg)
            .border_1()
            .border_color(theme.border)
            .bg(theme.background)
            .child(
                div()
                    .flex_none()
                    .w_full()
                    .h(super::theme::bar_height(cx))
                    .flex()
                    .items_center()
                    .justify_center()
                    .border_b_1()
                    .border_color(theme.border)
                    .child(unfold),
            )
            .children(glyphs)
            .into_any_element()
    }
}

fn home_folded(path: &Path, cx: &App) -> bool {
    super::store::Store::global(cx)
        .worktrees
        .get(path)
        .is_some_and(|state| state.focus_summary_folded)
}
