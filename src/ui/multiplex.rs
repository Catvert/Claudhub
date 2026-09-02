//! The terminal strip: every open terminal at once, and nothing else.
//!
//! It is a **screen** and not a panel, and that is the whole point. Watching
//! five agents work means watching five terminals at the same time, and a dock
//! shows one tab of a group: one either splits the area five ways by hand,
//! every time, or one reads them one after the other and misses the one that
//! stopped. So the window drops everything it usually carries — the rails, the
//! docks, the status bar — and paints tiles.
//!
//! **Columns, not a grid**, the way niri lays windows out. Every terminal is a
//! column of the full height, the columns stand side by side in a strip wider
//! than the window, and the strip scrolls sideways. A grid of nine gave each
//! terminal a third of the height, which is twelve lines of an agent's
//! transcript; a column keeps every line the window has and pays for it in
//! width, which is the axis a terminal has to spare — eighty columns is what
//! it asks for, and a column of half the window is more than that. And the
//! shape no longer depends on the count: opening a tenth terminal adds a
//! column at the end instead of reshuffling the nine one was reading.
//!
//! **The strip follows the keyboard.** Whatever puts the focus in a terminal —
//! a click on its head, `Ctrl+Tab`, the terminal one was reading when the
//! strip came up — the column is scrolled into view on the next frame. It is
//! read off the focus rather than written at each of those places: a click on
//! a terminal's own surface focuses it too, and that click is one the program
//! receives, not us.
//!
//! **Every worktree**, deliberately. What one comes here to ask is "which of
//! the agents I left running has finished", and that question does not stop at
//! the checkout being looked at. Each tile therefore says which project it
//! belongs to, which a tab bar of one worktree never has to.
//!
//! What decides the shape is pure and tested; what paints it is below.

use std::path::PathBuf;

use gpui::{
    prelude::*, px, relative, Context, Entity, Focusable as _, SharedString, WeakEntity, Window,
};
use gpui_component::{
    button::{Button, ButtonVariants as _},
    h_flex,
    menu::{DropdownMenu as _, PopupMenuItem},
    v_flex, ActiveTheme, Disableable as _, Sizable as _,
};

use super::app::ClaudhubApp;
use super::terminal_view::{Launch, TerminalView};
use crate::tr;

/// How wide a column is, as a share of the window.
///
/// **Presets and not a free width**, which is niri's choice too
/// (`switch-preset-column-width`): a column one drags to a size is a column
/// one has to drag every time, and what a terminal wants is one of three
/// things — a third to keep an eye on it, half to read it, all of it to work
/// in it. A share and not a count of pixels, so that the same strip fits the
/// same number of columns on a laptop and on a wide screen.
///
/// **Half by default.** Two terminals side by side is the shape the grid
/// started at for two, and half of any window this runs on is past the
/// eighty columns a terminal is laid out for.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Width {
    Third,
    #[default]
    Half,
    TwoThirds,
    Full,
}

impl Width {
    /// The share of the strip's width the column takes.
    pub fn fraction(self) -> f32 {
        match self {
            Width::Third => 1. / 3.,
            Width::Half => 0.5,
            Width::TwoThirds => 2. / 3.,
            Width::Full => 1.,
        }
    }

    /// The next preset, round and round: the head carries one button, and a
    /// button that cycles is the whole of the gesture — there is no width
    /// past "all of it" but the narrowest again.
    pub fn next(self) -> Self {
        match self {
            Width::Third => Width::Half,
            Width::Half => Width::TwoThirds,
            Width::TwoThirds => Width::Full,
            Width::Full => Width::Third,
        }
    }

    /// How the tooltip names it.
    pub fn label(self) -> &'static str {
        match self {
            Width::Third => "⅓",
            Width::Half => "½",
            Width::TwoThirds => "⅔",
            Width::Full => "1",
        }
    }
}

/// One end of the strip, which is what each of its two rails stands for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum End {
    Start,
    End,
}

impl End {
    /// Which way the offset moves to look towards this end: gpui's offsets
    /// are zero or negative, so looking right is going down.
    fn sign(self) -> f32 {
        match self {
            End::Start => 1.,
            End::End => -1.,
        }
    }
}

/// How far a step slides the strip when no column says: **half a column**
/// of the default preset. A whole column would replace what is on screen
/// with what was beside it, which is a page and not a step; half of one
/// keeps the column one was reading in view while the next one comes in. In
/// pixels of the window, since that is what a column is.
pub fn step(viewport_width: f32) -> f32 {
    viewport_width * Width::default().fraction() / 2.
}

