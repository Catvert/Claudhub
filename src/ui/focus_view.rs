//! The home screen's focus view, its default: a sidebar of every worktree on
//! show, and the ones chosen in it in the middle, side by side, each one on
//! its board.
//!
//! The plane answers « where does each worktree stand », and pays for it in
//! room: five worktrees share one screen. Most of a day is spent in one
//! worktree with a glance at the others, which is what an editor's layout
//! is — a list to choose from on the left, what was chosen filling the rest.
//! The sidebar is that list, and it says of each worktree what one glances
//! at: whether an agent works in it, how much it has in progress, how many
//! terminals it has open. A click shows a worktree alone, `Ctrl`+click adds
//! it beside the others or takes it away (`focus::shown_worktrees`) — which
//! is what the columns view was, every worktree at once, and why it went:
//! two views of the same boards differed only by how many were chosen. The
//! sidebar folds to a rail of initials, for the room.
//!
//! **A board is a home and one view** (`ui::focus`, pure): on the left a
//! digest of the worktree — its git, its review, its principal note, its
//! to-do list, its agents (`ui::summary_view`) — and on the right the whole
//! of one of them, the one its section was pressed for. One at a time:
//! views of different kinds side by side were a board to arrange before
//! they were one to read.
//!
//! **Which worktree is the one on show** — `active`, the window's: choosing
//! one here is choosing it for the editor too, and the branch picker, the
//! review and the title bar all speak of it already. The others shown beside
//! it are only shown.
//!
//! **What a board holds in the moment is its own** — where its home and
//! its view stand — keyed by worktree.

use std::path::{Path, PathBuf};

use gpui_kit::component::{
    button::{Button, ButtonVariants as _},
    h_flex,
    menu::{ContextMenuExt as _, PopupMenu},
    v_flex, ActiveTheme, Selectable as _, Sizable as _,
};
use gpui_kit::{
    canvas, div, point, prelude::*, px, AnyElement, App, Context, Pixels, SharedString, Window,
};

use crate::tr;
use crate::ui::app::ClaudhubApp;
use crate::ui::focus::{self, View};
use crate::ui::icons::icon;
use crate::ui::motion::Axes;
use crate::ui::overview::{self, Doing, HomeMode, Node};
use crate::ui::overview_view::live_worktrees;

/// The boards' row's bar, and the key of its arrows' slide.
const BOARD_BAR: &str = "focus-board-bar";

/// The sidebar's width.
const SIDEBAR_WIDTH: f32 = 260.;
/// What each worktree's agents say, the ones at rest left out.
pub(super) type Doings = std::collections::HashMap<PathBuf, Doing>;
/// The sidebar folded to its rail: a column of initials.
const RAIL_WIDTH: f32 = 52.;
/// How many of the commits a branch adds its git view lists.
const GIT_COMMITS: usize = 5;
/// The git view's left column — the branch and the Changes panel —, the
/// diff taking the rest.
const GIT_LIST_WIDTH: f32 = 380.;
/// The tests view's tree, left of the run it follows.
const TESTS_LIST_WIDTH: f32 = 420.;
/// The notes view's list, left of the note it shows.
const NOTES_LIST_WIDTH: f32 = 240.;

impl ClaudhubApp {
    /// The focus view: the sidebar, and the worktrees on show.
    pub(super) fn render_overview_focus(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let shown = self.focus_worktrees();
        let doings = self.sidebar_doings();
        let at_work = self.prepare_laid_out(&shown, &doings, cx);
        let gap = window.rem_size() * 0.75;
        let sidebar = self.render_focus_sidebar(&shown, &doings, cx);
        let main: AnyElement = match self.overview_zoomed.clone() {
            // A maximised node fills the middle, the sidebar staying: it is
            // how one goes elsewhere.
            Some(node) => v_flex()
                .size_full()
                .child(self.column_node(&node, false, &at_work, window, cx))
                .into_any_element(),
            None if shown.is_empty() => v_flex()
                .size_full()
                .items_center()
                .justify_center()
                .text_sm()
                .text_color(cx.theme().muted_foreground)
                .child(tr!("overview-empty"))
                .into_any_element(),
            None => {
                // Their lists of changes, even if the pickers leave them out
                // of what the home screen reads.
                self.ensure_changes_read(&shown, cx);
                // A wheel's slide goes on where the last frame left it.
                let scroll = self.focus_scroll.clone();
                self.motion(BOARD_BAR.into(), Axes::Both)
                    .advance(&scroll, window);
                let mut boards: Vec<AnyElement> = Vec::new();
                for (index, path) in shown.iter().enumerate() {
                    // Between two boards, a line: side by side, one's last
                    // column reads as the next one's first otherwise.
                    if index > 0 {
                        boards.push(
                            div()
                                .flex_none()
                                .w(px(1.))
                                .h_full()
                                .bg(cx.theme().border)
                                .into_any_element(),
                        );
                    }
                    boards.push(self.render_focus_board(path, gap, &at_work, window, cx));
                }
                // The row scrolls sideways, never a board alone: each takes
                // its share of the width, and never less than its columns'.
                let row = h_flex()
                    .id("focus-boards")
                    .track_scroll(&self.focus_scroll)
                    .size_full()
                    .gap(gap)
                    // The wheel scrolls what it is over, up and down: a row
                    // that scrolls only sideways would otherwise turn every
                    // notch that misses a column into a slide of the whole
                    // row. Sideways is the bar's, or a sideways wheel's.
                    .restrict_scroll_to_axis()
                    .overflow_x_scroll()
                    .children(boards);
                // The window's buttons are drawn over the top-right corner:
                // where they are, the row starts under them.
                let corner = Self::draws_window_buttons(window)
                    .then(|| div().flex_none().h(super::theme::toolbar_height(cx)));
                v_flex()
                    .size_full()
                    .children(corner)
                    .child(div().flex_1().min_h_0().child(super::scroll::both(
                        BOARD_BAR,
                        &self.focus_scroll,
                        row,
                    )))
                    .into_any_element()
            }
        };
        let buttons = self.render_window_buttons(window, cx);
        h_flex()
            .id("overview")
            .relative()
            .flex_1()
            .min_h_0()
            .min_w_0()
            .overflow_hidden()
            .bg(super::theme::gutter(cx))
            .p_3()
            .gap(gap)
            // A node's height is dragged from its bottom edge: followed from
            // here — the pointer leaves what it pressed at once.
            .on_mouse_move(
                cx.listener(|this, event: &gpui_kit::MouseMoveEvent, _, cx| {
                    this.overview_dragged(event, cx);
                }),
            )
            .capture_any_mouse_up(cx.listener(|this, _, _, cx| {
                this.end_overview_drag(cx);
            }))
            .child(sidebar)
            .child(div().flex_1().min_w_0().h_full().child(main))
            .children(buttons)
            .into_any_element()
    }

    /// What every worktree's Claudes say, loudest first: the sidebar
    /// dresses each row as the boards dress a terminal.
    pub(super) fn sidebar_doings(&self) -> Doings {
        let among: Vec<PathBuf> = self.repos.iter().flat_map(live_worktrees).collect();
        among
            .iter()
            .map(|path| (path.clone(), self.worktree_doing(path, &among)))
            .filter(|(_, doing)| *doing != Doing::Rest)
            .collect()
    }

    /// The worktrees the middle shows, side by side — see
    /// `focus::shown_worktrees`, among every project's. The window's own is
    /// the one on show, else the first of all.
    pub(super) fn focus_worktrees(&self) -> Vec<PathBuf> {
        let on_show: Vec<PathBuf> = self.repos.iter().flat_map(live_worktrees).collect();
        let primary = self.active.clone().or_else(|| on_show.first().cloned());
        focus::shown_worktrees(&self.focus_chosen, primary.as_deref(), &on_show)
    }

