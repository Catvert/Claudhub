//! La fenêtre d'aide des raccourcis.
//!
//! Elle ne connaît aucun raccourci : tout vient de `shortcuts::sheet`, qui les
//! lit dans la table même dont sortent les liaisons. Une aide écrite à part
//! aurait divergé au premier ajout, et une aide qui ment sur les touches est
//! pire qu'une absence d'aide.
//!
//! Deux colonnes, et le partage se fait sur le **nombre de lignes** plutôt
//! qu'au milieu de la liste : les familles n'ont pas la même taille — la
//! relecture en compte quatre fois plus que les worktrees — et couper au
//! milieu du tableau laisserait une colonne à moitié vide.

use gpui::{div, prelude::*, px, Context, SharedString, Window};
use gpui_component::{h_flex, v_flex, ActiveTheme, StyledExt, WindowExt};

use crate::tr;
use crate::ui::app::ClaudhubApp;
use crate::ui::settings::Settings;
use crate::ui::shortcuts::{self, Section};

const WIDTH: gpui::Pixels = px(860.);
const HEIGHT: gpui::Pixels = px(560.);
/// Largeur réservée aux touches. Fixe, pour que les libellés s'alignent d'une
/// ligne à l'autre : c'est la colonne qu'on parcourt du regard.
const KEYS: gpui::Pixels = px(148.);

impl ClaudhubApp {
    pub(super) fn open_shortcuts(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let vim = Settings::global(cx).vim_mode;
        // La poignée est prise ici et non dans la fermeture : créée au rendu,
        // elle remettrait la liste en haut à chaque frame.
        let scroll = self.scroll_of("shortcuts");
        window.open_dialog(cx, move |dialog, _window, cx| {
            let sections = shortcuts::sheet(vim);
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
                        // Le mode vim ne se devine pas : sans cette ligne, la
                        // moitié des touches de cette fenêtre n'existent pour
                        // personne.
                        .when(!vim, |el| {
                            el.child(
                                div()
                                    .text_xs()
                                    .text_color(muted)
                                    .child(tr!("shortcuts-vim-hint")),
                            )
                        }),
                )
        });
    }
}

/// Répartit les familles en deux colonnes de hauteur comparable.
///
/// Chaque famille va dans la colonne la **plus courte** du moment, et non dans
/// la gauche jusqu'à la moitié : « Relecture » pèse à elle seule un tiers du
/// tableau, et couper la liste en deux à l'endroit où la somme dépasse la
/// moitié laisserait la colonne de droite aux deux tiers vide.
///
/// Le compte inclut le titre de chaque famille : une famille d'une seule ligne
/// en occupe deux à l'écran, et l'ignorer déséquilibrerait d'autant.
fn split(sections: Vec<Section>) -> (Vec<Section>, Vec<Section>) {
    let mut columns: [(usize, Vec<Section>); 2] = [(0, Vec::new()), (0, Vec::new())];
    for section in sections {
        let size = section.rows.len() + 2;
        // À égalité, la gauche : c'est là que la lecture commence.
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

    /// Une famille qui pèse un tiers du tableau ne doit pas faire une colonne
    /// pleine et une colonne vide.
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
