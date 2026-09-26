//! The home screen's focus view, its default: a sidebar of every worktree on
//! show, and one of them at a time in the middle, its cards on a board.
//!
//! The plane answers « where does each worktree stand », the columns « what
//! is each doing », and both pay for it in room: five worktrees share one
//! screen. Most of a day is spent in one worktree with a glance at the
//! others, which is what an editor's layout is — a list to choose from on
//! the left, what was chosen filling the rest. The sidebar is that list, and
//! it says of each worktree what one glances at: whether an agent works in
//! it, how much it has in progress, how many terminals it has open.
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
//! review and the title bar all speak of it already.

use std::path::{Path, PathBuf};

use gpui_kit::component::{
    button::{Button, ButtonVariants as _},
    h_flex,
    menu::DropdownMenu as _,
    v_flex, ActiveTheme, Sizable as _,
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
use crate::ui::overview::{self, Node};

/// The sidebar's width.
const SIDEBAR_WIDTH: f32 = 260.;
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

/// A card on its way to another place.
#[derive(Clone, Debug)]
pub(super) struct FocusDrag {
    pub node: Node,
    pub from: Point<Pixels>,
    pub at: Point<Pixels>,
    /// Past the slop: a drag, and no longer a click.
    pub moving: bool,
}

impl ClaudhubApp {
    /// The focus view: the sidebar, and the worktree on show.
    pub(super) fn render_overview_focus(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let (_, at_work) = self.prepare_laid_out(cx);
        let gap = window.rem_size() * 0.75;
        let sidebar = self.render_focus_sidebar(cx);
        let shown = self.focus_worktree();
        let main: AnyElement = match (shown, self.overview_zoomed.clone()) {
            // A maximised node fills the middle, the sidebar staying: it is
            // how one goes elsewhere.
            (_, Some(node)) => v_flex()
                .size_full()
                .child(self.column_node(&node, false, &at_work, window, cx))
                .into_any_element(),
            (None, None) => v_flex()
                .size_full()
                .items_center()
                .justify_center()
                .text_sm()
                .text_color(cx.theme().muted_foreground)
                .child(tr!("overview-empty"))
                .into_any_element(),
            (Some(path), None) => {
                // Its list of changes, even if the pickers leave it out of
                // what the home screen reads.
                self.ensure_changes_read(std::slice::from_ref(&path), cx);
                self.render_focus_board(&path, gap, &at_work, window, cx)
            }
        };
        let dragging = self.focus_drag.as_ref().is_some_and(|drag| drag.moving);
        h_flex()
            .id("overview")
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
            .into_any_element()
    }

    /// The worktree the middle shows: the one on show if there is one, else
    /// the first of the projects on show.
    fn focus_worktree(&self) -> Option<PathBuf> {
        self.active.clone().or_else(|| {
            self.overview_repos()
                .into_iter()
                .find_map(|repo| self.overview_worktrees_of(repo).into_iter().next())
        })
    }

    // — The sidebar ————————————————————————————————————————————————

    /// Every worktree of the projects on show, under its project's name —
    /// the one on show lit, as a list's selection is.
    fn render_focus_sidebar(&self, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme().clone();
        let mut rows: Vec<AnyElement> = Vec::new();
        for repo in self.overview_repos() {
            let main = repo.main.clone();
            rows.push(
                h_flex()
                    .w_full()
                    .px_2()
                    .pt_3()
                    .pb_1()
                    .gap_1p5()
                    .items_center()
                    .child(icon("git-merge").text_color(theme.muted_foreground))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .text_sm()
                            .font_weight(gpui_kit::FontWeight::SEMIBOLD)
                            .child(SharedString::from(repo.name.clone())),
                    )
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
                    )
                    .into_any_element(),
            );
            for path in self.overview_worktrees_of(repo) {
                rows.push(self.render_focus_row(&path, cx));
            }
        }
        let list = v_flex()
            .id("focus-sidebar-list")
            .track_scroll(&self.focus_sidebar_scroll)
            .size_full()
            .pb_2()
            .overflow_y_scroll()
            .children(rows);
        v_flex()
            .flex_none()
            .w(px(SIDEBAR_WIDTH))
            .h_full()
            .overflow_hidden()
            .rounded(theme.radius_lg)
            .border_1()
            .border_color(theme.border)
            .bg(theme.background)
            .child(super::scroll::vertical(
                "focus-sidebar-bar",
                &self.focus_sidebar_scroll,
                list,
            ))
            .into_any_element()
    }

    /// A worktree in the sidebar: its name and branch, who works in it, how
    /// many terminals it has, and how much it has in progress. A click shows
    /// it; twice goes to work in it, as a card's double click does.
    fn render_focus_row(&self, path: &Path, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme().clone();
        let (_, label) = self.project_label(path);
        let branch = self
            .repos
            .worktree(path)
            .and_then(|worktree| worktree.branch.clone())
            .filter(|branch| branch.as_str() != label.as_ref())
            .map(SharedString::from);
        let lit = self.focus_worktree().as_deref() == Some(path);
        let agent = self.agents.get(path).cloned();
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
        let open = path.to_path_buf();
        h_flex()
            .id(SharedString::from(format!("focus-row-{}", path.display())))
            .w_full()
            .px_2()
            .py_1()
            .gap_2()
            .items_center()
            .cursor_pointer()
            // The selection is a band, and a bar at its edge says it louder
            // than a tint alone — the sidebar is read at a glance.
            .border_l_2()
            .border_color(if lit {
                theme.ring
            } else {
                gpui_kit::transparent_black()
            })
            .when(lit, |el| el.bg(theme.list_active))
            .when(!lit, |el| el.hover(|style| style.bg(theme.list_hover)))
            .on_click(
                cx.listener(move |this, event: &gpui_kit::ClickEvent, window, cx| {
                    if event.click_count() >= 2 {
                        this.work_in_worktree(&open, window, cx);
                    } else if this.active.as_deref() != Some(open.as_path()) {
                        this.overview_zoomed = None;
                        this.select_worktree(open.clone(), window, cx);
                    }
                }),
            )
            .child(icon("git-branch").xsmall().text_color(if lit {
                theme.ring
            } else {
                theme.muted_foreground
            }))
            .child(
                v_flex()
                    .flex_1()
                    .min_w_0()
                    .child(
                        div()
                            .truncate()
                            .text_sm()
                            .when(lit, |el| el.font_weight(gpui_kit::FontWeight::SEMIBOLD))
                            .child(label),
                    )
                    .children(branch.map(|branch| {
                        div()
                            .truncate()
                            .text_xs()
                            .text_color(theme.muted_foreground)
                            .child(branch)
                    })),
            )
            .children(agent.map(|agent| super::topbar::agent_dot(&agent, cx)))
            .when(terminals > 0, |el| {
                el.child(
                    h_flex()
                        .flex_none()
                        .gap_0p5()
                        .items_center()
                        .text_xs()
                        .text_color(theme.muted_foreground)
                        .child(icon("square-terminal").xsmall())
                        .child(SharedString::from(terminals.to_string())),
                )
            })
            .children(summary.map(|summary| super::topbar::volume_on(summary, None, cx)))
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

    /// The worktree on show: its title, and its board.
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
        while self.focus_column_scrolls.len() < shown.len() {
            self.focus_column_scrolls
                .push(gpui_kit::ScrollHandle::new());
        }
        // Where a drop would land, read against what the last frame
        // measured, the room it opened taken back out — before this frame
        // measures anew.
        let drag = self.focus_drag.clone().filter(|drag| drag.moving);
        let count = shown.len();
        let list_gap = f32::from(window.rem_size() * 0.5);
        let (aim, room_height) = match &drag {
            Some(drag) => self.focus_aim(drag, &shown),
            None => (None, 0.),
        };
        // The room's height is what it is painted at — it grows as it opens,
        // and its own measure writes it here — so that the drop is always
        // read against the cards as they would stand without it.
        let opened = aim
            .filter(|target| target.column < count)
            .map(|target| (target.column, target.index, 0.));
        *self.focus_geometry.borrow_mut() = focus::Geometry {
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
        let mut columns: Vec<AnyElement> = Vec::new();
        for (rank, (board_column, cards)) in shown.into_iter().enumerate() {
            let room = opened
                .filter(|(column, _, _)| *column == rank)
                .map(|(_, index, _)| (index, room_height, list_gap));
            columns.push(
                self.render_focus_column(
                    path,
                    rank,
                    board_column,
                    cards,
                    FocusColumn {
                        drag: drag.as_ref(),
                        room,
                        least: column_min,
                        width: self
                            .focus_boards
                            .get(path)
                            .and_then(|board| board.width(board_column)),
                        gap,
                    },
                    at_work,
                    window,
                    cx,
                ),
            );
        }
        let new_column = aim.is_some_and(|target| target.column >= count);
        if let (true, Some(drag)) = (new_column, drag.as_ref()) {
            columns.push(self.render_new_column_room(drag, column_min, cx));
        }
        columns.push(self.render_add_column(path, new_column, cx));

        let row = h_flex()
            .id("focus-board")
            .track_scroll(&self.focus_scroll)
            .size_full()
            .gap(gap)
            .overflow_x_scroll()
            // The wheel scrolls what it is over, up and down: a row that
            // scrolls only sideways would otherwise turn every notch that
            // misses a column into a slide of the whole board. Sideways is
            // the bar's, or a sideways wheel's.
            .restrict_scroll_to_axis()
            .children(columns);
        v_flex()
            .size_full()
            .child(
                h_flex()
                    .flex_none()
                    .items_center()
                    .gap_2()
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .child(self.worktree_title(path, cx)),
                    )
                    .child(
                        div()
                            .flex_none()
                            .pb_2()
                            .text_xs()
                            .text_color(cx.theme().muted_foreground)
                            .child(tr!("focus-drag-hint")),
                    ),
            )
            .child(div().flex_1().min_h_0().child(super::scroll::both(
                "focus-board-bar",
                &self.focus_scroll,
                row,
            )))
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
    ) -> (Option<Target>, f32) {
        let geometry = self.focus_geometry.borrow();
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
            let measured = self.focus_geometry.clone();
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
        let measured = self.focus_geometry.clone();
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
        let scroll = self.focus_column_scrolls[rank].clone();
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
            .width_grip(board_column, rank, cx)
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
        board_column: usize,
        rank: usize,
        cx: &mut Context<Self>,
    ) -> gpui_kit::Stateful<gpui_kit::Div> {
        let theme = cx.theme().clone();
        let held = self
            .focus_resize
            .is_some_and(|(column, _, _)| column == board_column);
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
                        .borrow()
                        .spans
                        .get(rank)
                        .map(|span| span.right - span.left)
                        .unwrap_or(0.);
                    if drawn > 0. {
                        this.focus_resize =
                            Some((board_column, f32::from(event.position.x), drawn));
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
                    if let Some(path) = this.focus_worktree() {
                        if let Some(board) = this.focus_boards.get_mut(&path) {
                            board.set_width(board_column, None);
                        }
                        this.remember_focus_board(&path, cx);
                    }
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
    pub(super) fn focus_grab(&mut self, node: Node, at: Point<Pixels>) {
        self.focus_drag = Some(FocusDrag {
            node,
            from: at,
            at,
            moving: false,
        });
    }

    fn focus_dragged(&mut self, at: Point<Pixels>, cx: &mut Context<Self>) {
        // A column's edge, held: its width follows the pointer, never under
        // the least the settings give.
        if let Some((column, from, start)) = self.focus_resize {
            let least = super::settings::Settings::global(cx).terminal.column_min;
            let width = (start + f32::from(at.x) - from).max(least);
            if let Some(path) = self.focus_worktree() {
                if let Some(board) = self.focus_boards.get_mut(&path) {
                    board.set_width(column, Some(width));
                }
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
        if self.focus_resize.take().is_some() {
            if let Some(path) = self.focus_worktree() {
                self.remember_focus_board(&path, cx);
            }
            cx.notify();
            return;
        }
        let Some(drag) = self.focus_drag.take() else {
            return;
        };
        if !drag.moving {
            return;
        }
        let Some(path) = self.focus_worktree() else {
            return;
        };
        let target = {
            let closed = self.focus_geometry.borrow().closed();
            focus::drop_target(&closed, (f32::from(drag.at.x), f32::from(drag.at.y)))
        };
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
        let measured = self.focus_geometry.clone();
        let measure = canvas(
            move |_, _, _| {},
            move |bounds, _, _, _| {
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