    /// Keeps what the sidebar chose, for the next session too.
    fn choose_focus(&mut self, chosen: Vec<PathBuf>, cx: &mut Context<Self>) {
        self.focus_chosen = chosen.clone();
        super::store::Store::update_global(cx, |store| store.session.focus_shown = chosen);
    }

    /// A worktree of the sidebar clicked: shown alone; with `Ctrl`, added
    /// beside the others or taken away; twice, gone to work in.
    fn focus_row_clicked(
        &mut self,
        path: &Path,
        event: &gpui_kit::ClickEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let beside = event.modifiers().secondary();
        if event.click_count() >= 2 {
            // The second press of a `Ctrl` double click has nothing more to
            // say than the first did.
            if !beside {
                self.work_in_worktree(path, window, cx);
            }
            return;
        }
        self.overview_zoomed = None;
        if beside {
            let next = focus::toggled(&self.focus_worktrees(), path);
            // The window's own worktree taken away: the first left is.
            let gone = self
                .active
                .as_deref()
                .is_some_and(|active| !next.iter().any(|shown| shown == active));
            let first = next.first().cloned();
            self.choose_focus(next, cx);
            if let (true, Some(first)) = (gone, first) {
                self.select_worktree(first, window, cx);
            }
        } else {
            self.choose_focus(vec![path.to_path_buf()], cx);
            if self.active.as_deref() != Some(path) {
                self.select_worktree(path.to_path_buf(), window, cx);
            }
        }
        // The skill speaks for a checkout on show, which may have changed
        // project.
        self.ask_skill_status();
        cx.notify();
    }

    /// Folds the sidebar to its rail, or unfolds it.
    fn toggle_focus_rail(&mut self, cx: &mut Context<Self>) {
        self.focus_rail = !self.focus_rail;
        let rail = self.focus_rail;
        super::store::Store::update_global(cx, |store| store.session.focus_rail = rail);
        cx.notify();
    }

    // — The sidebar ————————————————————————————————————————————————