/// Half of the column a step towards `end` brings in.
///
/// The columns are not all one width — a third, two thirds, the whole window
/// — and half of the default is a third of a wide column and all of a narrow
/// one. The column that measures the step is the one **entering** from that
/// side: the first past the right edge going right, the last before the left
/// edge going left. `None` when nothing stands beyond that edge, which is
/// the strip already at its end. Column edges are the unscrolled strip's, as
/// the handle reports them, and `offset` is where it is scrolled to.
pub fn half_column(
    columns: &[(f32, f32)],
    viewport: (f32, f32),
    offset: f32,
    end: End,
) -> Option<f32> {
    let (left, right) = viewport;
    let column = match end {
        End::End => columns.iter().find(|(_, stop)| stop + offset > right + 0.5),
        End::Start => columns
            .iter()
            .rev()
            .find(|(start, _)| start + offset < left - 0.5),
    }?;
    Some((column.1 - column.0) / 2.)
}

/// The offset that brings a column into view, from the one the strip is at.
///
/// gpui's own `scroll_to_item`, written out — because that one **jumps**, and
/// what this returns is handed to the wheel smoothing as a destination. The
/// rule is its rule: a column wider than the window, or starting before its
/// left edge, is put against that edge; one ending past the right edge is put
/// against that one; one already in view moves nothing. Offsets are gpui's,
/// zero or negative, and the column's edges are those of the unscrolled strip.
pub fn reveal(viewport: (f32, f32), column: (f32, f32), offset: f32) -> f32 {
    let (left, right) = viewport;
    let (start, end) = column;
    if end - start > right - left || start + offset < left {
        left - start
    } else if end + offset > right {
        right - end
    } else {
        offset
    }
}

/// Where two tiles change places, read off the order the strip walks.
///
/// A drop on the tile one picked up is a gesture that changed its mind, and a
/// tile that went away while the pointer was down — a shell that exited — is
/// one that has no place any more. Both answer `None`, which is what "do
/// nothing" is spelt as here.
pub fn exchange(order: &[u64], from: u64, to: u64) -> Option<(usize, usize)> {
    if from == to {
        return None;
    }
    let at = |wanted: u64| order.iter().position(|id| *id == wanted);
    Some((at(from)?, at(to)?))
}

/// Where a tile and its neighbour change places, one step along the strip.
///
/// niri's `move-column-left` and `-right`, and the gesture the head's two
/// chevrons make. At either end there is no neighbour on that side, and the
/// answer is `None` rather than a wrap: a column pushed off the right edge
/// reappearing on the left is a column one then goes looking for.
pub fn shift(order: &[u64], id: u64, delta: isize) -> Option<(usize, usize)> {
    let from = order.iter().position(|each| *each == id)?;
    let to = from.checked_add_signed(delta)?;
    (to < order.len()).then_some((from, to))
}

/// A tile being dragged.
///
/// The payload of the drag and the ghost that follows the pointer: a drag's
/// value has to be a type of its own, since that is what a drop listener reads
/// to know the drag is one of ours.
#[derive(Clone)]
pub struct DraggedTile {
    view: u64,
    label: SharedString,
}

impl gpui::Render for DraggedTile {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        gpui::div()
            .px_2()
            .py_1()
            .rounded(cx.theme().radius)
            .bg(cx.theme().secondary)
            .border_1()
            .border_color(cx.theme().border)
            .text_xs()
            .child(self.label.clone())
    }
}

