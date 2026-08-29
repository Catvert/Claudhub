//! The terminal grid: every open terminal at once, and nothing else.
//!
//! It is a **screen** and not a panel, and that is the whole point. Watching
//! five agents work means watching five terminals at the same time, and a dock
//! shows one tab of a group: one either splits the area five ways by hand,
//! every time, or one reads them one after the other and misses the one that
//! stopped. So the window drops everything it usually carries — the rails, the
//! docks, the status bar — and paints tiles.
//!
//! **Every worktree**, deliberately. What one comes here to ask is "which of
//! the agents I left running has finished", and that question does not stop at
//! the checkout being looked at. Each tile therefore says which project it
//! belongs to, which a tab bar of one worktree never has to.
//!
//! What decides the shape is pure and tested; what paints it is below.

use std::path::PathBuf;

use gpui::{prelude::*, px, Context, Entity, Focusable as _, SharedString, Window};
use gpui_component::{h_flex, v_flex, ActiveTheme};

use super::app::ClaudhubApp;
use super::terminal_view::TerminalView;
use crate::tr;

/// How many tiles each row of the grid carries.
///
/// As square as the count allows, then **wider than tall**: a terminal is
/// eighty columns of text, and a tall thin one wraps every command it is given.
/// So the columns are the ceiling of the square root and the rows follow — four
/// terminals make two by two, five make three then two.
///
/// The remainder is **spread** rather than left at the end: five tiles as 3 and
/// 2 reads as a grid, where 3 and 1 and a hole reads as something broken. The
/// fuller rows come first, which is where the eye starts.
pub fn rows(count: usize) -> Vec<usize> {
    if count == 0 {
        return Vec::new();
    }
    let columns = (count as f64).sqrt().ceil() as usize;
    let rows = count.div_ceil(columns);
    let (each, extra) = (count / rows, count % rows);
    (0..rows)
        .map(|row| each + usize::from(row < extra))
        .collect()
}

impl ClaudhubApp {
    /// The grid, in place of the whole workspace.
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
            })
            .collect();
        if tiles.is_empty() {
            return v_flex()
                .flex_1()
                .items_center()
                .justify_center()
                .child(
                    gpui::div()
                        .text_sm()
                        .text_color(cx.theme().muted_foreground)
                        .child(tr!("multiplex-empty")),
                )
                .into_any_element();
        }
        let mut rest = tiles.into_iter();
        v_flex()
            .flex_1()
            .min_h_0()
            .gap(px(4.))
            .p(px(4.))
            .children(rows(rest.len()).into_iter().map(|width| {
                // **A bare flex row and not `h_flex`**, which centres its
                // children: a centred tile takes the height of its content
                // instead of the row's, so the grid came out as a strip of
                // header floating in the middle of an empty window.
                gpui::div()
                    .flex()
                    .flex_row()
                    .flex_1()
                    .min_h_0()
                    .gap(px(4.))
                    .children(
                        rest.by_ref()
                            .take(width)
                            .map(|tile| tile.render(window, cx))
                            .collect::<Vec<_>>(),
                    )
            }))
            .into_any_element()
    }
}

/// One terminal as the grid shows it.
struct Tile {
    view: Entity<TerminalView>,
    label: SharedString,
    /// The repository and the checkout, as the picker writes them.
    project: (Option<SharedString>, SharedString),
    worktree: PathBuf,
}

impl Tile {
    fn render(self, window: &mut Window, cx: &mut Context<ClaudhubApp>) -> gpui::AnyElement {
        let focused = self.view.focus_handle(cx).contains_focused(window, cx);
        let id = self.view.entity_id();
        let (repo, checkout) = self.project;
        let muted = cx.theme().muted_foreground;
        let worktree = self.worktree.clone();
        v_flex()
            .flex_1()
            .min_w_0()
            .min_h_0()
            .rounded(cx.theme().radius_lg)
            .overflow_hidden()
            .bg(cx.theme().background)
            .border_1()
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
    /// Turns the grid on and off.
    ///
    /// The keyboard is handed over both ways, and it is not a nicety: what had
    /// the focus is no longer painted after either switch, and a focus on
    /// something the window does not show is a window where no key does
    /// anything. Coming in, the terminal one was reading keeps it if it is
    /// there; failing that the last one of the checkout on show, which is the
    /// one the `+` and the shortcuts have been opening.
    pub(super) fn toggle_multiplex(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.multiplex = !self.multiplex;
        if !self.multiplex {
            self.focus_handle(cx).focus(window, cx);
            cx.notify();
            return;
        }
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

    /// Hands the keyboard to one tile, and files where it came from.
    ///
    /// The panel is activated too, though nothing shows it while the grid is
    /// on: leaving the grid then lands on the terminal one was last reading,
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

    #[test]
    fn a_lone_terminal_takes_the_whole_screen() {
        assert_eq!(rows(1), vec![1]);
    }

    /// Two side by side, not one above the other: eighty columns is what a
    /// terminal is for.
    #[test]
    fn two_terminals_sit_side_by_side() {
        assert_eq!(rows(2), vec![2]);
    }

    #[test]
    fn four_make_a_square() {
        assert_eq!(rows(4), vec![2, 2]);
        assert_eq!(rows(9), vec![3, 3, 3]);
    }

    /// The odd one out goes to the top row rather than leaving a hole in the
    /// bottom one.
    #[test]
    fn the_remainder_is_spread_not_left_at_the_end() {
        assert_eq!(rows(3), vec![2, 1]);
        assert_eq!(rows(5), vec![3, 2]);
        assert_eq!(rows(7), vec![3, 2, 2]);
        // Eleven is four columns and three rows, and the hole is at the end of
        // the last one — never in the middle.
        assert_eq!(rows(11), vec![4, 4, 3]);
    }

    /// Every tile is placed, exactly once: the rows are what the render walks
    /// the list with, so a count that does not add up drops a terminal or
    /// repeats one.
    #[test]
    fn every_terminal_lands_somewhere() {
        for count in 0..64 {
            assert_eq!(rows(count).iter().sum::<usize>(), count, "{count}");
        }
    }

    #[test]
    fn nothing_open_is_no_grid_at_all() {
        assert!(rows(0).is_empty());
    }
}