    /// Every project open and every worktree of each, under its project's
    /// name — the ones shown lit, as a list's selection is. The title bar's
    /// pickers are the plane's: here, the list is the choice. A project
    /// folds to its name, keeping in sight only what of it is on show.
    /// Folded, a rail.
    pub(super) fn render_focus_sidebar(
        &self,
        shown: &[PathBuf],
        doings: &Doings,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        if self.focus_rail {
            return self.render_focus_rail(shown, doings, cx);
        }
        let theme = cx.theme().clone();
        let mut rows: Vec<AnyElement> = Vec::new();
        for repo in self.repos.iter() {
            let main = repo.main.clone();
            let folded = self.focus_folded.contains(&main);
            let worktrees = live_worktrees(repo);
            // Folded, the project says what the loudest of its worktrees
            // does: a question hidden under a fold is still asked.
            let hidden_doing = folded.then(|| {
                overview::loudest(
                    worktrees
                        .iter()
                        .filter(|path| !shown.contains(path))
                        .filter_map(|path| doings.get(path).copied()),
                )
            });
            let (fold, pick) = (main.clone(), main.clone());
            // A project is a heading, not a row: small capitals in the muted
            // tone, so that the worktrees under it are what the eye reads.
            // Its `+` shows under the pointer only — five of them, one a
            // project, were five more things to read on every look.
            let group = SharedString::from(format!("focus-project-group-{}", main.display()));
            rows.push(
                h_flex()
                    .group(group.clone())
                    .w_full()
                    .pl_1()
                    .pr_2()
                    .pt_3()
                    .pb_1()
                    .gap_0p5()
                    .items_center()
                    // The chevron folds; the name shows the whole project.
                    .child(
                        Button::new(SharedString::from(format!(
                            "focus-project-fold-{}",
                            main.display()
                        )))
                        .ghost()
                        .xsmall()
                        .icon(icon(if folded {
                            "chevron-right"
                        } else {
                            "chevron-down"
                        }))
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.toggle_focus_project(&fold, cx);
                        })),
                    )
                    .child(
                        h_flex()
                            .id(SharedString::from(format!(
                                "focus-project-{}",
                                main.display()
                            )))
                            .relative()
                            .flex_1()
                            .min_w_0()
                            .px_1()
                            .py_0p5()
                            .gap_1p5()
                            .items_center()
                            .rounded(theme.radius)
                            .cursor_pointer()
                            .text_xs()
                            .font_weight(gpui_kit::FontWeight::SEMIBOLD)
                            .text_color(theme.muted_foreground)
                            .hover(|style| style.text_color(theme.foreground))
                            .tooltip(|window, cx| {
                                gpui_kit::component::tooltip::Tooltip::new(tr!(
                                    "focus-project-hint"
                                ))
                                .build(window, cx)
                            })
                            .on_click(cx.listener(
                                move |this, event: &gpui_kit::ClickEvent, window, cx| {
                                    this.focus_project_clicked(&pick, event, window, cx);
                                },
                            ))
                            .child(
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .truncate()
                                    .child(SharedString::from(repo.name.to_uppercase())),
                            )
                            .when(folded, |el| {
                                el.child(
                                    div()
                                        .flex_none()
                                        .font_weight(gpui_kit::FontWeight::NORMAL)
                                        .child(SharedString::from(worktrees.len().to_string())),
                                )
                            })
                            .children(outline_of(hidden_doing.as_ref(), &theme)),
                    )
                    .child(
                        div()
                            .opacity(0.)
                            .group_hover(group, |style| style.opacity(1.))
                            .child(
                                Button::new(SharedString::from(format!(
                                    "focus-new-worktree-{}",
                                    main.display()
                                )))
                                .ghost()
                                .xsmall()
                                .icon(icon("plus"))
                                .tooltip(tr!("worktree-new"))
                                .on_click(cx.listener(
                                    move |this, _, window, cx| {
                                        this.prompt_new_worktree(main.clone(), window, cx);
                                    },
                                )),
                            ),
                    )
                    .into_any_element(),
            );
            for path in worktrees {
                if folded && !shown.contains(&path) {
                    continue;
                }
                rows.push(self.render_focus_row(&path, shown, doings, cx));
            }
        }
        let list = v_flex()
            .id("focus-sidebar-list")
            .track_scroll(&self.focus_sidebar_scroll)
            .size_full()
            .pb_2()
            .overflow_y_scroll()
            .children(rows);
        // The home screen has no title bar: what it said is here — the
        // menu, and the way back to the editor. The head is the window's
        // drag region, as the title bar was; what can be pressed in it keeps
        // its press (`held`).
        let screens = h_flex()
            .w_full()
            .px_1p5()
            .pt_1p5()
            .gap_1p5()
            .items_center()
            .child(held(self.render_main_menu(cx)))
            .child(held(self.screen_segments(false, cx).w_full()).flex_1())
            .child(held(
                Button::new("focus-rail-fold")
                    .ghost()
                    .xsmall()
                    .icon(icon("panel-left-close"))
                    .tooltip(tr!("focus-sidebar-fold"))
                    .on_click(cx.listener(|this, _, _, cx| this.toggle_focus_rail(cx))),
            ));
        // What `Ctrl` does is said where it is done: the one gesture of the
        // list nothing on it would show.
        let hint = div()
            .w_full()
            .px_2()
            .pt_1p5()
            .pb_1p5()
            .truncate()
            .text_xs()
            .text_color(theme.muted_foreground)
            .child(tr!("focus-sidebar-hint"));
        let head = self
            .window_drag_region("home-drag", cx)
            .flex()
            .flex_col()
            .flex_none()
            .w_full()
            .border_b_1()
            .border_color(theme.border)
            .child(screens)
            .child(hint);
        // At the foot, how the worktrees chosen above are looked at — the two
        // views, what either hid — then what speaks for the whole screen and
        // not a worktree: the skill, another repository, putting the
        // worktrees on show back as the rules lay them, and the settings.
        let foot = v_flex()
            .flex_none()
            .w_full()
            .p_1p5()
            .gap_1p5()
            .border_t_1()
            .border_color(theme.border)
            .child(self.view_segments(false, cx).w_full())
            .child(
                h_flex()
                    .w_full()
                    .gap_1()
                    .items_center()
                    .child(self.render_skill_button(false, cx))
                    .children(self.render_hidden_menu(true, cx))
                    .child(div().flex_1())
                    .child(self.open_repo_button(cx))
                    .child(self.reset_button(cx))
                    .child(settings_button(cx)),
            );
        v_flex()
            .flex_none()
            .w(px(SIDEBAR_WIDTH))
            .h_full()
            .overflow_hidden()
            .rounded(theme.radius_lg)
            .border_1()
            .border_color(theme.border)
            .bg(theme.background)
            .child(head)
            .child(div().flex_1().min_h_0().child(super::scroll::vertical(
                "focus-sidebar-bar",
                &self.focus_sidebar_scroll,
                list,
            )))
            .child(foot)
            .into_any_element()
    }

    /// Opening another repository: it joins the list at once.
    fn open_repo_button(&self, cx: &mut Context<Self>) -> Button {
        Button::new("focus-open-repo")
            .ghost()
            .xsmall()
            .icon(icon("folder-plus"))
            .tooltip(tr!("repo-open"))
            .on_click(cx.listener(|this, _, window, cx| {
                this.prompt_open_repository(window, cx);
            }))
    }

    /// The two screens, as a segmented choice: the home screen — lit, being
    /// where one is — and the editor. `compact`, glyphs alone, one above the
    /// other.
    fn screen_segments(&self, compact: bool, cx: &mut Context<Self>) -> gpui_kit::Div {
        let label = |text: SharedString| (!compact).then_some(text);
        segmented(
            compact,
            vec![
                self.segment(
                    "home-screen-home",
                    "layout-dashboard",
                    label(tr!("overview-short")),
                    tr!("overview-toggle"),
                    true,
                    |_, _, _| {},
                    cx,
                ),
                self.segment(
                    "home-screen-editor",
                    "file-code",
                    label(tr!("editor-short")),
                    tr!("editor-toggle"),
                    false,
                    |this, window, cx| this.toggle_overview(window, cx),
                    cx,
                ),
            ],
            cx,
        )
    }

    /// The home screen's two views, as a segmented choice.
    fn view_segments(&self, compact: bool, cx: &mut Context<Self>) -> gpui_kit::Div {
        let label = |text: SharedString| (!compact).then_some(text);
        let mode = self.home_mode;
        segmented(
            compact,
            vec![
                self.segment(
                    "home-mode-focus",
                    "columns-2",
                    label(tr!("overview-mode-focus")),
                    tr!("overview-mode-hint"),
                    mode == HomeMode::Focus,
                    |this, _, cx| this.set_home_mode(HomeMode::Focus, cx),
                    cx,
                ),
                self.segment(
                    "home-mode-canvas",
                    "grid-3x3",
                    label(tr!("overview-mode-canvas")),
                    tr!("overview-mode-hint"),
                    mode == HomeMode::Canvas,
                    |this, _, cx| this.set_home_mode(HomeMode::Canvas, cx),
                    cx,
                ),
            ],
            cx,
        )
    }

    /// One segment of a `segmented` choice: its glyph, its word when there
    /// is room for one. The one chosen is raised — the card's own colour, a
    /// hairline, the full foreground and its glyph in the accent — and the
    /// others sit quieter in the track, brightening under the pointer.
    #[allow(clippy::too_many_arguments)]
    fn segment(
        &self,
        id: &'static str,
        glyph: &'static str,
        label: Option<SharedString>,
        tooltip: SharedString,
        lit: bool,
        press: impl Fn(&mut Self, &mut Window, &mut Context<Self>) + 'static,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let theme = cx.theme().clone();
        let (foreground, muted, hover) =
            (theme.foreground, theme.muted_foreground, theme.list_hover);
        let radius = px((f32::from(theme.radius) - 2.).max(0.));
        h_flex()
            .id(id)
            .flex_1()
            .justify_center()
            .items_center()
            .gap_1p5()
            .px_2()
            .py_1()
            .rounded(radius)
            .border_1()
            .text_sm()
            .cursor_pointer()
            .map(|el| {
                if lit {
                    el.bg(theme.background)
                        .border_color(theme.border)
                        .shadow_sm()
                        .text_color(foreground)
                        .font_weight(gpui_kit::FontWeight::MEDIUM)
                } else {
                    el.border_color(gpui_kit::transparent_black())
                        .text_color(muted)
                        .hover(move |style| style.text_color(foreground).bg(hover))
                }
            })
            .tooltip(move |window, cx| {
                gpui_kit::component::tooltip::Tooltip::new(tooltip.clone()).build(window, cx)
            })
            .on_click(cx.listener(move |this, _, window, cx| press(this, window, cx)))
            .child(icon(glyph).text_color(if lit { theme.ring } else { muted }))
            .children(label.map(|label| div().truncate().child(label)))
            .into_any_element()
    }

    /// Puts the worktrees on show back as the rules lay them — see
    /// `reset_overview`.
    fn reset_button(&self, cx: &mut Context<Self>) -> Button {
        Button::new("home-reset")
            .ghost()
            .small()
            .icon(icon("layout-dashboard"))
            .tooltip(match self.home_mode {
                HomeMode::Focus => tr!("focus-reset-hint"),
                HomeMode::Canvas => tr!("overview-reset-hint"),
            })
            .on_click(cx.listener(|this, _, _, cx| this.reset_overview(cx)))
    }

    /// A project's name pressed: all its worktrees on show — with `Ctrl`,
    /// beside the ones already there. The window's own stays if it is among
    /// them, else the first of them becomes it.
    fn focus_project_clicked(
        &mut self,
        main: &Path,
        event: &gpui_kit::ClickEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(repo) = self.repos.iter().find(|repo| repo.main == main) else {
            return;
        };
        let worktrees = live_worktrees(repo);
        let mut chosen = if event.modifiers().secondary() {
            self.focus_worktrees()
        } else {
            Vec::new()
        };
        for path in &worktrees {
            if !chosen.contains(path) {
                chosen.push(path.clone());
            }
        }
        self.overview_zoomed = None;
        let keeps = self
            .active
            .as_deref()
            .is_some_and(|active| chosen.iter().any(|path| path == active));
        let first = worktrees.first().cloned();
        self.choose_focus(chosen, cx);
        if let (false, Some(first)) = (keeps, first) {
            self.select_worktree(first, window, cx);
        }
        self.ask_skill_status();
        cx.notify();
    }

    /// A worktree's own menu under a right click on its row or its pill —
    /// the editor's `…`, for any worktree of the list and not only the one
    /// on show: open, run its tasks, integrate, remove, and the rest.
    fn worktree_context_menu(
        &self,
        path: &Path,
        cx: &mut Context<Self>,
    ) -> impl Fn(PopupMenu, &mut Window, &mut Context<PopupMenu>) -> PopupMenu + 'static {
        let entity = cx.entity();
        let main = self.main_of(path);
        let worktree = path.to_path_buf();
        move |menu, _window, cx| match &main {
            // Built on the right click, not in a render: the application is
            // free to be read.
            Some(main) => entity.update(cx, |this, cx| {
                this.worktree_menu(menu, main.clone(), worktree.clone(), cx)
            }),
            None => menu,
        }
    }

    /// Folds a project of the sidebar to its name, or unfolds it.
    fn toggle_focus_project(&mut self, main: &Path, cx: &mut Context<Self>) {
        if let Some(at) = self.focus_folded.iter().position(|folded| folded == main) {
            self.focus_folded.remove(at);
        } else {
            self.focus_folded.push(main.to_path_buf());
            self.focus_folded.sort();
        }
        let folded = self.focus_folded.clone();
        super::store::Store::update_global(cx, |store| store.session.focus_folded = folded);
        cx.notify();
    }

    /// The sidebar folded: a column of initials, a line between two
    /// projects — every worktree, a project's fold being the list's. The
    /// same clicks as the list's, and each worktree's name in its tooltip;
    /// the foot's buttons, their glyphs alone.
    pub(super) fn render_focus_rail(
        &self,
        shown: &[PathBuf],
        doings: &Doings,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let theme = cx.theme().clone();
        let mut pills: Vec<AnyElement> = Vec::new();
        for (index, repo) in self.repos.iter().enumerate() {
            if index > 0 {
                pills.push(
                    div()
                        .flex_none()
                        .w(px(24.))
                        .h(px(1.))
                        .my_1()
                        .bg(theme.border)
                        .into_any_element(),
                );
            }
            for path in live_worktrees(repo) {
                pills.push(self.render_focus_pill(&path, shown, doings, cx));
            }
        }
        let list = v_flex()
            .id("focus-rail-list")
            .track_scroll(&self.focus_sidebar_scroll)
            .size_full()
            .items_center()
            .gap_1()
            .py_1()
            .overflow_y_scroll()
            .children(pills);
        let foot = v_flex()
            .flex_none()
            .w_full()
            .py_1p5()
            .gap_1()
            .items_center()
            .border_t_1()
            .border_color(theme.border)
            .child(self.view_segments(true, cx))
            .children(self.render_hidden_menu(true, cx))
            .child(self.open_repo_button(cx))
            .child(self.render_skill_button(true, cx))
            .child(self.reset_button(cx))
            .child(settings_button(cx));
        // The list's head, one above the other: the menu, the two screens,
        // and the unfold — the head moving the window, as the list's does.
        let head = self
            .window_drag_region("home-drag", cx)
            .flex()
            .flex_col()
            .flex_none()
            .w_full()
            .items_center()
            .gap_1()
            .py_1p5()
            .border_b_1()
            .border_color(theme.border)
            .child(held(self.render_main_menu(cx)))
            .child(held(self.screen_segments(true, cx)))
            .child(held(
                Button::new("focus-rail-unfold")
                    .ghost()
                    .xsmall()
                    .icon(icon("panel-left-open"))
                    .tooltip(tr!("focus-sidebar-unfold"))
                    .on_click(cx.listener(|this, _, _, cx| this.toggle_focus_rail(cx))),
            ));
        v_flex()
            .flex_none()
            .w(px(RAIL_WIDTH))
            .h_full()
            .items_center()
            .overflow_hidden()
            .rounded(theme.radius_lg)
            .border_1()
            .border_color(theme.border)
            .bg(theme.background)
            .child(head)
            .child(div().flex_1().min_h_0().w_full().child(list))
            .child(foot)
            .into_any_element()
    }

    /// A worktree on the rail: two letters, lit when shown, ringed when it
    /// is the window's own, and its agent's signal on its edge.
    fn render_focus_pill(
        &self,
        path: &Path,
        shown: &[PathBuf],
        doings: &Doings,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let theme = cx.theme().clone();
        let (_, label) = self.project_label(path);
        let branch = self
            .repos
            .worktree(path)
            .and_then(|worktree| worktree.branch.clone())
            .filter(|branch| branch.as_str() != label.as_ref());
        let name = SharedString::from(match &branch {
            Some(branch) => format!("{label} · {branch}"),
            None => label.to_string(),
        });
        let lit = shown.iter().any(|shown| shown == path);
        let own = self.active.as_deref() == Some(path);
        let open = path.to_path_buf();
        div()
            .id(SharedString::from(format!("focus-pill-{}", path.display())))
            .relative()
            .flex_none()
            .size(px(34.))
            .flex()
            .items_center()
            .justify_center()
            .rounded(theme.radius)
            .border_1()
            .border_color(if own && !dressed(doings, path) {
                theme.ring
            } else {
                gpui_kit::transparent_black()
            })
            .when(lit, |el| el.bg(theme.list_active))
            .when(!lit, |el| el.hover(|style| style.bg(theme.list_hover)))
            .cursor_pointer()
            .text_xs()
            .font_weight(gpui_kit::FontWeight::SEMIBOLD)
            .text_color(if lit {
                theme.foreground
            } else {
                theme.muted_foreground
            })
            .tooltip(move |window, cx| {
                gpui_kit::component::tooltip::Tooltip::new(name.clone()).build(window, cx)
            })
            .on_click(
                cx.listener(move |this, event: &gpui_kit::ClickEvent, window, cx| {
                    this.focus_row_clicked(&open, event, window, cx);
                }),
            )
            .child(SharedString::from(focus::initials(&label)))
            .children(edge_signal(doings.get(path), 7., &theme))
            .context_menu(self.worktree_context_menu(path, cx))
            .into_any_element()
    }

    /// A worktree in the sidebar: one line, most of the time — its name,
    /// and its branch only when that says something else (`row_words`) —
    /// and at the right, quiet and short, how many terminals it has and how
    /// much it has in progress. Who works in it is its edge — no dot besides:
    /// it said again what the edge says. A click shows it alone, `Ctrl`+click beside the
    /// others; twice goes to work in it, as a card's double click does.
    fn render_focus_row(
        &self,
        path: &Path,
        shown: &[PathBuf],
        doings: &Doings,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let theme = cx.theme().clone();
        let worktree = self.repos.worktree(path);
        let is_main = worktree.is_some_and(|worktree| worktree.is_main);
        let label = worktree
            .map(|worktree| worktree.label())
            .unwrap_or_else(|| self.project_label(path).1.to_string());
        let (title, detail) = focus::row_words(
            &label,
            worktree.and_then(|worktree| worktree.branch.as_deref()),
            is_main,
        );
        let lit = shown.iter().any(|shown| shown == path);
        let own = self.active.as_deref() == Some(path);
        let dressed = dressed(doings, path);
        let summary = self
            .summaries
            .get(path)
            .copied()
            .filter(|summary| !summary.is_empty());
        let terminals = self
            .terminals
            .iter()
            .filter(|terminal| terminal.worktree == path)
            .count();
        let diff = super::theme::DiffColors::of(cx);
        let open = path.to_path_buf();
        let volume = summary.map(|summary| {
            h_flex()
                .flex_none()
                .gap_1()
                .when(summary.added > 0, |el| {
                    el.child(
                        div()
                            .text_color(diff.added_fg)
                            .child(format!("+{}", focus::short_count(summary.added))),
                    )
                })
                .when(summary.removed > 0, |el| {
                    el.child(
                        div()
                            .text_color(diff.removed_fg)
                            .child(format!("−{}", focus::short_count(summary.removed))),
                    )
                })
                // A rename or a binary moves no line: the files, then.
                .when(summary.added == 0 && summary.removed == 0, |el| {
                    el.child(SharedString::from(summary.files.to_string()))
                })
        });
        h_flex()
            .id(SharedString::from(format!("focus-row-{}", path.display())))
            .relative()
            .w_full()
            .pl_3()
            .pr_2()
            .py_1()
            .gap_2()
            .items_center()
            .cursor_pointer()
            // The selection is a band, and a bar at its edge says it louder
            // than a tint alone — the sidebar is read at a glance.
            .border_l_2()
            .border_color(if lit && !dressed {
                theme.ring
            } else {
                gpui_kit::transparent_black()
            })
            .when(lit, |el| el.bg(theme.list_active))
            .when(!lit, |el| el.hover(|style| style.bg(theme.list_hover)))
            .on_click(
                cx.listener(move |this, event: &gpui_kit::ClickEvent, window, cx| {
                    this.focus_row_clicked(&open, event, window, cx);
                }),
            )
            // The main checkout is the project's house; the others branch.
            .child(
                icon(if is_main { "house" } else { "git-branch" })
                    .xsmall()
                    .text_color(if lit {
                        theme.ring
                    } else {
                        theme.muted_foreground
                    }),
            )
            .child(
                v_flex()
                    .flex_1()
                    .min_w_0()
                    .child(
                        div()
                            .truncate()
                            .text_sm()
                            .text_color(theme.foreground)
                            // The window's own, among those shown.
                            .when(own, |el| el.font_weight(gpui_kit::FontWeight::SEMIBOLD))
                            .child(SharedString::from(title)),
                    )
                    .children(detail.map(|detail| {
                        div()
                            .truncate()
                            .text_xs()
                            .text_color(theme.muted_foreground)
                            .child(SharedString::from(detail))
                    })),
            )
            .child(
                h_flex()
                    .flex_none()
                    .gap_2()
                    .items_center()
                    .text_xs()
                    .text_color(theme.muted_foreground)
                    .when(terminals > 0, |el| {
                        el.child(
                            h_flex()
                                .gap_0p5()
                                .items_center()
                                .child(icon("square-terminal").xsmall())
                                .child(SharedString::from(terminals.to_string())),
                        )
                    })
                    .children(volume),
            )
            .children(outline_of(doings.get(path), &theme))
            .context_menu(self.worktree_context_menu(path, cx))
            .into_any_element()
    }

    // — The board ——————————————————————————————————————————————————

    /// A worktree's terminals, in the order they were opened.
    pub(super) fn board_terminals(&self, path: &Path) -> Vec<u64> {
        self.terminals
            .iter()
            .filter(|terminal| terminal.worktree == path)
            .map(|terminal| terminal.view.entity_id().as_u64())
            .collect()
    }

    /// A worktree's notes and its repository's, the hidden ones left out.
    pub(super) fn board_notes(&self, path: &Path) -> Vec<PathBuf> {
        let hidden = &self.overview_hand.hidden;
        let groups = self.overview_groups();
        let group = groups
            .iter()
            .find(|group| group.checkouts.iter().any(|c| c.path == path));
        let checkout = group.and_then(|group| group.checkouts.iter().find(|c| c.path == path));
        checkout
            .map(|checkout| checkout.notes.clone())
            .into_iter()
            .flatten()
            .chain(group.map(|group| group.notes.clone()).into_iter().flatten())
            .filter(|note| !hidden.contains(&Node::Note(note.clone())))
            .collect()
    }

    /// The note a board's home shows as its principal one — see
    /// `focus::principal_note`.
    pub(super) fn principal_note(&self, path: &Path, cx: &App) -> Option<PathBuf> {
        let pinned = super::store::Store::global(cx)
            .worktrees
            .get(path)
            .and_then(|state| state.pinned_note.clone());
        let notes: Vec<(PathBuf, Option<String>)> = self
            .board_notes(path)
            .into_iter()
            .map(|note| {
                let created = self
                    .canvas_entry(&note)
                    .and_then(|(_, entry)| entry.node.created.clone());
                (note, created)
            })
            .collect();
        focus::principal_note(pinned.as_deref(), &notes)
    }

    /// Pins a note as a board's principal one, or — pinned already — lets
    /// the newest be again.
    pub(super) fn toggle_pinned_note(&mut self, path: &Path, note: &Path, cx: &mut Context<Self>) {
        super::store::Store::update_global(cx, |store| {
            let state = store.worktrees.entry(path.to_path_buf()).or_default();
            state.pinned_note = match state.pinned_note.as_deref() {
                Some(pinned) if pinned == note => None,
                _ => Some(note.to_path_buf()),
            };
        });
        cx.notify();
    }

    /// The view a board shows under its tabs — see `focus::view_of`.
    pub(super) fn board_view(&self, path: &Path, cx: &App) -> View {
        let chosen = super::store::Store::global(cx)
            .worktrees
            .get(path)
            .and_then(|state| state.focus_view);
        focus::view_of(chosen)
    }

    /// Puts a view on a board, in the place of the one there.
    pub(super) fn show_board_view(&mut self, path: &Path, view: View, cx: &mut Context<Self>) {
        super::store::Store::update_global(cx, |store| {
            store
                .worktrees
                .entry(path.to_path_buf())
                .or_default()
                .focus_view = Some(view);
        });
        self.overview_zoomed = None;
        cx.notify();
    }

    /// A worktree on show: its title — its name, its branch and what acts
    /// on it —, its tabs, and the one view chosen under them.
    ///
    /// **The title is the board's own and scrolls with it.** It stood in a
    /// line over the row, held at the left of the part of its board in view,
    /// which meant placing it from what the row measured at every frame —
    /// and the line flickered at every sideways scroll.
    fn render_focus_board(
        &mut self,
        path: &Path,
        _gap: Pixels,
        at_work: &overview::AtWork,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let view = self.board_view(path, cx);
        let column_min = super::settings::Settings::global(cx).terminal.column_min;
        // Never narrower than what its view lays side by side: under it, the
        // view ran over the next board. The terminals' tab scrolls its
        // terminals, not the board.
        let least_width = match view {
            View::Home => super::summary_view::HOME_LEAST,
            View::Git => GIT_LIST_WIDTH.max(420.) + 8. + column_min,
            View::Notes => NOTES_LIST_WIDTH + 8. + column_min,
            View::Review => super::changes_view::REVIEW_LIST_WIDTH + column_min,
            View::Todo | View::Terminals | View::Pr => column_min,
            View::Tests => TESTS_LIST_WIDTH + 8. + column_min,
        };
        let open = path.to_path_buf();
        let actions = vec![Button::new("focus-edit")
            .ghost()
            .small()
            .icon(icon("pencil"))
            .label(tr!("focus-edit"))
            .tooltip(tr!("overview-open-worktree"))
            .on_click(cx.listener(move |this, _, window, cx| {
                this.work_in_worktree(&open, window, cx);
            }))
            .into_any_element()];
        // What runs in it — the environment, the recipes —, at the far
        // right: see `run_view`. No « Review »: the board has its tab.
        let run: Vec<AnyElement> = self
            .render_run(path, gpui_kit::component::Size::Small, cx)
            .into_iter()
            .collect();
        let title = h_flex()
            .flex_none()
            .w_full()
            .h(super::theme::toolbar_height(cx))
            .items_center()
            .child(self.board_title(path, actions, run, cx));
        let tabs = self.render_board_tabs(path, view, at_work, cx);
        let shown = self.render_board_view(path, view, at_work, window, cx);
        v_flex()
            .relative()
            // Its own id, under which its ids are told apart from the next
            // board's.
            .id(SharedString::from(format!(
                "focus-board-{}",
                path.display()
            )))
            .flex_1()
            .min_w(px(least_width))
            .h_full()
            .gap_2()
            // What does not fit is cut at the board's edge, never painted
            // over the next one.
            .overflow_hidden()
            .child(title)
            .child(tabs)
            .child(div().relative().flex_1().min_h_0().w_full().child(shown))
            .into_any_element()
    }

    /// The view under a board's tabs, whole.
    fn render_board_view(
        &mut self,
        path: &Path,
        view: View,
        at_work: &overview::AtWork,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        match view {
            View::Home => self.render_home_view(path, at_work, window, cx),
            View::Git => self.render_git_view(path, window, cx),
            View::Review => self.render_review_card(path, true, false, window, cx),
            View::Pr => self.render_pr_view(path, window, cx),
            View::Tests => self.render_tests_view(path, window, cx),
            View::Notes => self.render_notes_view(path, cx),
            View::Todo => self.render_todo_view(path, cx),
            View::Terminals => self.render_terminals_view(path, at_work, window, cx),
        }
    }

    /// What a view's tab says of it, beside the view — and its card on the
    /// home too: the files and lines a commit would take, what the
    /// review compares against, how far the to-do list has come.
    pub(super) fn view_detail(
        &self,
        path: &Path,
        view: View,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        match view {
            View::Git => {
                let summary = self.summaries.get(path).copied().unwrap_or_default();
                Some(if summary.is_empty() {
                    div().child(tr!("home-clean")).into_any_element()
                } else {
                    h_flex()
                        .gap_1()
                        .items_center()
                        .child(tr!("home-files", { count: summary.files }))
                        .child(super::topbar::volume_on(summary, None, cx))
                        .into_any_element()
                })
            }
            View::Review => self
                .review
                .get(path)
                .and_then(
                    |state| match (state.since_review, state.review_point.as_ref()) {
                        (true, Some(_)) => Some(tr!("sheet-review-since")),
                        _ => state
                            .base
                            .clone()
                            .map(|base| tr!("sheet-review-against", { base: base })),
                    },
                )
                .map(|text| div().truncate().child(text).into_any_element()),
            View::Todo => self
                .review
                .get(path)
                .and_then(|state| state.todo.as_ref())
                .map(|todo| {
                    div()
                        .child(tr!("todo-progress", { done: todo.done(), total: todo.tasks.len() }))
                        .into_any_element()
                }),
            View::Home | View::Notes | View::Terminals | View::Pr | View::Tests => None,
        }
    }

    /// The git view — the commit sheet laid in the board, no dialog: on the
    /// left the branch, how far it is from its remote and from its base —
    /// with the way to merge it there — and the commits it adds, over the
    /// Changes panel and its commit box; the diff on the right.
    fn render_git_view(
        &mut self,
        path: &Path,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let history = self.git_history.contains(path);
        let switch = |id: &'static str, label: SharedString, glyph: &'static str, on: bool| {
            let board = path.to_path_buf();
            Button::new(id)
                .ghost()
                .small()
                .icon(icon(glyph))
                .label(label)
                .selected(on)
                .on_click(cx.listener(move |this, _, _, cx| {
                    if id == "focus-git-history" {
                        this.git_history.insert(board.clone());
                    } else {
                        this.git_history.remove(&board);
                    }
                    cx.notify();
                }))
        };
        let bar = h_flex()
            .flex_none()
            .gap_1()
            .child(switch(
                "focus-git-changes",
                tr!("focus-git-changes"),
                "git-commit-horizontal",
                !history,
            ))
            .child(switch(
                "focus-git-history",
                tr!("focus-git-history"),
                "history",
                history,
            ));
        let body = if history {
            self.render_git_history(path, window, cx)
        } else {
            self.render_git_changes(path, window, cx)
        };
        v_flex()
            .size_full()
            .gap_2()
            .child(bar)
            .child(div().flex_1().min_h_0().w_full().child(body))
            .into_any_element()
    }

    /// The git view's history: the editor's history panel — the graph, its
    /// search, the files of the commit chosen — beside that commit's diff.
    fn render_git_history(
        &mut self,
        path: &Path,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        if self.active.as_deref() != Some(path) {
            return self.follow_the_active(path, cx);
        }
        if self.commit_sheet {
            return centered(tr!("focus-review-sheet"), cx);
        }
        let theme = cx.theme().clone();
        self.ensure_history(cx);
        let on_commit = self
            .review
            .get(path)
            .is_some_and(|state| matches!(state.range, crate::git::DiffRange::Commit { .. }));
        let history = self.render_history(window, cx).into_any_element();
        let diff = if on_commit {
            self.render_diff(window, cx).into_any_element()
        } else {
            centered(tr!("focus-git-pick-commit"), cx)
        };
        let boxed = |el: gpui_kit::Div| {
            el.h_full()
                .overflow_hidden()
                .rounded(theme.radius_lg)
                .border_1()
                .border_color(theme.border)
                .bg(theme.background)
        };
        h_flex()
            .size_full()
            .gap_2()
            .child(boxed(v_flex().flex_grow(3.).flex_basis(px(0.)).min_w(px(420.))).child(history))
            .child(boxed(v_flex().flex_grow(2.).flex_basis(px(0.)).min_w_0()).child(diff))
            .into_any_element()
    }

    /// The git view's changes: the commit sheet laid in the board.
    fn render_git_changes(
        &mut self,
        path: &Path,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let theme = cx.theme().clone();
        let muted = theme.muted_foreground;
        let worktree = self.repos.worktree(path);
        let is_main = worktree.is_some_and(|worktree| worktree.is_main);
        let branch = worktree
            .and_then(|worktree| worktree.branch.clone())
            .map(SharedString::from)
            .unwrap_or_else(|| tr!("overview-detached"));
        let outline = self.outlines.get(path);
        let upstream = outline
            .and_then(|outline| outline.upstream)
            .map(|(ahead, behind)| SharedString::from(format!("↑{ahead} ↓{behind}")));
        let ahead = outline.map_or(0, |outline| outline.ahead_of_base);
        let base = outline.and_then(|outline| outline.base.clone());
        let title = match (&base, ahead) {
            (Some(base), ahead) if ahead > 0 => tr!("overview-ahead", { count: ahead, base: base }),
            _ => tr!("overview-recent"),
        };
        let merge = base.filter(|_| !is_main && ahead > 0).map(|base| {
            let merge = path.to_path_buf();
            Button::new("focus-git-merge")
                .ghost()
                .xsmall()
                .icon(icon("git-merge"))
                .label(tr!("overview-merge", { base: base }))
                .on_click(cx.listener(move |this, _, window, cx| {
                    this.confirm_merge(&merge, window, cx);
                }))
        });
        let now = chrono::Utc::now().timestamp();
        let commits = outline
            .map(|outline| outline.commits.clone())
            .unwrap_or_default();
        let facts = v_flex()
            .flex_none()
            .w_full()
            .p_3()
            .gap_2()
            .rounded(theme.radius_lg)
            .border_1()
            .border_color(theme.border)
            .bg(theme.background)
            .text_sm()
            .child(
                h_flex()
                    .gap_1p5()
                    .items_center()
                    .child(icon("git-branch").xsmall().text_color(muted))
                    .child(
                        div()
                            .min_w_0()
                            .truncate()
                            .font_weight(gpui_kit::FontWeight::SEMIBOLD)
                            .child(branch),
                    )
                    .children(
                        upstream
                            .map(|text| div().flex_none().text_xs().text_color(muted).child(text)),
                    )
                    .child(div().flex_1())
                    .children(merge),
            )
            .child(div().text_xs().text_color(muted).child(title))
            .children(commits.into_iter().take(GIT_COMMITS).map(|commit| {
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
            }));
        // The Changes panel and the diff are the editor's, one of each: only
        // the worktree on show has them, and only while no sheet paints them.
        let (list, diff) = if self.active.as_deref() != Some(path) {
            (div().into_any_element(), self.follow_the_active(path, cx))
        } else if self.commit_sheet {
            (
                div().into_any_element(),
                centered(tr!("focus-review-sheet"), cx),
            )
        } else {
            let ready = self.pick_git_file(path, cx);
            let list = self.render_changes(window, cx).into_any_element();
            let diff = if ready {
                self.render_diff(window, cx).into_any_element()
            } else {
                centered(tr!("review-clean"), cx)
            };
            (list, diff)
        };
        let border = theme.border;
        h_flex()
            .size_full()
            .gap_2()
            .child(
                v_flex()
                    .flex_none()
                    .w(px(GIT_LIST_WIDTH))
                    .h_full()
                    .gap_2()
                    .child(facts)
                    .child(
                        v_flex()
                            .flex_1()
                            .min_h_0()
                            .w_full()
                            .overflow_hidden()
                            .rounded(theme.radius_lg)
                            .border_1()
                            .border_color(border)
                            .bg(theme.background)
                            .child(list),
                    ),
            )
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .h_full()
                    .overflow_hidden()
                    .rounded(theme.radius_lg)
                    .border_1()
                    .border_color(border)
                    .bg(theme.background)
                    .child(diff),
            )
            .into_any_element()
    }

    /// Brings the diff to the changes in progress for the git view — the
    /// file shown kept when it is among them, else the first one — and says
    /// whether there is one to show. The review leaves it on the branch's
    /// range, the editor on whatever it was reading.
    fn pick_git_file(&mut self, worktree: &Path, cx: &mut Context<Self>) -> bool {
        let Some(state) = self.review.get(worktree) else {
            return false;
        };
        let kept = state.range == crate::git::DiffRange::Working
            && state
                .selected
                .as_deref()
                .is_some_and(|selected| state.status.file(selected).is_some());
        if kept {
            return true;
        }
        let Some(first) = state.status.files.first().map(|file| file.path.clone()) else {
            return false;
        };
        self.open_file(
            worktree.to_path_buf(),
            first,
            crate::git::DiffRange::Working,
            cx,
        );
        true
    }

    /// The notes view: the list on the left — each with its pin, the
    /// principal one first — and the note chosen on the right, the
    /// principal one until another is pressed.
    fn render_notes_view(&self, path: &Path, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme().clone();
        let principal = self.principal_note(path, cx);
        let pinned = super::store::Store::global(cx)
            .worktrees
            .get(path)
            .and_then(|state| state.pinned_note.clone());
        let mut notes = self.board_notes(path);
        if let Some(principal) = &principal {
            notes.retain(|note| note != principal);
            notes.insert(0, principal.clone());
        }
        if notes.is_empty() {
            return centered(tr!("focus-notes-none"), cx);
        }
        let shown = self
            .focus_note_shown
            .get(path)
            .filter(|note| notes.contains(note))
            .cloned()
            .or(principal)
            .unwrap_or_else(|| notes[0].clone());
        let rows: Vec<AnyElement> = notes
            .iter()
            .enumerate()
            .map(|(index, note)| {
                let (_, name) = self.card_name(&Node::Note(note.clone()), cx);
                let lit = *note == shown;
                let is_pinned = pinned.as_deref() == Some(note.as_path());
                let (board, pressed, pin) = (path.to_path_buf(), note.clone(), note.clone());
                let pin_board = path.to_path_buf();
                h_flex()
                    .id(("focus-note-row", index))
                    .w_full()
                    .pl_2()
                    .pr_1()
                    .h(super::theme::row_height(cx))
                    .gap_1p5()
                    .items_center()
                    .cursor_pointer()
                    .when(lit, |el| el.bg(theme.list_active))
                    .when(!lit, |el| el.hover(|style| style.bg(theme.list_hover)))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.focus_note_shown.insert(board.clone(), pressed.clone());
                        cx.notify();
                    }))
                    .child(
                        icon("sticky-note")
                            .xsmall()
                            .text_color(theme.muted_foreground),
                    )
                    .child(div().flex_1().min_w_0().truncate().text_sm().child(name))
                    .child(
                        Button::new(("focus-note-pin", index))
                            .ghost()
                            .xsmall()
                            .icon(icon("pin"))
                            .when(is_pinned, |button| button.text_color(theme.ring))
                            .tooltip(if is_pinned {
                                tr!("focus-note-unpin")
                            } else {
                                tr!("focus-note-pin")
                            })
                            .on_click(cx.listener(move |this, _, _, cx| {
                                cx.stop_propagation();
                                this.toggle_pinned_note(&pin_board, &pin, cx);
                            })),
                    )
                    .into_any_element()
            })
            .collect();
        h_flex()
            .size_full()
            .gap_2()
            .child(
                v_flex()
                    .id("focus-notes-list")
                    .flex_none()
                    .w(px(NOTES_LIST_WIDTH))
                    .h_full()
                    .py_1()
                    .overflow_y_scroll()
                    .rounded(theme.radius_lg)
                    .border_1()
                    .border_color(theme.border)
                    .bg(theme.background)
                    .children(rows),
            )
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .h_full()
                    .child(self.render_home_note(&shown, 1., cx)),
            )
            .into_any_element()
    }

    /// The tests view — the editor's two panels side by side: the tree of
    /// the suites, filtered by default to the tests the branch touched, and
    /// the run being followed. The worktree on show's, as those panels are.
    fn render_tests_view(
        &mut self,
        path: &Path,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        if self.active.as_deref() != Some(path) {
            return self.follow_the_active(path, cx);
        }
        let theme = cx.theme().clone();
        let tree = self.render_pest_in(true, window, cx);
        let run = self.render_test_run(window, cx).into_any_element();
        let boxed = |el: gpui_kit::Div| {
            el.h_full()
                .overflow_hidden()
                .rounded(theme.radius_lg)
                .border_1()
                .border_color(theme.border)
                .bg(theme.background)
        };
        h_flex()
            .size_full()
            .gap_2()
            .child(boxed(v_flex().flex_none().w(px(TESTS_LIST_WIDTH))).child(tree))
            .child(boxed(v_flex().flex_1().min_w_0()).child(run))
            .into_any_element()
    }

    /// The to-do view: the notes panel's list — ticked, edited, added to in
    /// place. It speaks of the worktree on show, as that panel does.
    fn render_todo_view(&mut self, path: &Path, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme().clone();
        if self.active.as_deref() != Some(path) {
            return self.follow_the_active(path, cx);
        }
        v_flex()
            .id("focus-todo")
            .size_full()
            .overflow_y_scroll()
            .rounded(theme.radius_lg)
            .border_1()
            .border_color(theme.border)
            .bg(theme.background)
            .child(self.render_todo_section(true, cx))
            .into_any_element()
    }

    /// A view that only the worktree on show can have — the editor's panels
    /// are one of each —, on another: what it would be, and the way there.
    pub(super) fn follow_the_active(&self, path: &Path, cx: &mut Context<Self>) -> AnyElement {
        let worktree = path.to_path_buf();
        v_flex()
            .size_full()
            .gap_2()
            .items_center()
            .justify_center()
            .child(
                div()
                    .text_xs()
                    .text_color(cx.theme().muted_foreground)
                    .child(tr!("focus-review-idle")),
            )
            .child(
                Button::new(SharedString::from(format!(
                    "focus-follow-{}",
                    path.display()
                )))
                .small()
                .icon(icon("eye"))
                .label(tr!("focus-review-show"))
                .on_click(cx.listener(move |this, _, window, cx| {
                    this.select_worktree(worktree.clone(), window, cx);
                    cx.notify();
                })),
            )
            .into_any_element()
    }

    /// The terminals view: every terminal of the worktree side by side,
    /// scrolling sideways when they outgrow it.
    fn render_terminals_view(
        &self,
        path: &Path,
        at_work: &overview::AtWork,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let ids = self.board_terminals(path);
        if ids.is_empty() {
            let worktree = path.to_path_buf();
            return v_flex()
                .size_full()
                .gap_2()
                .items_center()
                .justify_center()
                .child(
                    div()
                        .text_xs()
                        .text_color(cx.theme().muted_foreground)
                        .child(tr!("focus-terminals-none")),
                )
                .child(
                    Button::new("focus-terminals-open")
                        .small()
                        .icon(icon("square-terminal"))
                        .label(tr!("terminal-new"))
                        .on_click(cx.listener(move |this, _, window, cx| {
                            this.open_focus_terminal(&worktree, window, cx);
                        })),
                )
                .into_any_element();
        }
        let column_min = super::settings::Settings::global(cx).terminal.column_min;
        let tiles: Vec<AnyElement> = ids
            .iter()
            .filter_map(|id| {
                let terminal = self
                    .terminals
                    .iter()
                    .find(|terminal| terminal.view.entity_id().as_u64() == *id)?;
                let doing = at_work
                    .terminals
                    .get(id)
                    .copied()
                    .unwrap_or(overview::Doing::Rest);
                Some(
                    div()
                        .flex_1()
                        .min_w(px(column_min))
                        .h_full()
                        .child(self.render_tile(terminal, None, 1., doing, window, cx))
                        .into_any_element(),
                )
            })
            .collect();
        // They scroll sideways inside the view, each never under the least
        // width, and the tabs above them stay where they are.
        h_flex()
            .id("focus-terminals")
            .size_full()
            .gap_2()
            .overflow_x_scroll()
            .children(tiles)
            .into_any_element()
    }

    /// Opens a shell in the worktree and hands it the keyboard, from a
    /// gesture of the board — whose view becomes its terminals: asking for
    /// one is asking to see it.
    pub(super) fn open_focus_terminal(
        &mut self,
        worktree: &Path,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.show_board_view(worktree, View::Terminals, cx);
        self.open_terminal(worktree, super::terminal_view::Launch::shell(), window, cx);
        if let Some(view) = self.terminals.last().map(|terminal| terminal.view.clone()) {
            super::dialogs::focus_field(&view, window, cx);
        }
    }

    /// What a thing is called, with its glyph.
    pub(super) fn card_name(
        &self,
        node: &Node,
        cx: &Context<Self>,
    ) -> (&'static str, SharedString) {
        match node {
            Node::Worktree(path) => ("git-branch", self.project_label(path).1),
            Node::Terminal(id) => (
                "square-terminal",
                self.terminals
                    .iter()
                    .find(|terminal| terminal.view.entity_id().as_u64() == *id)
                    .map(|terminal| {
                        terminal
                            .name
                            .clone()
                            .unwrap_or_else(|| terminal.view.read(cx).label())
                    })
                    .unwrap_or_default(),
            ),
            Node::Note(path) => (
                "sticky-note",
                self.canvas_entry(path)
                    .and_then(|(_, entry)| entry.node.heading())
                    .map(SharedString::from)
                    .unwrap_or_else(|| tr!("overview-note")),
            ),
            Node::Git(_) | Node::Changes(_) => view_name(View::Git),
            Node::Review(_) => view_name(View::Review),
        }
    }
}