impl ClaudhubApp {
    /// The strip, in place of the whole workspace.
    pub(super) fn render_multiplex(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        // Read once: the tiles are handed their label and their project, and a
        // closure that read the application per tile would borrow it while this
        // render still holds it.
        let tiles: Vec<Tile> = self
            .terminals
            .iter()
            .map(|terminal| Tile {
                view: terminal.view.clone(),
                label: terminal
                    .name
                    .clone()
                    .unwrap_or_else(|| terminal.view.read(cx).label()),
                project: self.project_label(&terminal.worktree),
                worktree: terminal.worktree.clone(),
                width: terminal.column,
                rank: 0,
                count: 0,
            })
            .collect();
        let count = tiles.len();
        let tiles: Vec<Tile> = tiles
            .into_iter()
            .enumerate()
            .map(|(rank, tile)| Tile {
                rank,
                count,
                ..tile
            })
            .collect();
        // **Two rails, one at each edge, and they never scroll.** A `+` at
        // the end of the strip was where the hand is once it has scrolled
        // through what is there — and nowhere else: from the first column it
        // was ten screens away. Each rail carries what its edge means — a
        // step of the strip that way, and a terminal opened at that end.
        let rails = (
            self.render_strip_rail(End::Start, cx),
            self.render_strip_rail(End::End, cx),
        );
        if tiles.is_empty() {
            return h_flex()
                .flex_1()
                .min_h_0()
                .min_w_0()
                .child(rails.0)
                .child(
                    v_flex()
                        .flex_1()
                        .h_full()
                        .items_center()
                        .justify_center()
                        .child(
                            gpui::div()
                                .text_sm()
                                .text_color(cx.theme().muted_foreground)
                                .child(tr!("multiplex-empty")),
                        ),
                )
                .child(rails.1)
                .into_any_element();
        }
        // The column under the hand is brought into view when the hand moves
        // to another one — and only then: scrolling to it on every frame would
        // pin the strip, and the wheel could never look past the focused
        // column at the ones beside it.
        let focused = tiles
            .iter()
            .position(|tile| tile.view.focus_handle(cx).contains_focused(window, cx));
        let seen = focused.map(|rank| tiles[rank].view.entity_id());
        if seen.is_some() && seen != self.multiplex_seen {
            if let Some(rank) = focused {
                self.reveal_column(rank, tiles[rank].width);
            }
        }
        self.multiplex_seen = seen;
        let scroll = self.multiplex_scroll.clone();
        // **A share of the strip's own width**, not of what it scrolls: a
        // percentage resolves against the box that clips, so half stays half
        // a window however many columns stand beyond the edge. Each column
        // refuses to shrink — that is what makes the strip overflow instead of
        // squeezing ten terminals into ten slivers.
        //
        // And the strip is a flex item of a flex column, which it has to be: it
        // asks for `size_full`, a hundred percent of the parent's **height** —
        // not of what the title bar leaves. `flex_1` on the wrapper is what
        // asks for the rest.
        let strip = h_flex()
            .id("multiplex-strip")
            .size_full()
            .overflow_x_scroll()
            .track_scroll(&scroll)
            .p(px(2.))
            .children(tiles.into_iter().map(|tile| {
                gpui::div()
                    .flex_none()
                    .h_full()
                    .w(relative(tile.width.fraction()))
                    .p(px(2.))
                    // **A notch is a step**, half a column like the rails,
                    // and not gpui's three line heights — thirty pixels of a
                    // five-hundred-pixel column, twenty notches to cross
                    // one. Taken here, on the column, which is where the
                    // event still is: the strip's own handler is the parent's,
                    // and a child's listener runs first in the bubble phase.
                    // What reaches this is a vertical notch over a head, or
                    // a sideways one the terminal let through; a trackpad's
                    // pixels are left to gpui and the smoothing's `cancel`.
                    .on_scroll_wheel(cx.listener(|this, event: &gpui::ScrollWheelEvent, _, cx| {
                        let gpui::ScrollDelta::Lines(lines) = event.delta else {
                            return;
                        };
                        let along = if lines.x != 0. { lines.x } else { lines.y };
                        if along == 0. {
                            return;
                        }
                        cx.stop_propagation();
                        // A positive notch is "up", which on this axis is
                        // towards the start: gpui adds it to the offset.
                        let end = if along > 0. { End::Start } else { End::End };
                        this.step_strip(end, cx);
                    }))
                    .child(tile.render(window, cx))
            }));
        // The same wheel smoothing as every list of the window, on the other
        // axis: a notch over a head — or a tilt of the wheel over a column,
        // which the terminal lets through — is a jump of three line heights
        // that gpui applies at once, and `scrolled` replays it as the
        // transition the docks have.
        let strip = self.scrolled(
            "multiplex-scroll",
            &scroll,
            super::motion::Axes::Horizontal,
            window,
            strip,
            cx,
        );
        h_flex()
            .flex_1()
            .min_h_0()
            .min_w_0()
            .child(rails.0)
            .child(v_flex().flex_1().h_full().min_w_0().child(strip))
            .child(rails.1)
            .into_any_element()
    }

