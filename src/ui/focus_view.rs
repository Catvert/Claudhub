//! The home screen's focus view, its default: a sidebar of every worktree on
//! show, and the ones chosen in it in the middle, side by side, each one's
//! cards on its board.
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
//! **The middle is a board** (`ui::focus`, pure): columns of cards the hand
//! arranges. A card is dragged by its head — the head every node already
//! has — and dropped in another column, at a rank, or right of the last
//! column, which makes a new one. While it travels, the card stays in place,
//! faded, a chip follows the pointer, and a bar says where it would land:
//! nothing moves under the hand until it lets go, so what one aims at stays
//! where it was aimed at. Its arrangement is kept per worktree.
//!
//! **The worktree's card carries its changes**: on this board they are one
//! card, what the branch is and what it is about to say. A terminal alone at
//! the foot of a column takes the rest of its height — a column of
//! terminals is read as one — and a column that holds a terminal or a note
//! ends with a button that stacks another of it; the tall button right of the last column
//! opens one in a column of its own. A column's right edge is taken to give
//! it a width, never under the least the settings give; a double click
//! lets it fill again.
//!
//! **Which worktree is the one on show** — `active`, the window's: choosing
//! one here is choosing it for the editor too, and the branch picker, the
//! review and the title bar all speak of it already. The others shown beside
//! it are only shown.
//!
//! **What a board holds in the moment is its own** — its geometry, its
//! columns' scrolls, the column being widened — keyed by worktree: two
//! boards side by side measure two sets of columns, and a card only ever
//! moves within its own.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use gpui_kit::component::{
    button::{Button, ButtonVariants as _},
    h_flex,
    menu::DropdownMenu as _,
    v_flex, ActiveTheme, Disableable as _, Sizable as _,
};
use gpui_kit::{
    anchored, canvas, deferred, div, point, prelude::*, px, Animation, AnimationExt as _,
    AnyElement, Context, Pixels, Point, SharedString, Window,
};

use crate::tr;
use crate::ui::app::ClaudhubApp;
use crate::ui::canvas_view::Hang;
use crate::ui::focus::{self, Board, ColumnSpan, Target};
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
/// The button right of the last column.
const ADD_COLUMN_WIDTH: f32 = 44.;
/// How far the pointer travels before a press on a head is a drag: a click
/// on a head that trembled is still a click.
const DRAG_SLOP: f32 = 5.;

/// The room opened for a drop: never smaller than a card's head and a
/// line, never taller than a card one can still see past.
const ROOM_HEIGHT: (f32, f32) = (56., 180.);
/// The widest the column a drop would make is drawn.
const NEW_COLUMN_ROOM: f32 = 320.;

/// How long the room for a drop takes to open, a card to land, and the
/// chip to come under the pointer: long enough to be seen as movement,
/// short enough never to be waited for.
const ROOM_OPENS: std::time::Duration = std::time::Duration::from_millis(180);
const CARD_LANDS: std::time::Duration = std::time::Duration::from_millis(380);
const CHIP_SHOWS: std::time::Duration = std::time::Duration::from_millis(120);

/// Fast out, slow in: what moves arrives, it does not stop.
fn ease_out(t: f32) -> f32 {
    1. - (1. - t).powi(3)
}

/// What a column of the board is told besides its cards.
struct FocusColumn<'a> {
    /// The card on its way, if one is.
    drag: Option<&'a FocusDrag>,
    /// The room to open for it here: before which card, how tall, and the
    /// gap that follows it.
    room: Option<(usize, f32, f32)>,
    /// The least width of a column, from the settings.
    least: f32,
    /// The width the hand gave it; `None` fills.
    width: Option<f32>,
    /// The gap between two columns, which its grip is centred in.
    gap: Pixels,
}

/// Where the boards stood at the last paint, for the header over them —
/// each board's edges in the row's own coordinates, what the window saw
/// less how far the row was scrolled — so that the header, drawn before the
/// row, can place each title against the scroll of **this** frame.
#[derive(Default)]
pub(super) struct BoardsLaidOut {
    /// The header's left edge and width, in the window.
    strip: (f32, f32),
    /// How far the row was scrolled when the boards were measured.
    painted: f32,
    /// Each board's left and right edges in the row.
    boards: HashMap<PathBuf, (f32, f32)>,
}