/// A view's glyph and name.
pub(super) fn view_name(view: View) -> (&'static str, SharedString) {
    match view {
        View::Home => ("house", tr!("focus-home")),
        View::Git => ("git-branch", tr!("focus-view-git")),
        View::Review => ("file-diff", tr!("focus-review")),
        View::Pr => ("git-pull-request", tr!("focus-view-pr")),
        View::Tests => ("circle-check", tr!("focus-view-tests")),
        View::Notes => ("sticky-note", tr!("focus-view-notes")),
        View::Todo => ("check-check", tr!("todo-title")),
        View::Terminals => ("square-terminal", tr!("focus-view-terminals")),
    }
}

/// A line of text in the middle of a view that has nothing to show.
fn centered(text: SharedString, cx: &mut Context<ClaudhubApp>) -> AnyElement {
    v_flex()
        .size_full()
        .items_center()
        .justify_center()
        .text_xs()
        .text_color(cx.theme().muted_foreground)
        .child(text)
        .into_any_element()
}

/// A sidebar entry's signal when its agents work or wait: **its own left
/// edge**, where the selection's bar is, and not a box drawn round it — a
/// dashed frame over a selected band spoke two languages at once. Working,
/// the edge glows in the tone of work and a bright run goes down it;
/// waiting, it breathes in the tone of a question. `inset` keeps it off the
/// ends of an entry with rounded corners.
fn outline_of(doing: Option<&Doing>, theme: &gpui_kit::component::Theme) -> Option<AnyElement> {
    edge_signal(doing, 0., theme)
}