    /// Slides the strip until a column is in view.
    ///
    /// **Through the wheel smoothing and not `scroll_to_item`**: that one
    /// writes the offset in one go, and a click on a column half off the
    /// screen made the strip teleport — the very jump the wheel no longer
    /// makes. The destination is worked out from the bounds of the previous
    /// frame, which is what the handle holds, and handed to `push` as a
    /// wheel notch of exactly that length; `scrolled` advances it from then
    /// on. The first frame of the strip has no bounds yet, and there is
    /// nothing on screen to slide from: gpui's jump is the right answer there.
    ///
    /// **A column that was not there last frame** — the one the `+` just
    /// opened — has no bounds either, and it is the case one sees most: it
    /// stands past the last column that has some, and it is as wide as its
    /// preset says, which is enough to aim at.
    fn reveal_column(&mut self, rank: usize, width: Width) {
        let handle = self.multiplex_scroll.clone();
        let viewport = handle.bounds();
        // Nothing but columns stands in the strip, and it had better stay
        // so: the `+` once stood after the last one, and a column opened at
        // the end took its rank — and its stale forty pixels for bounds, so
        // the strip revealed a sliver of it and stopped. A column opened at
        // the *start* reads the old first column's bounds, which start where
        // it does: the answer is the same, the left edge.
        let column = handle.bounds_for_item(rank).or_else(|| {
            let before = handle.bounds_for_item(rank.checked_sub(1)?)?;
            let start = before.right();
            Some(gpui::Bounds::new(
                gpui::point(start, before.top()),
                gpui::size(viewport.size.width * width.fraction(), before.size.height),
            ))
        });
        let Some(column) = column.filter(|_| viewport.size.width > px(0.)) else {
            handle.scroll_to_item(rank);
            return;
        };
        let offset = handle.offset();
        let target = reveal(
            (f32::from(viewport.left()), f32::from(viewport.right())),
            (f32::from(column.left()), f32::from(column.right())),
            f32::from(offset.x),
        );
        let delta = target - f32::from(offset.x);
        if delta.abs() < 0.5 {
            return;
        }
        // The farthest the strip can go is the previous frame's too, and a
        // column that was not there is not counted in it: the smoothing
        // clamps its destination to that bound, so the strip slid to the old
        // end and stopped a column short. The bound is stretched to what
        // showing this column needs; the next layout measures the real one,
        // which is never smaller.
        let mut max = handle.max_offset();
        max.x = max.x.max(column.right() - viewport.right());
        let next = self
            .motion(
                SharedString::from("multiplex-scroll"),
                super::motion::Axes::Horizontal,
            )
            .push(offset, gpui::point(px(delta), px(0.)), max);
        handle.set_offset(next);
    }