/// A card on its way to another place.
#[derive(Clone, Debug)]
pub(super) struct FocusDrag {
    /// The board it was taken from, and the only one it is dropped on: a
    /// repository's note stands on every board at once.
    pub board: PathBuf,
    pub node: Node,
    pub from: Point<Pixels>,
    pub at: Point<Pixels>,
    /// Past the slop: a drag, and no longer a click.
    pub moving: bool,
}

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
        // The boards gone take what they held in the moment with them.
        self.focus_geometry.retain(|path, _| shown.contains(path));
        self.focus_column_scrolls
            .retain(|path, _| shown.contains(path));
        self.focus_laid_out
            .borrow_mut()
            .boards
            .retain(|path, _| shown.contains(path));
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
                let alone = shown.len() == 1;
                // An arrow's slide goes on where the last frame left it.
                let scroll = self.focus_scroll.clone();
                self.motion(BOARD_BAR.into(), Axes::Both)
                    .advance(&scroll, window);
                let header = self.render_focus_header(&shown, alone, window, cx);
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
                    // row. Sideways is the bar's, a sideways wheel's, or the
                    // header's arrows.
                    .restrict_scroll_to_axis()
                    .overflow_x_scroll()
                    .children(boards);
                // The header stays: the titles over the row do not scroll
                // away with it — see `render_focus_header`.
                v_flex()
                    .size_full()
                    .gap_2()
                    .child(header)
                    .child(div().flex_1().min_h_0().child(super::scroll::both(
                        BOARD_BAR,
                        &self.focus_scroll,
                        row,
                    )))
                    .into_any_element()
            }
        };
        let dragging = self.focus_drag.as_ref().is_some_and(|drag| drag.moving);
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
            .when(dragging, |el| el.cursor_grabbing())
            // A node's height is dragged from its bottom edge, and a card
            // from its head: both are followed from here — the pointer
            // leaves what it pressed at once.
            .on_mouse_move(
                cx.listener(|this, event: &gpui_kit::MouseMoveEvent, _, cx| {
                    this.overview_dragged(event, cx);
                    this.focus_dragged(event.position, cx);
                }),
            )
            .capture_any_mouse_up(cx.listener(|this, _, _, cx| {
                this.end_overview_drag(cx);
                this.focus_dropped(cx);
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
            .into_any_element()
    }

    // — The board ——————————————————————————————————————————————————

    /// What of a worktree is on its board: its card, its terminals, and its
    /// notes and the repository's that are not hidden.
    fn focus_present(&self, path: &Path) -> Vec<Node> {
        let hidden = &self.overview_hand.hidden;
        let groups = self.overview_groups();
        let group = groups
            .iter()
            .find(|group| group.checkouts.iter().any(|c| c.path == path));
        let checkout = group.and_then(|group| group.checkouts.iter().find(|c| c.path == path));
        let mut present = vec![Node::Worktree(path.to_path_buf())];
        present.extend(
            self.terminals
                .iter()
                .filter(|terminal| terminal.worktree == path)
                .map(|terminal| Node::Terminal(terminal.view.entity_id().as_u64())),
        );
        present.extend(
            checkout
                .map(|checkout| checkout.notes.clone())
                .into_iter()
                .flatten()
                .chain(group.map(|group| group.notes.clone()).into_iter().flatten())
                .map(Node::Note)
                .filter(|node| !hidden.contains(node)),
        );
        present
    }

    /// Brings a worktree's board in line with what it has: read back from
    /// the store the first time, then new cards placed by rule — a terminal
    /// in the column whose button asked for it.
    fn settle_focus_board(&mut self, path: &Path, cx: &mut Context<Self>) {
        let present = self.focus_present(path);
        let board = self
            .focus_boards
            .entry(path.to_path_buf())
            .or_insert_with(|| {
                let (keys, widths) = super::store::Store::global(cx)
                    .worktrees
                    .get(path)
                    .map(|state| (state.focus_board.clone(), state.focus_widths.clone()))
                    .unwrap_or_default();
                Board::from_keys(path, &keys, &widths)
            });
        let asked = |wish: &Option<(PathBuf, usize)>| {
            wish.as_ref()
                .filter(|(worktree, _)| worktree == path)
                .map(|(_, column)| *column)
        };
        let pending = focus::Pending {
            terminal: asked(&self.focus_pending),
            note: asked(&self.focus_pending_note),
        };
        let count = |board: &Board, terminal: bool| {
            board
                .columns
                .iter()
                .flatten()
                .filter(|slot| match slot {
                    focus::Slot::Card(Node::Terminal(_)) => terminal,
                    focus::Slot::Card(Node::Note(_)) => !terminal,
                    _ => false,
                })
                .count()
        };
        let (terminals_before, notes_before) = (count(board, true), count(board, false));
        let mut changed = board.settle(&present, pending);
        // Every terminal kept from the last session has come back, or never
        // will: the places left held are nobody's.
        if self.terminals_revived.contains(path) {
            changed |= board.drop_vacants();
        }
        if changed {
            // What was asked for has come: the next one goes by rule.
            if count(board, true) > terminals_before && pending.terminal.is_some() {
                self.focus_pending = None;
            }
            if count(board, false) > notes_before && pending.note.is_some() {
                self.focus_pending_note = None;
            }
            self.remember_focus_board(path, cx);
        }
    }

    fn remember_focus_board(&self, path: &Path, cx: &mut Context<Self>) {
        let Some((keys, widths)) = self
            .focus_boards
            .get(path)
            .map(|board| (board.keys(), board.widths()))
        else {
            return;
        };
        super::store::Store::update_global(cx, |store| {
            let state = store.worktrees.entry(path.to_path_buf()).or_default();
            state.focus_board = keys;
            state.focus_widths = widths;
        });
    }

    /// The line over the boards, which does not scroll with them: each
    /// board's title — its name, its branch, « Edit » — held at the left of
    /// the part of it in view, and at the right the arrows that slide the
    /// row a column at a time. `alone`, it says how a card is moved.
    ///
    /// **It is drawn before the row, from what the row measured**: each
    /// board's edges in the row's own coordinates, which scrolling does not
    /// change, set against the scroll of this frame — so a title keeps up
    /// with the slide instead of trailing it by a frame.
    fn render_focus_header(
        &mut self,
        shown: &[PathBuf],
        alone: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let height = super::theme::toolbar_height(cx);
        let at = -f32::from(self.focus_scroll.offset().x);
        let max = f32::from(self.focus_scroll.max_offset().x).max(0.);
        // The window's buttons sit over the top-right corner, and the arrows
        // before them: the titles have the rest.
        let corner = if Self::draws_window_buttons(window) {
            super::topbar::WINDOW_BUTTON_WIDTH * 3.
        } else {
            0.
        };
        let overflows = max > 0.5;
        let arrows_width = if overflows { 72. } else { 0. };
        let (strip, boards) = {
            let laid = self.focus_laid_out.borrow();
            (laid.strip, laid.boards.clone())
        };
        let room = strip.1 - corner - arrows_width;
        let mut cells: Vec<AnyElement> = Vec::new();
        for path in shown {
            // Before the row has been measured — the first frame — a lone
            // board's title takes the line; the others wait the one frame.
            let span = match boards.get(path) {
                Some(board) => focus::header_span(*board, at, room),
                None if shown.len() == 1 => Some((0., room.max(0.))),
                None => None,
            };
            let Some((left, right)) = span else {
                continue;
            };
            let open = path.to_path_buf();
            let edit = Button::new(SharedString::from(format!("focus-edit-{}", path.display())))
                .ghost()
                .small()
                .icon(icon("pencil"))
                .label(tr!("focus-edit"))
                .tooltip(tr!("overview-open-worktree"))
                .on_click(cx.listener(move |this, _, window, cx| {
                    this.work_in_worktree(&open, window, cx);
                }));
            cells.push(
                h_flex()
                    .absolute()
                    .top_0()
                    .left(px(left))
                    .w(px(right - left))
                    .h_full()
                    .gap_2()
                    .items_center()
                    .overflow_hidden()
                    .child(div().flex_1().min_w_0().child(self.worktree_title(
                        path,
                        Some(edit.into_any_element()),
                        cx,
                    )))
                    .when(alone, |el| {
                        el.child(
                            div()
                                .flex_none()
                                .pr_2()
                                .text_xs()
                                .text_color(cx.theme().muted_foreground)
                                .child(tr!("focus-drag-hint")),
                        )
                    })
                    .into_any_element(),
            );
        }
        let arrows = overflows.then(|| {
            h_flex()
                .absolute()
                .top_0()
                .right(px(corner))
                .h_full()
                .gap_1()
                .items_center()
                .child(
                    Button::new("focus-scroll-back")
                        .ghost()
                        .small()
                        .icon(icon("arrow-left"))
                        .tooltip(tr!("focus-column-back"))
                        .disabled(at <= 0.5)
                        .on_click(cx.listener(|this, _, _, cx| this.slide_focus(false, cx))),
                )
                .child(
                    Button::new("focus-scroll-forward")
                        .ghost()
                        .small()
                        .icon(icon("arrow-right"))
                        .tooltip(tr!("focus-column-forward"))
                        .disabled(at >= max - 0.5)
                        .on_click(cx.listener(|this, _, _, cx| this.slide_focus(true, cx))),
                )
        });
        let laid = self.focus_laid_out.clone();
        let measure = canvas(
            move |_, _, _| {},
            move |bounds, _, window, _| {
                let seen = (f32::from(bounds.origin.x), f32::from(bounds.size.width));
                let mut laid = laid.borrow_mut();
                if laid.strip != seen {
                    laid.strip = seen;
                    window.refresh();
                }
            },
        )
        .absolute()
        .size_full();
        div()
            .relative()
            .flex_none()
            .w_full()
            .h(height)
            .child(measure)
            .children(cells)
            .children(arrows)
            .into_any_element()
    }

    /// The row slid a column on, or back — smoothly, as a notch of the wheel
    /// is: the same motion, keyed by the row's bar. The columns' edges are
    /// the ones the last frame painted, taken into the row's coordinates.
    fn slide_focus(&mut self, forward: bool, cx: &mut Context<Self>) {
        let (strip_left, painted) = {
            let laid = self.focus_laid_out.borrow();
            (laid.strip.0, laid.painted)
        };
        let mut lefts: Vec<f32> = Vec::new();
        for path in self.focus_worktrees() {
            if let Some(geometry) = self.focus_geometry.get(&path) {
                lefts.extend(
                    geometry
                        .borrow()
                        .spans
                        .iter()
                        .map(|span| span.left - strip_left - painted),
                );
            }
        }
        let offset = self.focus_scroll.offset();
        let max = self.focus_scroll.max_offset();
        let at = -f32::from(offset.x);
        let target = focus::next_stop(&lefts, at, f32::from(max.x), forward);
        let next = self.motion(BOARD_BAR.into(), Axes::Both).push(
            offset,
            point(px(at - target), px(0.)),
            max,
        );
        self.focus_scroll.set_offset(next);
        cx.notify();
    }

    /// A worktree on show: its board, its title being the header's.
    ///
    /// **A board is as wide as its share, never narrower than its columns**
    /// — each at the width the hand gave it or the least the settings give,
    /// the gaps and the button after the last: the row of boards scrolls,
    /// never a board alone.
    fn render_focus_board(
        &mut self,
        path: &Path,
        gap: Pixels,
        at_work: &overview::AtWork,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        self.settle_focus_board(path, cx);
        let shown = self
            .focus_boards
            .get(path)
            .map(Board::shown)
            .unwrap_or_default();
        let scrolls = self
            .focus_column_scrolls
            .entry(path.to_path_buf())
            .or_default();
        while scrolls.len() < shown.len() {
            scrolls.push(gpui_kit::ScrollHandle::new());
        }
        let geometry = self
            .focus_geometry
            .entry(path.to_path_buf())
            .or_default()
            .clone();
        // Where a drop would land, read against what the last frame
        // measured, the room it opened taken back out — before this frame
        // measures anew. Only on the board the card was taken from.
        let drag = self
            .focus_drag
            .clone()
            .filter(|drag| drag.moving && drag.board == path);
        let count = shown.len();
        let list_gap = f32::from(window.rem_size() * 0.5);
        let (aim, room_height) = match &drag {
            Some(drag) => self.focus_aim(drag, &shown, &geometry),
            None => (None, 0.),
        };
        // The room's height is what it is painted at — it grows as it opens,
        // and its own measure writes it here — so that the drop is always
        // read against the cards as they would stand without it.
        let opened = aim
            .filter(|target| target.column < count)
            .map(|target| (target.column, target.index, 0.));
        *geometry.borrow_mut() = focus::Geometry {
            spans: shown
                .iter()
                .map(|(_, cards)| ColumnSpan {
                    left: 0.,
                    right: 0.,
                    cards: vec![(0., 0.); cards.len()],
                })
                .collect(),
            opened,
        };

        let column_min = super::settings::Settings::global(cx).terminal.column_min;
        let widths: Vec<Option<f32>> = shown
            .iter()
            .map(|(board_column, _)| {
                self.focus_boards
                    .get(path)
                    .and_then(|board| board.width(*board_column))
            })
            .collect();
        let least_width = widths
            .iter()
            .map(|width| width.map_or(column_min, |width| width.max(column_min)))
            .sum::<f32>()
            + f32::from(gap) * count as f32
            + ADD_COLUMN_WIDTH;
        let mut columns: Vec<AnyElement> = Vec::new();
        for (rank, (board_column, cards)) in shown.into_iter().enumerate() {
            let room = opened
                .filter(|(column, _, _)| *column == rank)
                .map(|(_, index, _)| (index, room_height, list_gap));
            columns.push(self.render_focus_column(
                path,
                rank,
                board_column,
                cards,
                FocusColumn {
                    drag: drag.as_ref(),
                    room,
                    least: column_min,
                    width: widths[rank],
                    gap,
                },
                at_work,
                window,
                cx,
            ));
        }
        let new_column = aim.is_some_and(|target| target.column >= count);
        if let (true, Some(drag)) = (new_column, drag.as_ref()) {
            columns.push(self.render_new_column_room(drag, column_min, cx));
        }
        columns.push(self.render_add_column(path, new_column, cx));

        let row = h_flex()
            .flex_1()
            .min_h_0()
            .w_full()
            .gap(gap)
            .children(columns);
        v_flex()
            .relative()
            // Its own id, under which its columns' ids are told apart from
            // the next board's.
            .id(SharedString::from(format!(
                "focus-board-{}",
                path.display()
            )))
            .flex_1()
            .min_w(px(least_width))
            .h_full()
            // Where it stands, for the header's title over it.
            .child({
                let (laid, scroll, board) = (
                    self.focus_laid_out.clone(),
                    self.focus_scroll.clone(),
                    path.to_path_buf(),
                );
                canvas(
                    move |_, _, _| {},
                    move |bounds, _, window, _| {
                        let mut laid = laid.borrow_mut();
                        let at = f32::from(scroll.offset().x);
                        let left = laid.strip.0;
                        let edges = (
                            f32::from(bounds.origin.x) - left - at,
                            f32::from(bounds.origin.x + bounds.size.width) - left - at,
                        );
                        laid.painted = at;
                        // Only a board that moved in the row asks again:
                        // scrolling moves none of them.
                        let moved = laid.boards.get(&board).is_none_or(|was| {
                            (was.0 - edges.0).abs() > 0.5 || (was.1 - edges.1).abs() > 0.5
                        });
                        if moved {
                            laid.boards.insert(board.clone(), edges);
                            window.refresh();
                        }
                    },
                )
                .absolute()
                .size_full()
            })
            .child(row)
            .children(drag.map(|drag| self.render_drag_ghost(&drag, aim, count, cx)))
            .into_any_element()
    }

    /// Where the card being dragged would land — `None` where it would not
    /// move — and the height of the room to open for it: its own, within
    /// reason, so that the room reads as the card's place.
    fn focus_aim(
        &self,
        drag: &FocusDrag,
        shown: &[(usize, Vec<(usize, Node)>)],
        geometry: &std::rc::Rc<std::cell::RefCell<focus::Geometry>>,
    ) -> (Option<Target>, f32) {
        let geometry = geometry.borrow();
        let closed = geometry.closed();
        let target = focus::drop_target(&closed, (f32::from(drag.at.x), f32::from(drag.at.y)));
        let from = shown.iter().enumerate().find_map(|(column, (_, cards))| {
            cards
                .iter()
                .position(|(_, node)| *node == drag.node)
                .map(|index| (column, index))
        });
        let height = from
            .and_then(|(column, index)| closed.get(column)?.cards.get(index).copied())
            .map_or(ROOM_HEIGHT.0, |(top, bottom)| bottom - top)
            .clamp(ROOM_HEIGHT.0, ROOM_HEIGHT.1);
        // Just above or just below itself, in its own column: it stays.
        let stays = matches!(
            (target, from),
            (Some(target), Some((column, index)))
                if target.column == column && (target.index == index || target.index == index + 1)
        );
        (target.filter(|_| !stays), height)
    }

    /// One column of the board: its cards, a terminal at its foot taking
    /// the rest of the height, and the button that stacks another terminal
    /// when it holds one.
    #[allow(clippy::too_many_arguments)]
    fn render_focus_column(
        &mut self,
        path: &Path,
        rank: usize,
        board_column: usize,
        cards: Vec<(usize, Node)>,
        column: FocusColumn,
        at_work: &overview::AtWork,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let FocusColumn {
            drag,
            room,
            least,
            width,
            gap,
        } = column;
        let geometry = self.focus_geometry[path].clone();
        let last = cards.len().saturating_sub(1);
        let holds_terminal = cards
            .iter()
            .any(|(_, node)| matches!(node, Node::Terminal(_)));
        let holds_note = cards.iter().any(|(_, node)| matches!(node, Node::Note(_)));
        let mut elements: Vec<AnyElement> = Vec::new();
        let count = cards.len();
        for (index, (_, node)) in cards.into_iter().enumerate() {
            if let (Some((at, height, gap)), Some(drag)) = (room, drag) {
                if at == index {
                    elements.push(self.render_drop_room(drag, (rank, at), height, gap, cx));
                }
            }
            let fills = index == last
                && matches!(node, Node::Terminal(_))
                && !self.overview_hand.collapsed.contains(&node);
            let faded = drag.is_some_and(|drag| drag.node == node);
            let card = self.render_focus_card(&node, fills, at_work, window, cx);
            let measured = geometry.clone();
            let measure = canvas(
                move |_, _, _| {},
                move |bounds, _, _, _| {
                    if let Some(span) = measured.borrow_mut().spans.get_mut(rank) {
                        if let Some(card) = span.cards.get_mut(index) {
                            *card = (
                                f32::from(bounds.origin.y),
                                f32::from(bounds.origin.y + bounds.size.height),
                            );
                        }
                    }
                },
            )
            .absolute()
            .size_full();
            elements.push(
                div()
                    .relative()
                    .w_full()
                    .map(|el| {
                        if fills {
                            el.flex_1().min_h(px(overview::MIN_COLUMN_TILE))
                        } else {
                            el.flex_none()
                        }
                    })
                    // The card on its way stays in place, faded: nothing
                    // moves under the hand until it lets go.
                    .when(faded, |el| el.opacity(0.35))
                    .child(card)
                    .child(measure)
                    .map(|el| self.landing(el, &node, cx)),
            );
        }
        if let (Some((at, height, gap)), Some(drag)) = (room, drag) {
            if at >= count {
                elements.push(self.render_drop_room(drag, (rank, at), height, gap, cx));
            }
        }
        // One more of what the column holds, at its foot: a terminal where
        // terminals are, a note where notes are — side by side when it
        // holds both.
        if holds_terminal || holds_note {
            let terminal = holds_terminal.then(|| {
                let worktree = path.to_path_buf();
                Button::new(("focus-stack-terminal", rank))
                    .ghost()
                    .small()
                    .flex_1()
                    .icon(icon("plus"))
                    .label(tr!("focus-stack-terminal"))
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.focus_pending = Some((worktree.clone(), board_column));
                        this.open_focus_terminal(&worktree, window, cx);
                    }))
            });
            let note = holds_note.then(|| {
                let worktree = path.to_path_buf();
                Button::new(("focus-stack-note", rank))
                    .ghost()
                    .small()
                    .flex_1()
                    .icon(icon("plus"))
                    .label(tr!("focus-stack-note"))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.focus_pending_note = Some((worktree.clone(), board_column));
                        this.add_home_note(Hang::Worktree(worktree.clone()), cx);
                    }))
            });
            elements.push(
                h_flex()
                    .flex_none()
                    .w_full()
                    .gap_1()
                    .children(terminal)
                    .children(note)
                    .into_any_element(),
            );
        }
        let measured = geometry.clone();
        let measure = canvas(
            move |_, _, _| {},
            move |bounds, _, _, _| {
                if let Some(span) = measured.borrow_mut().spans.get_mut(rank) {
                    span.left = f32::from(bounds.origin.x);
                    span.right = f32::from(bounds.origin.x + bounds.size.width);
                }
            },
        )
        .absolute()
        .size_full();
        let gutter = super::theme::scroll_gutter();
        let scroll = self.focus_column_scrolls[path][rank].clone();
        let list = v_flex()
            .id(("focus-column-list", rank))
            .track_scroll(&scroll)
            .size_full()
            .gap_2()
            .pb_2()
            .pr(gutter)
            .overflow_y_scroll()
            .children(elements);
        let grip = self
            .width_grip(path, board_column, rank, cx)
            .right(-super::overview_view::grip_offset(gap, &scroll));
        div()
            .relative()
            .h_full()
            // A width of its own is kept, never under the least; without
            // one the column shares what the others leave.
            .map(|el| match width {
                Some(width) => el.flex_none().w(px(width.max(least))),
                None => el.flex_1().min_w(px(least)),
            })
            .child(measure)
            .child(super::scroll::vertical(
                SharedString::from(format!("focus-column-bar-{rank}")),
                &scroll,
                list,
            ))
            .child(grip)
            .into_any_element()
    }

    /// A column's right edge, taken to give it a width: a strip in the gap
    /// after it, lit under the pointer and while it is held. A double click
    /// lets the column fill again — the one gesture back to where it started.
    fn width_grip(
        &self,
        path: &Path,
        board_column: usize,
        rank: usize,
        cx: &mut Context<Self>,
    ) -> gpui_kit::Stateful<gpui_kit::Div> {
        let theme = cx.theme().clone();
        let held = self
            .focus_resize
            .as_ref()
            .is_some_and(|(board, column, _, _)| board == path && *column == board_column);
        let (pressed, reset) = (path.to_path_buf(), path.to_path_buf());
        let lit = theme.ring.opacity(0.6);
        div()
            .id(("focus-width-grip", rank))
            .absolute()
            .top_0()
            .bottom_0()
            .w(super::overview_view::GRIP_WIDTH)
            .rounded_full()
            .cursor(gpui_kit::CursorStyle::ResizeLeftRight)
            .when(held, |el| el.bg(theme.ring))
            .hover(move |style| style.bg(lit))
            .tooltip(|window, cx| {
                gpui_kit::component::tooltip::Tooltip::new(tr!("overview-column-width-hint"))
                    .build(window, cx)
            })
            .on_mouse_down(
                gpui_kit::MouseButton::Left,
                cx.listener(move |this, event: &gpui_kit::MouseDownEvent, _, cx| {
                    cx.stop_propagation();
                    // It starts from the width it is drawn at: a column that
                    // fills has no width of its own yet.
                    let drawn = this
                        .focus_geometry
                        .get(&pressed)
                        .and_then(|geometry| {
                            let geometry = geometry.borrow();
                            let span = geometry.spans.get(rank)?;
                            Some(span.right - span.left)
                        })
                        .unwrap_or(0.);
                    if drawn > 0. {
                        this.focus_resize = Some((
                            pressed.clone(),
                            board_column,
                            f32::from(event.position.x),
                            drawn,
                        ));
                        cx.notify();
                    }
                }),
            )
            .on_click(
                cx.listener(move |this, event: &gpui_kit::ClickEvent, _, cx| {
                    if event.click_count() < 2 {
                        return;
                    }
                    this.focus_resize = None;
                    if let Some(board) = this.focus_boards.get_mut(&reset) {
                        board.set_width(board_column, None);
                    }
                    this.remember_focus_board(&reset, cx);
                    cx.notify();
                }),
            )
    }

    /// A card of the board. The worktree's is its own height — its changes
    /// are in it — the others the one the hand gave them, with a grip at
    /// their foot; a terminal that `fills` takes the rest of its column.
    fn render_focus_card(
        &self,
        node: &Node,
        fills: bool,
        at_work: &overview::AtWork,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        match node {
            Node::Worktree(path) => {
                let folded = self.overview_hand.collapsed.contains(node);
                let doing = at_work
                    .cards
                    .get(path)
                    .copied()
                    .unwrap_or(overview::Doing::Rest);
                div()
                    .relative()
                    .w_full()
                    .when(folded, |el| el.h(px(overview::HEAD)).overflow_hidden())
                    .child(self.render_worktree_card(path, 1., cx))
                    .when(doing != overview::Doing::Rest, |el| {
                        el.child(super::overview_view::tile_outline(
                            doing,
                            false,
                            1.,
                            cx.theme(),
                        ))
                    })
                    .into_any_element()
            }
            Node::Terminal(id) if fills => {
                let Some(terminal) = self
                    .terminals
                    .iter()
                    .find(|terminal| terminal.view.entity_id().as_u64() == *id)
                else {
                    return div().into_any_element();
                };
                let doing = at_work
                    .terminals
                    .get(id)
                    .copied()
                    .unwrap_or(overview::Doing::Rest);
                div()
                    .size_full()
                    .child(self.render_tile(terminal, None, 1., doing, window, cx))
                    .into_any_element()
            }
            _ => self.column_node(node, true, at_work, window, cx),
        }
    }

    /// The tall button right of the last column: a terminal in a column of
    /// its own, and under its chevron the rest of what can be added. It is
    /// also where a card is dropped to make a new column, and lights up
    /// when one would land there.
    fn render_add_column(&self, path: &Path, lit: bool, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme().clone();
        let worktree = path.to_path_buf();
        let app = cx.entity().downgrade();
        let hang = Hang::Worktree(path.to_path_buf());
        v_flex()
            .flex_none()
            .w(px(ADD_COLUMN_WIDTH))
            .h_full()
            .gap_1()
            .child(
                v_flex()
                    .id("focus-add-column")
                    .flex_1()
                    .w_full()
                    .items_center()
                    .justify_center()
                    .gap_2()
                    .rounded(theme.radius_lg)
                    .border_1()
                    .border_color(if lit { theme.ring } else { theme.border })
                    .when(lit, |el| el.bg(theme.ring.opacity(0.12)))
                    .text_color(if lit {
                        theme.ring
                    } else {
                        theme.muted_foreground
                    })
                    .cursor_pointer()
                    .hover(|style| style.bg(theme.list_hover))
                    .tooltip(|window, cx| {
                        gpui_kit::component::tooltip::Tooltip::new(tr!("focus-new-column"))
                            .build(window, cx)
                    })
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.focus_pending = None;
                        this.open_focus_terminal(&worktree, window, cx);
                    }))
                    .child(icon("plus"))
                    .child(icon("square-terminal")),
            )
            .child(
                Button::new("focus-add-more")
                    .ghost()
                    .small()
                    .w_full()
                    .icon(icon("chevron-down"))
                    .tooltip(tr!("overview-add"))
                    .dropdown_menu(move |menu, _, cx| {
                        super::overview_view::add_items(&app, &hang, false, menu, cx)
                    }),
            )
            .into_any_element()
    }

    /// Opens a shell in the worktree and hands it the keyboard, from a
    /// gesture of the board.
    fn open_focus_terminal(
        &mut self,
        worktree: &Path,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.open_terminal(worktree, super::terminal_view::Launch::shell(), window, cx);
        if let Some(view) = self.terminals.last().map(|terminal| terminal.view.clone()) {
            super::dialogs::focus_field(&view, window, cx);
        }
    }

    // — Dragging a card —————————————————————————————————————————————

    /// A press on a card's head, on the focus board: a drag, once it moves.
    ///
    /// Its board is the one under the press: a repository's note stands on
    /// every board, and only where it was taken says which it moves on.
    pub(super) fn focus_grab(&mut self, node: Node, at: Point<Pixels>) {
        let x = f32::from(at.x);
        let under = self
            .focus_geometry
            .iter()
            .find(|(_, geometry)| geometry.borrow().covers(x))
            .map(|(path, _)| path.clone());
        let own = match &node {
            Node::Worktree(path) | Node::Changes(path) => Some(path.clone()),
            Node::Terminal(id) => self
                .terminals
                .iter()
                .find(|terminal| terminal.view.entity_id().as_u64() == *id)
                .map(|terminal| terminal.worktree.clone()),
            Node::Note(_) | Node::Git(_) => None,
        };
        let Some(board) = under
            .or(own)
            .or_else(|| self.focus_worktrees().into_iter().next())
        else {
            return;
        };
        self.focus_drag = Some(FocusDrag {
            board,
            node,
            from: at,
            at,
            moving: false,
        });
    }

    fn focus_dragged(&mut self, at: Point<Pixels>, cx: &mut Context<Self>) {
        // A column's edge, held: its width follows the pointer, never under
        // the least the settings give.
        if let Some((path, column, from, start)) = self.focus_resize.clone() {
            let least = super::settings::Settings::global(cx).terminal.column_min;
            let width = (start + f32::from(at.x) - from).max(least);
            if let Some(board) = self.focus_boards.get_mut(&path) {
                board.set_width(column, Some(width));
            }
            cx.notify();
            return;
        }
        let Some(drag) = self.focus_drag.as_mut() else {
            return;
        };
        drag.at = at;
        let travelled = (f32::from(at.x - drag.from.x)).hypot(f32::from(at.y - drag.from.y));
        if travelled > DRAG_SLOP {
            drag.moving = true;
        }
        if drag.moving {
            cx.notify();
        }
    }

    /// The card let go: moved where the bar said, and the board kept.
    fn focus_dropped(&mut self, cx: &mut Context<Self>) {
        // A column's edge let go: its width is kept.
        if let Some((path, ..)) = self.focus_resize.take() {
            self.remember_focus_board(&path, cx);
            cx.notify();
            return;
        }
        let Some(drag) = self.focus_drag.take() else {
            return;
        };
        if !drag.moving {
            return;
        }
        let path = drag.board.clone();
        let target = self.focus_geometry.get(&path).and_then(|geometry| {
            let closed = geometry.borrow().closed();
            focus::drop_target(&closed, (f32::from(drag.at.x), f32::from(drag.at.y)))
        });
        if let (Some(target), Some(board)) = (target, self.focus_boards.get_mut(&path)) {
            let target = board.resolve(target);
            board.move_card(&drag.node, target);
            let count = self.focus_landed.as_ref().map_or(0, |(_, count)| count + 1);
            self.focus_landed = Some((drag.node.clone(), count));
            self.remember_focus_board(&path, cx);
        }
        cx.notify();
    }

    /// The room opened where the card would land: its height, a dashed
    /// frame in the accent, and its name — the card's place, before it is
    /// there.
    fn render_drop_room(
        &self,
        drag: &FocusDrag,
        (rank, index): (usize, usize),
        height: f32,
        gap: f32,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let theme = cx.theme().clone();
        let (glyph, name) = self.card_name(&drag.node, cx);
        let measured = self.focus_geometry.get(&drag.board).cloned();
        let measure = canvas(
            move |_, _, _| {},
            move |bounds, _, _, _| {
                let Some(measured) = &measured else {
                    return;
                };
                if let Some(opened) = measured.borrow_mut().opened.as_mut() {
                    opened.2 = f32::from(bounds.size.height) + gap;
                }
            },
        )
        .absolute()
        .size_full();
        h_flex()
            .relative()
            .flex_none()
            .w_full()
            .overflow_hidden()
            .items_center()
            .justify_center()
            .gap_2()
            .rounded(theme.radius_lg)
            .border_2()
            .border_dashed()
            .border_color(theme.ring.opacity(0.8))
            .bg(theme.ring.opacity(0.08))
            .text_sm()
            .text_color(theme.ring)
            .child(measure)
            .child(icon(glyph).small())
            .child(div().max_w(px(260.)).truncate().child(name))
            // It opens rather than appears: the cards below slide down to
            // make it, and a place that moves reads as a place.
            .with_animation(
                SharedString::from(format!("focus-room-{rank}-{index}")),
                Animation::new(ROOM_OPENS).with_easing(ease_out),
                move |el, t| el.h(px(height * t)).opacity(0.4 + 0.6 * t),
            )
            .into_any_element()
    }

    /// A card just dropped, on its new place: it comes down the last few
    /// pixels and a ring around it fades — the eye is taken to where it
    /// went. Every other card is left as it is.
    fn landing(&self, card: gpui_kit::Div, node: &Node, cx: &mut Context<Self>) -> AnyElement {
        let Some((_, count)) = self
            .focus_landed
            .as_ref()
            .filter(|(landed, _)| landed == node)
        else {
            return card.into_any_element();
        };
        let (ring, radius) = (cx.theme().ring, cx.theme().radius_lg);
        card.with_animation(
            SharedString::from(format!("focus-landed-{count}")),
            Animation::new(CARD_LANDS).with_easing(ease_out),
            move |el, t| {
                el.top(px(-14. * (1. - t))).opacity(0.5 + 0.5 * t).child(
                    div()
                        .absolute()
                        .inset_0()
                        .rounded(radius)
                        .border_2()
                        .border_color(ring.opacity(1. - t)),
                )
            },
        )
        .into_any_element()
    }

    /// The column a drop right of the last one would make, drawn before it
    /// is made: nothing to its left moves.
    fn render_new_column_room(
        &self,
        drag: &FocusDrag,
        width: f32,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let theme = cx.theme().clone();
        let (glyph, _) = self.card_name(&drag.node, cx);
        let room = width.min(NEW_COLUMN_ROOM);
        v_flex()
            .flex_none()
            .overflow_hidden()
            .h_full()
            .items_center()
            .justify_center()
            .gap_2()
            .rounded(theme.radius_lg)
            .border_2()
            .border_dashed()
            .border_color(theme.ring.opacity(0.8))
            .bg(theme.ring.opacity(0.06))
            .text_sm()
            .text_color(theme.ring)
            .child(icon(glyph).small())
            .child(tr!("focus-drop-new-column"))
            .with_animation(
                "focus-new-column-room",
                Animation::new(ROOM_OPENS).with_easing(ease_out),
                move |el, t| el.w(px(room * t)).opacity(0.4 + 0.6 * t),
            )
            .into_any_element()
    }

    /// What travels with the pointer: a card in miniature — its glyph, its
    /// name, and where it would land — over everything.
    fn render_drag_ghost(
        &self,
        drag: &FocusDrag,
        aim: Option<Target>,
        columns: usize,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let theme = cx.theme().clone();
        let (glyph, name) = self.card_name(&drag.node, cx);
        let whither = match aim {
            None => tr!("focus-drop-stays"),
            Some(target) if target.column >= columns => tr!("focus-drop-new-column"),
            Some(target) => tr!("focus-drop-column", { n: target.column + 1 }),
        };
        let chip = v_flex()
            .w(px(240.))
            .overflow_hidden()
            .rounded(theme.radius_lg)
            .border_1()
            .border_color(theme.ring)
            .bg(theme.background)
            .shadow_lg()
            .child(
                h_flex()
                    .px_2()
                    .py_1()
                    .gap_1p5()
                    .items_center()
                    .bg(theme.ring.opacity(0.14))
                    .text_sm()
                    .font_weight(gpui_kit::FontWeight::SEMIBOLD)
                    .child(icon(glyph).xsmall().text_color(theme.ring))
                    .child(div().flex_1().min_w_0().truncate().child(name)),
            )
            .child(
                h_flex()
                    .px_2()
                    .py_1()
                    .gap_1()
                    .items_center()
                    .text_xs()
                    .text_color(theme.muted_foreground)
                    .child(icon("arrow-right").xsmall())
                    .child(whither),
            );
        let chip = chip.with_animation(
            "focus-drag-chip",
            Animation::new(CHIP_SHOWS).with_easing(ease_out),
            |el, t| el.opacity(t),
        );
        deferred(
            anchored()
                .position(drag.at + point(px(16.), px(12.)))
                .child(chip),
        )
        .with_priority(2)
        .into_any_element()
    }

    /// What a card is called on the chip that carries it.
    fn card_name(&self, node: &Node, cx: &Context<Self>) -> (&'static str, SharedString) {
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
            Node::Git(_) | Node::Changes(_) => ("file-diff", tr!("overview-changes")),
        }
    }
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

fn edge_signal(
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