pub(super) fn edge_signal(
    doing: Option<&Doing>,
    inset: f32,
    theme: &gpui_kit::component::Theme,
) -> Option<AnyElement> {
    let doing = doing.copied().filter(|doing| *doing != Doing::Rest)?;
    let (work, asks) = (theme.warning, theme.danger);
    let seconds = super::overview_view::flow_seconds();
    Some(
        canvas(
            move |_, _, _| {},
            move |bounds, _, window, _| {
                let (x, y) = (bounds.origin.x, bounds.origin.y);
                let (w, h) = (bounds.size.width, bounds.size.height);
                let bar = |top: f32, bottom: f32, color: gpui_kit::Hsla| {
                    gpui_kit::fill(
                        gpui_kit::Bounds::new(
                            point(x, y + px(top)),
                            gpui_kit::size(w, px(bottom - top)),
                        ),
                        color,
                    )
                    .corner_radii(w / 2.)
                };
                let height = f32::from(h);
                match doing {
                    Doing::Waiting => {
                        let breath = overview::breath(seconds, super::overview_view::PULSE_PERIOD);
                        window.paint_quad(bar(0., height, asks.opacity(0.35 + 0.65 * breath)));
                    }
                    _ => {
                        window.paint_quad(bar(0., height, work.opacity(0.3)));
                        if let Some((top, bottom)) = focus::edge_run(height, seconds) {
                            window.paint_quad(bar(top, bottom, work));
                        }
                    }
                }
            },
        )
        .absolute()
        .left_0()
        .top(px(inset))
        .bottom(px(inset))
        .w(px(3.))
        .into_any_element(),
    )
}