    /// One edge of the strip: a step that way, and a terminal at that end.
    fn render_strip_rail(&mut self, end: End, cx: &mut Context<Self>) -> impl IntoElement {
        let chevron = match end {
            End::Start => "chevron-left",
            End::End => "chevron-right",
        };
        v_flex()
            .flex_none()
            .h_full()
            .w(super::theme::bar_height(cx))
            .items_center()
            .justify_center()
            .gap_1()
            .child(
                Button::new(("multiplex-step", end as u64))
                    .ghost()
                    .small()
                    .icon(super::icons::icon(chevron))
                    .tooltip(tr!(match end {
                        End::Start => "multiplex-step-left",
                        End::End => "multiplex-step-right",
                    }))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.step_strip(end, cx);
                    })),
            )
            .child(self.new_terminal_button(end, cx))
    }

    /// Slides the strip half a column towards one end, smoothly.
    ///
    /// Through the wheel smoothing, as a notch of that length: the rail is a
    /// wheel for a hand that has none, and a step that jumped would be the
    /// one thing here that does. The column that measures the half is the
    /// one coming in (`half_column`), read off the bounds of the last frame.
    pub(super) fn step_strip(&mut self, end: End, cx: &mut Context<Self>) {
        let handle = self.multiplex_scroll.clone();
        let viewport = handle.bounds();
        let width = f32::from(viewport.size.width);
        if width <= 0. {
            return;
        }
        let columns: Vec<(f32, f32)> = (0..)
            .map_while(|rank| handle.bounds_for_item(rank))
            .map(|bounds| (f32::from(bounds.left()), f32::from(bounds.right())))
            .collect();
        let offset = f32::from(handle.offset().x);
        let half = half_column(
            &columns,
            (f32::from(viewport.left()), f32::from(viewport.right())),
            offset,
            end,
        )
        .unwrap_or_else(|| step(width));
        let delta = end.sign() * half;
        let next = self
            .motion(
                SharedString::from("multiplex-scroll"),
                super::motion::Axes::Horizontal,
            )
            .push(
                handle.offset(),
                gpui::point(px(delta), px(0.)),
                handle.max_offset(),
            );
        handle.set_offset(next);
        cx.notify();
    }

    /// The `+` of a rail: a terminal on a worktree one **names first**, at
    /// that end of the strip.
    ///
    /// The dock's `+` opens on the checkout being looked at, and the strip
    /// looks at all of them — a terminal that landed on whichever was
    /// current underneath would land on a project the screen does not single
    /// out. So the menu is the list of worktrees, and each of them the same
    /// choice the dock's `+` offers: a shell, or an agent profile.
    fn new_terminal_button(&self, end: End, cx: &mut Context<Self>) -> impl IntoElement {
        let choices: std::rc::Rc<[(PathBuf, SharedString)]> = self
            .repos
            .worktrees_in_order()
            .into_iter()
            .map(|path| {
                let (repo, label) = self.project_label(&path);
                let label = match repo {
                    Some(repo) => SharedString::from(format!("{repo} · {label}")),
                    None => label,
                };
                (path, label)
            })
            .collect();
        let app = cx.entity().downgrade();
        Button::new(("multiplex-new", end as u64))
            .ghost()
            .small()
            .icon(super::icons::icon("plus"))
            .tooltip(tr!(match end {
                End::Start => "multiplex-new-left",
                End::End => "multiplex-new-right",
            }))
            .dropdown_menu(move |menu, window, cx| {
                let profiles = super::settings::Settings::global(cx)
                    .terminal
                    .agents
                    .clone();
                choices.iter().cloned().fold(menu, |menu, (path, label)| {
                    let app = app.clone();
                    if profiles.is_empty() {
                        // No profile: naming the worktree is the whole
                        // question, and a submenu of one entry is a click
                        // for nothing.
                        return menu.item(PopupMenuItem::new(label).on_click(
                            move |_, window, cx| {
                                open_on(&app, &path, Launch::shell(), end, window, cx);
                            },
                        ));
                    }
                    let profiles = profiles.clone();
                    menu.submenu(label, window, cx, move |menu, _, _| {
                        let shell = (app.clone(), path.clone());
                        let menu = menu.item(
                            PopupMenuItem::new(tr!("terminal-new"))
                                .icon(super::icons::icon("plus"))
                                .on_click(move |_, window, cx| {
                                    open_on(&shell.0, &shell.1, Launch::shell(), end, window, cx);
                                }),
                        );
                        profiles.iter().cloned().fold(menu, |menu, profile| {
                            let (app, path) = (app.clone(), path.clone());
                            let label = SharedString::from(profile.label().to_string());
                            menu.item(
                                PopupMenuItem::new(label)
                                    .icon(super::icons::icon("bot"))
                                    .on_click(move |_, window, cx| {
                                        open_on(
                                            &app,
                                            &path,
                                            Launch::agent(&profile),
                                            end,
                                            window,
                                            cx,
                                        );
                                    }),
                            )
                        })
                    })
                })
            })
    }

    /// Moves a column one step along the strip.
    pub(super) fn move_tile(&mut self, view: u64, delta: isize, cx: &mut Context<Self>) {
        let order: Vec<u64> = self
            .terminals
            .iter()
            .map(|terminal| terminal.view.entity_id().as_u64())
            .collect();
        let Some((from, to)) = shift(&order, view, delta) else {
            return;
        };
        self.terminals.swap(from, to);
        cx.notify();
    }

    /// Gives a column its next preset width.
    pub(super) fn cycle_column(&mut self, view: gpui::EntityId, cx: &mut Context<Self>) {
        if let Some(terminal) = self
            .terminals
            .iter_mut()
            .find(|terminal| terminal.view.entity_id() == view)
        {
            terminal.column = terminal.column.next();
            cx.notify();
        }
    }
}

/// Opens a terminal on a named worktree, from a menu entry.
///
/// The strip stays up: the new column is the answer being looked at, and it
/// takes the focus, so the strip slides to it on the next frame.
///
/// **The focus is given a second time, deferred.** `open_terminal` focuses
/// the new terminal, and the menu this runs from then closes and hands the
/// focus back to whatever had it before it opened — the terminal one was in.
/// The strip then saw no focus change, and the new column stood off screen.
/// The same race as a dialog's field (`dialogs::focus_field`), and the same
/// answer.
///
/// At the **start** of the strip when the left rail asks: `open_terminal`
/// appends, which is the right rail's meaning, and the new one is moved to
/// the front afterwards — it reorders `terminals` itself, as the chevrons of
/// a head do, so `Ctrl+Tab` walks the same order the eye does.
fn open_on(
    app: &WeakEntity<ClaudhubApp>,
    worktree: &std::path::Path,
    launch: Launch,
    end: End,
    window: &mut Window,
    cx: &mut gpui::App,
) {
    let Some(app) = app.upgrade() else {
        return;
    };
    let opened = app.update(cx, |app, cx| {
        app.open_terminal(worktree, launch, window, cx);
        let view = app.terminals.last().map(|terminal| terminal.view.clone());
        if end == End::Start {
            if let Some(opened) = app.terminals.pop() {
                app.terminals.insert(0, opened);
            }
        }
        view
    });
    if let Some(view) = opened {
        super::dialogs::focus_field(&view, window, cx);
    }
}

