//! The shortcuts help window.
//!
//! It knows no shortcut: everything comes from `shortcuts::sheet`, which reads
//! them from the very table the bindings come out of. Help written separately
//! would have diverged on the first addition, and help that lies about the keys
//! is worse than no help.
//!
//! Two columns, and the split is made on the **number of rows** rather than in
//! the middle of the list: the families are not the same size — review has four
//! times as many as the worktrees — and cutting in the middle of the table would
//! leave one column half empty.

use gpui::{div, prelude::*, px, Context, SharedString, Window};
use gpui_component::{h_flex, v_flex, ActiveTheme, StyledExt, WindowExt};

use crate::tr;
use crate::ui::app::ClaudhubApp;
use crate::ui::settings::Settings;
use crate::ui::shortcuts::{self, Section};

const WIDTH: gpui::Pixels = px(860.);
const HEIGHT: gpui::Pixels = px(560.);
/// Width reserved for the keys. Fixed, so the labels line up from one row to the
/// next: it is the column one scans by eye.
const KEYS: gpui::Pixels = px(148.);

impl ClaudhubApp {
    pub(super) fn open_shortcuts(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let vim = Settings::global(cx).vim_mode;
        // The handle is taken here and not inside the closure: created at render
        // time, it would put the list back at the top on every frame.
        let scroll = self.scroll_of("shortcuts");
        window.open_dialog(cx, move |dialog, _window, cx| {
            // Read on every frame rather than captured: the help window can be
            // left open while a shortcut is customised next door, and it is the
            // one place that must not lie about the keys.
            let sections = shortcuts::sheet(vim, &Settings::global(cx).shortcuts);
            let (left, right) = split(sections);
            let muted = cx.theme().muted_foreground;
            dialog
                .title(tr!("shortcuts-title"))
                .w(WIDTH)
                .max_w(WIDTH)
                .child(
                    v_flex()
                        .h(HEIGHT)
                        .gap_2()
                        .child(
                            div().flex_1().min_h_0().child(crate::ui::scroll::vertical(
                                "shortcuts-bar",
                                &scroll,
                                h_flex()
                                    .id("shortcuts-list")
                                    .size_full()
                                    .overflow_y_scroll()
                                    .track_scroll(&scroll)
                                    .items_start()
                                    .gap_8()
                                    .child(column(left, cx))
                                    .child(column(right, cx)),
                            )),
                        )
                        // Vim mode cannot be guessed: without this line, half
                        // the keys in this window exist for nobody.
                        .when(!vim, |el| {
                            el.child(
                                div()
                                    .text_xs()
                                    .text_color(muted)
                                    .child(tr!("shortcuts-vim-hint")),
                            )
                        }),
                )
                .footer(super::dialogs::close())
        });
    }
}

/// Splits the families into two columns of comparable height.
///
/// Each family goes into the **shorter** column of the moment, and not into the
/// left one up to the halfway mark: "Review" alone weighs a third of the table,
/// and cutting the list in two where the sum passes half would leave the right
/// column two thirds empty.
///
/// The count includes each family's title: a one-row family takes two on screen,
/// and ignoring that would unbalance things by as much.
fn split(sections: Vec<Section>) -> (Vec<Section>, Vec<Section>) {
    let mut columns: [(usize, Vec<Section>); 2] = [(0, Vec::new()), (0, Vec::new())];
    for section in sections {
        let size = section.rows.len() + 2;
        // On a tie, the left: that is where reading begins.
        let side = usize::from(columns[1].0 < columns[0].0);
        columns[side].0 += size;
        columns[side].1.push(section);
    }
    let [(_, left), (_, right)] = columns;
    (left, right)
}

fn column(sections: Vec<Section>, cx: &gpui::App) -> impl IntoElement {
    let (muted, mono, border) = (
        cx.theme().muted_foreground,
        cx.theme().mono_font_family.clone(),
        cx.theme().border,
    );
    v_flex()
        .flex_1()
        .min_w_0()
        .gap_4()
        .children(sections.into_iter().map(move |section| {
            let mono = mono.clone();
            v_flex()
                .gap_1()
                .child(
                    div()
                        .text_xs()
                        .font_semibold()
                        .text_color(muted)
                        .pb_1()
                        .border_b_1()
                        .border_color(border)
                        .child(section.title),
                )
                .children(section.rows.into_iter().map(move |row| {
                    h_flex()
                        .gap_3()
                        .items_baseline()
                        .child(
                            div()
                                .w(KEYS)
                                .flex_none()
                                .text_xs()
                                .font_family(mono.clone())
                                .text_color(muted)
                                .child(SharedString::from(row.keys)),
                        )
                        .child(div().flex_1().min_w_0().text_sm().child(row.label))
                }))
        }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ui::shortcuts::Row;

    fn section(rows: usize) -> Section {
        Section {
            title: "x".into(),
            rows: (0..rows)
                .map(|_| Row {
                    keys: String::new(),
                    label: "y".into(),
                })
                .collect(),
        }
    }

    /// A family weighing a third of the table must not make one full column and
    /// one empty one.
    #[test]
    fn the_two_columns_end_up_about_the_same_height() {
        let sizes = [7, 1, 4, 24, 7, 4, 8];
        let (left, right) = split(sizes.iter().map(|n| section(*n)).collect());
        let height =
            |column: &Vec<Section>| -> usize { column.iter().map(|s| s.rows.len() + 2).sum() };
        let (a, b) = (height(&left), height(&right));
        assert!(a.abs_diff(b) <= 4, "{a} contre {b}");
    }
}