/// Whether an entry of the sidebar wears the signal — see `edge_signal`.
fn dressed(doings: &Doings, path: &Path) -> bool {
    doings.get(path).is_some_and(|doing| *doing != Doing::Rest)
}

/// The settings, at the foot of the home screen's sidebar: the gear the
/// editor's title bar carries at its far right.
fn settings_button(cx: &mut Context<ClaudhubApp>) -> Button {
    Button::new("home-settings")
        .ghost()
        .small()
        .icon(icon("settings"))
        .tooltip(tr!("workspace-settings"))
        .on_click(cx.listener(|this, _, window, cx| this.open_settings(window, cx)))
}

/// A choice between a few, as one control: a track sunk a step under the
/// card, its segments side by side — or one above the other on the rail.
/// Two buttons beside each other read as two actions; this reads as one
/// question and its answer.
fn segmented(vertical: bool, segments: Vec<AnyElement>, cx: &gpui_kit::App) -> gpui_kit::Div {
    let theme = cx.theme();
    let track = if vertical { v_flex() } else { h_flex() };
    track
        .flex_none()
        .p(px(2.))
        .gap(px(2.))
        .rounded(theme.radius)
        .bg(super::theme::gutter(cx))
        .border_1()
        .border_color(theme.border)
        .children(segments)
}

/// What is pressed inside the home screen's head keeps its press: the head
/// is the window's drag region, and a press left to bubble up to it would
/// move the window instead of reaching the button — see `topbar::actions`.
fn held(child: impl IntoElement) -> gpui_kit::Div {
    div()
        .on_mouse_down(gpui_kit::MouseButton::Left, |_, _, cx| {
            cx.stop_propagation()
        })
        .child(child)
}