/// One terminal as the strip shows it.
struct Tile {
    view: Entity<TerminalView>,
    label: SharedString,
    /// The repository and the checkout, as the picker writes them.
    project: (Option<SharedString>, SharedString),
    worktree: PathBuf,
    width: Width,
    /// Its place along the strip, and how many there are: what says whether
    /// there is a neighbour on either side to change places with.
    rank: usize,
    count: usize,
}

impl Tile {
    fn render(self, window: &mut Window, cx: &mut Context<ClaudhubApp>) -> gpui::AnyElement {
        let focused = self.view.focus_handle(cx).contains_focused(window, cx);
        let id = self.view.entity_id();
        let (repo, checkout) = self.project;
        let muted = cx.theme().muted_foreground;
        let worktree = self.worktree.clone();
        let go = self.worktree.clone();
        let label = self.label.clone();
        let dropped = id.as_u64();
        v_flex()
            // An id, because a drop target has to have one — and the whole
            // tile is the target rather than its head: a strip of twenty-odd
            // pixels is a thing one misses, and there is nothing else a tile
            // can mean as a destination.
            .id(("multiplex-tile", id))
            .size_full()
            .min_w_0()
            .min_h_0()
            .rounded(cx.theme().radius_lg)
            .overflow_hidden()
            .bg(cx.theme().background)
            .border_1()
            .drag_over::<DraggedTile>(|style, _, _, cx| style.border_color(cx.theme().ring))
            .on_drop(cx.listener(move |this, dragged: &DraggedTile, _, cx| {
                this.swap_tiles(dragged.view, dropped, cx);
            }))
            // The one under the hand is outlined, and it has to be: five shells
            // side by side look alike, and what one types goes to exactly one
            // of them.
            .border_color(if focused {
                cx.theme().ring
            } else {
                cx.theme().border
            })
            .child(
                h_flex()
                    .id(("multiplex-head", id))
                    .flex_none()
                    .h(super::theme::bar_height(cx))
                    .w_full()
                    .px_2()
                    .gap_1()
                    .items_center()
                    .bg(cx.theme().secondary)
                    .text_xs()
                    // The head is what one picks a tile up by, and only the
                    // head: everything below it is a screen the program owns,
                    // where dragging is how one selects text.
                    .cursor_grab()
                    .on_drag(
                        DraggedTile {
                            view: id.as_u64(),
                            label,
                        },
                        |tile, _, _, cx| {
                            let tile = tile.clone();
                            cx.new(|_| tile)
                        },
                    )
                    // The head selects: it is the part of a tile with nothing
                    // in it, and clicking a terminal's own surface is a click
                    // the program receives.
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.focus_tile(id, &worktree, window, cx);
                    }))
                    .children(
                        repo.map(|repo| gpui::div().flex_none().text_color(muted).child(repo)),
                    )
                    .child(gpui::div().flex_none().text_color(muted).child(checkout))
                    .child(
                        gpui::div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .child(self.label.clone()),
                    )
                    // One step left, one step right: the gesture that changes
                    // the order, in the place the dragged head only promises
                    // it. A button at the end of the strip is greyed rather
                    // than absent, so the pair keeps its place in the head.
                    .child(
                        Button::new(("multiplex-left", id))
                            .ghost()
                            .small()
                            .icon(super::icons::icon("chevron-left"))
                            .tooltip(tr!("multiplex-move-left"))
                            .disabled(self.rank == 0)
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.move_tile(dropped, -1, cx);
                            })),
                    )
                    .child(
                        Button::new(("multiplex-right", id))
                            .ghost()
                            .small()
                            .icon(super::icons::icon("chevron-right"))
                            .tooltip(tr!("multiplex-move-right"))
                            .disabled(self.rank + 1 >= self.count)
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.move_tile(dropped, 1, cx);
                            })),
                    )
                    // The width, one press per preset. Its own button and not
                    // a drag on the column's edge: the edge is where the next
                    // column starts, and a handle there is what the grid had —
                    // a size one sets again every time.
                    .child(
                        Button::new(("multiplex-width", id))
                            .ghost()
                            .small()
                            .icon(super::icons::icon("columns-2"))
                            .tooltip(tr!("multiplex-width", { width: self.width.label() }))
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.cycle_column(id, cx);
                            })),
                    )
                    // The way out, tile by tile. One comes to the strip to find
                    // out which of the agents has finished; acting on that
                    // answer means going to that checkout and leaving the strip,
                    // and without this the screen could only be left the way
                    // one came in — by the title bar, landing wherever the
                    // window happened to be. An *external link* and not an
                    // arrow: beside two chevrons that move the column, an
                    // arrow read as a third move, and what this one does is
                    // leave the screen.
                    .child(
                        Button::new(("multiplex-go", id))
                            .ghost()
                            .small()
                            .icon(super::icons::icon("external-link"))
                            .tooltip(tr!("multiplexer-open-worktree"))
                            .on_click(cx.listener(move |this, _, window, cx| {
                                this.work_in_worktree(&go, window, cx);
                            })),
                    ),
            )
            // **`v_flex` and not `div`**, for the other half of the same
            // trap: a `div` is a *block*, the terminal under it asks for
            // `size_full`, and a full height of an undefined height is zero.
            .child(v_flex().flex_1().min_h_0().child(self.view))
            .into_any_element()
    }
}

impl ClaudhubApp {
    /// Turns the strip on and off.
    ///
    /// The keyboard is handed over both ways, and it is not a nicety: what had
    /// the focus is no longer painted after either switch, and a focus on
    /// something the window does not show is a window where no key does
    /// anything. Coming in, the terminal one was reading keeps it if it is
    /// there; failing that the last one of the checkout on show, which is the
    /// one the `+` and the shortcuts have been opening.
    ///
    /// Coming in also forgets which column the strip last revealed: the one
    /// under the hand is brought into view on the first frame, wherever the
    /// strip had been left.
    pub(super) fn toggle_multiplex(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.multiplex = !self.multiplex;
        if !self.multiplex {
            self.focus_handle(cx).focus(window, cx);
            cx.notify();
            return;
        }
        self.multiplex_seen = None;
        let focused = self
            .terminals
            .iter()
            .any(|terminal| terminal.view.focus_handle(cx).contains_focused(window, cx));
        if !focused {
            let landing = self
                .active
                .clone()
                .and_then(|worktree| self.terminals_of(&worktree).last().map(|t| t.view.clone()))
                .or_else(|| self.terminals.last().map(|t| t.view.clone()));
            if let Some(view) = landing {
                window.focus(&view.focus_handle(cx), cx);
            }
        }
        cx.notify();
    }

    /// Puts two tiles in each other's place.
    ///
    /// It reorders `self.terminals`, which is *the* order of the terminals —
    /// the one `Ctrl+Tab` steps through in a terminal as well. There is no second list to
    /// keep in step, and there had better not be: two orders for the same
    /// terminals is two answers to "the next one".
    ///
    /// The dock's tabs do not follow, and cannot: this strip mixes the
    /// worktrees, and their tab bars are one per checkout.
    pub(super) fn swap_tiles(&mut self, from: u64, to: u64, cx: &mut Context<Self>) {
        let order: Vec<u64> = self
            .terminals
            .iter()
            .map(|terminal| terminal.view.entity_id().as_u64())
            .collect();
        let Some((from, to)) = exchange(&order, from, to) else {
            return;
        };
        self.terminals.swap(from, to);
        cx.notify();
    }

    /// Hands the keyboard to one tile, and files where it came from.
    ///
    /// The panel is activated too, though nothing shows it while the strip is
    /// on: leaving the strip then lands on the terminal one was last reading,
    /// rather than on whichever tab the dock happened to hold. And the worktree
    /// follows for the same reason — the window comes back looking at the
    /// project one was watching.
    pub(super) fn focus_tile(
        &mut self,
        view: gpui::EntityId,
        worktree: &std::path::Path,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(terminal) = self
            .terminals
            .iter()
            .find(|terminal| terminal.view.entity_id() == view)
        else {
            return;
        };
        let (view, panel) = (terminal.view.clone(), terminal.panel.clone());
        super::panels::TerminalPanel::activate(&panel, window, cx);
        window.focus(&view.focus_handle(cx), cx);
        if self.active.as_deref() != Some(worktree) {
            self.select_worktree(worktree.to_path_buf(), window, cx);
        }
        cx.notify();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Half a window, so that two stand side by side out of the box: the
    /// shape the grid started at for two terminals, and past the eighty
    /// columns a terminal is laid out for on any window this runs on.
    #[test]
    fn a_column_starts_at_half_the_window() {
        assert_eq!(Width::default(), Width::Half);
        assert_eq!(Width::default().fraction(), 0.5);
    }

    /// The presets go round: the head has one button, and the width past
    /// "all of it" is the narrowest again.
    #[test]
    fn the_presets_cycle_through_all_four_and_come_back() {
        let mut width = Width::Third;
        let mut seen = vec![width];
        for _ in 0..3 {
            width = width.next();
            seen.push(width);
        }
        assert_eq!(
            seen,
            vec![Width::Third, Width::Half, Width::TwoThirds, Width::Full]
        );
        assert_eq!(width.next(), Width::Third);
    }

    /// Wider at every step, and never past the window: a column that
    /// overflowed the strip would be one whose right edge cannot be reached.
    #[test]
    fn each_preset_is_wider_than_the_last_and_none_exceeds_the_window() {
        let mut width = Width::Third;
        for _ in 0..3 {
            let next = width.next();
            assert!(next.fraction() > width.fraction(), "{width:?} → {next:?}");
            assert!(next.fraction() <= 1.);
            width = next;
        }
    }

    /// Half of a default column, in pixels of the window.
    #[test]
    fn a_step_is_half_a_default_column() {
        assert_eq!(step(1000.), 250.);
    }

    /// The step measures the column coming in, whatever its width: a
    /// two-thirds column past the right edge is worth a third of the window.
    #[test]
    fn a_step_is_half_the_column_entering_from_that_side() {
        let viewport = (0., 1000.);
        let columns = [(0., 500.), (500., 1000.), (1000., 1666.), (1666., 2666.)];
        // Going right from the start: the two-thirds column is next.
        assert_eq!(half_column(&columns, viewport, 0., End::End), Some(333.));
        // Nothing before the left edge yet.
        assert_eq!(half_column(&columns, viewport, 0., End::Start), None);
        // Scrolled six hundred: the second half-column is cut on the left,
        // and it is what a step left brings back.
        assert_eq!(
            half_column(&columns, viewport, -600., End::Start),
            Some(250.)
        );
        // And going right from there, the full-width column is the one
        // past the edge only once the two-thirds one is entirely in.
        assert_eq!(half_column(&columns, viewport, -666., End::End), Some(500.));
        // At the end, nothing more comes in.
        assert_eq!(half_column(&columns, viewport, -1666., End::End), None);
    }

    /// gpui's rule for bringing a child into view, on the strip's axis: the
    /// window is 0 to 1000, the strip is scrolled 400 to the left.
    #[test]
    fn a_column_is_revealed_the_way_gpui_reveals_it() {
        let viewport = (0., 1000.);
        // In view already: nothing moves.
        assert_eq!(reveal(viewport, (500., 1000.), -400.), -400.);
        // Past the right edge: its end is put against that edge.
        assert_eq!(reveal(viewport, (1000., 1500.), -400.), -500.);
        // Before the left edge: its start is put against that edge.
        assert_eq!(reveal(viewport, (0., 500.), -400.), 0.);
        // Wider than the window: the start wins, as gpui decides.
        assert_eq!(reveal(viewport, (500., 2000.), -400.), -500.);
    }

    /// A step swaps with the neighbour on that side, and stops at the ends
    /// rather than wrapping.
    #[test]
    fn a_step_swaps_with_the_neighbour_and_stops_at_the_ends() {
        let order = [10, 20, 30];
        assert_eq!(shift(&order, 20, -1), Some((1, 0)));
        assert_eq!(shift(&order, 20, 1), Some((1, 2)));
        assert_eq!(shift(&order, 10, -1), None);
        assert_eq!(shift(&order, 30, 1), None);
        // A shell that exited under the button.
        assert_eq!(shift(&order, 99, 1), None);
    }

    /// Two tiles change places; anything else changes nothing.
    #[test]
    fn a_drop_exchanges_the_two_tiles() {
        let order = [10, 20, 30];
        assert_eq!(exchange(&order, 10, 30), Some((0, 2)));
        assert_eq!(exchange(&order, 30, 10), Some((2, 0)));
        // Dropped on itself: a gesture that changed its mind.
        assert_eq!(exchange(&order, 20, 20), None);
        // A shell that exited while the pointer was down.
        assert_eq!(exchange(&order, 99, 20), None);
        assert_eq!(exchange(&order, 20, 99), None);
    }
}
