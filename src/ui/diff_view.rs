//! La vue de diff.
//!
//! Un diff se lit ligne à ligne, et un diff de relecture d'agent en fait
//! régulièrement plusieurs milliers. Toutes les lignes sont donc mises à plat
//! en une seule liste — en-têtes de hunk compris — et rendues par une liste
//! virtualisée : seul ce qui est à l'écran existe sous forme d'éléments.
//!
//! Aplatir est ce qui rend la virtualisation possible : une liste ne sait
//! adresser ses entrées que par un indice, alors qu'un diff est un arbre à
//! deux niveaux (hunks, puis lignes).

use std::path::Path;

use gpui_component::highlighter::HighlightTheme;

use crate::git::{DiffLineKind, FileDiff};
use crate::ui::highlight::DiffHighlights;

/// Un diff prêt à afficher.
///
/// Tout ce qui se déduit du diff — mise à plat, coloration, patchs
/// d'indexation, largeur de gouttière — est calculé ici, une fois, à
/// l'arrivée du diff. Le rendu ne fait plus que lire : c'est ce qui permet à
/// une frame de ne coûter que les lignes visibles, alors que la coloration
/// d'un fichier de dix mille lignes se compte en dizaines de millisecondes.
pub struct Rendered {
    pub file: FileDiff,
    pub rows: Vec<Row>,
    pub highlights: DiffHighlights,
    pub patches: Vec<String>,
    pub gutter_digits: usize,
    /// Indice de la ligne la plus large et sa longueur en caractères.
    ///
    /// La liste virtualisée ne mesure qu'un seul item pour décider de la
    /// largeur défilable. Lui désigner la ligne la plus longue est ce qui
    /// permet au défilement horizontal d'atteindre le bout du fichier ; sans
    /// cela il s'arrête à la largeur de la première ligne, qui est presque
    /// toujours courte.
    pub longest_row: usize,
    pub longest_chars: usize,
}

impl Rendered {
    pub fn new(path: &Path, file: FileDiff, theme: &HighlightTheme) -> Self {
        let rows = rows(&file);
        let (longest_row, longest_chars) = longest(&file, &rows);
        Self {
            highlights: DiffHighlights::compute(path, &file, theme),
            patches: hunk_patches(path, &file),
            gutter_digits: gutter_digits(&file),
            longest_row,
            longest_chars,
            rows,
            file,
        }
    }

    /// Le texte d'une entrée, pour la mesure comme pour le rendu.
    pub fn row_text(&self, row: Row) -> &str {
        match row {
            Row::Header { hunk } => self
                .file
                .hunks
                .get(hunk)
                .map(|h| h.header.as_str())
                .unwrap_or_default(),
            Row::Line { hunk, line } => self
                .file
                .hunks
                .get(hunk)
                .and_then(|h| h.lines.get(line))
                .map(|l| l.text.as_str())
                .unwrap_or_default(),
        }
    }
}

/// L'entrée la plus large, en nombre de caractères.
///
/// Compté en caractères et non en octets : à chasse fixe, c'est le nombre de
/// caractères qui donne la largeur, et un accent occupe deux octets pour une
/// seule colonne.
fn longest(file: &FileDiff, rows: &[Row]) -> (usize, usize) {
    let mut best = (0usize, 0usize);
    for (index, row) in rows.iter().enumerate() {
        let text = match row {
            Row::Header { hunk } => file.hunks.get(*hunk).map(|h| h.header.as_str()),
            Row::Line { hunk, line } => file
                .hunks
                .get(*hunk)
                .and_then(|h| h.lines.get(*line))
                .map(|l| l.text.as_str()),
        };
        let width = text.map(|t| t.chars().count()).unwrap_or(0);
        if width > best.1 {
            best = (index, width);
        }
    }
    best
}

/// Une entrée de la liste.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Row {
    /// L'en-tête `@@ … @@`, qui porte aussi le bouton d'indexation du hunk.
    Header {
        hunk: usize,
    },
    Line {
        hunk: usize,
        line: usize,
    },
}

/// Met un diff à plat, en-têtes compris.
pub fn rows(diff: &FileDiff) -> Vec<Row> {
    let mut rows = Vec::new();
    for (h, hunk) in diff.hunks.iter().enumerate() {
        rows.push(Row::Header { hunk: h });
        rows.extend((0..hunk.lines.len()).map(|line| Row::Line { hunk: h, line }));
    }
    rows
}

/// Le nombre de chiffres du plus grand numéro de ligne du fichier.
///
/// La gouttière est dimensionnée une fois pour tout le diff : la calculer par
/// écran la ferait changer de largeur en cours de défilement, quand on passe
/// de la ligne 99 à la ligne 100.
pub fn gutter_digits(diff: &FileDiff) -> usize {
    diff.hunks
        .iter()
        .flat_map(|hunk| hunk.lines.iter())
        .filter_map(|line| line.new_no.max(line.old_no))
        .max()
        .unwrap_or(1)
        .to_string()
        .len()
}

/// Le signe affiché devant une ligne.
pub fn sign(kind: DiffLineKind) -> &'static str {
    match kind {
        DiffLineKind::Added => "+",
        // Un vrai signe moins, et non le trait d'union du format git : à
        // chasse fixe, il s'aligne sur le `+` alors que le tiret flotte.
        DiffLineKind::Removed => "−",
        DiffLineKind::Context | DiffLineKind::NoNewline => " ",
    }
}

/// Les patchs d'indexation, un par hunk.
///
/// Ils sont construits une fois par diff affiché plutôt qu'au clic : un
/// gestionnaire de clic ne peut pas emprunter l'état de la revue, qui est déjà
/// emprunté par le rendu qui l'a installé.
pub fn hunk_patches(path: &Path, diff: &FileDiff) -> Vec<String> {
    diff.hunks
        .iter()
        .map(|hunk| crate::git::diff::hunk_patch(path, None, hunk, false))
        .collect()
}

// — Rendu ————————————————————————————————————————————————————————

use std::rc::Rc;

use gpui::{
    div, prelude::*, px, uniform_list, Context, Entity, ListHorizontalSizingBehavior, Pixels,
    SharedString, StyledText, Window,
};
use gpui_component::{
    button::{Button, ButtonVariants},
    h_flex, v_flex, ActiveTheme, Sizable,
};

use crate::git::DiffRange;
use crate::tr;
use crate::ui::app::PerchApp;
use crate::ui::icons::icon;
use crate::ui::theme::DiffColors;

/// Police du diff. La même que les terminaux : c'est du code, et les colonnes
/// doivent s'aligner.
const MONO: &str = "JetBrains Mono";
const FONT_SIZE: Pixels = px(12.);
/// Hauteur d'une ligne, fixée et non mesurée : toutes les entrées font
/// exactement une ligne de texte, et une hauteur explicite dispense la liste
/// virtualisée de mesurer quoi que ce soit.
const LINE_HEIGHT: Pixels = px(18.);

impl PerchApp {
    pub(super) fn render_diff(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let Some(state) = self.active_review() else {
            return div().into_any_element();
        };
        let Some(path) = state.selected.clone() else {
            return centered_message(tr!("review-pick-a-file"), cx);
        };
        let stageable = state.range == DiffRange::Unstaged;
        let diff = state.diff.clone();

        let header = h_flex()
            .h(px(30.))
            .w_full()
            .px_2()
            .gap_2()
            .items_center()
            .border_b_1()
            .border_color(cx.theme().border)
            .child(icon("file-diff").xsmall())
            .child(
                div()
                    .flex_1()
                    .truncate()
                    .text_sm()
                    .font_family(MONO)
                    .child(path.display().to_string()),
            );

        let Some(diff) = diff else {
            return v_flex()
                .size_full()
                .child(header)
                .child(hint(tr!("review-loading"), cx))
                .into_any_element();
        };
        if diff.file.binary {
            return v_flex()
                .size_full()
                .child(header)
                .child(hint(tr!("review-binary"), cx))
                .into_any_element();
        }
        if diff.rows.is_empty() {
            return v_flex()
                .size_full()
                .child(header)
                .child(hint(tr!("review-no-change"), cx))
                .into_any_element();
        }

        // Largeur d'un caractère, mesurée sur la police réellement choisie :
        // une chasse fixe ne veut pas dire une largeur connue d'avance, et un
        // écart d'un pixel décale la gouttière de tout un caractère au bout
        // d'une centaine de colonnes.
        let font = gpui::Font {
            family: MONO.into(),
            features: Default::default(),
            weight: Default::default(),
            style: Default::default(),
            fallbacks: None,
        };
        let font_id = window.text_system().resolve_font(&font);
        let cell = window
            .text_system()
            .advance(font_id, FONT_SIZE, 'M')
            .map(|size| size.width)
            .unwrap_or(px(7.));

        let gutter = cell * diff.gutter_digits as f32 + px(6.);
        // La largeur du contenu tient compte du viewport mesuré à la frame
        // précédente : sans ce plancher, le fond coloré d'une ligne modifiée
        // s'arrêterait au bout de son texte au lieu de traverser la vue.
        let viewport = self.diff_scroll.0.borrow().base_handle.bounds().size.width;
        let content_width =
            (cell * diff.longest_chars as f32 + gutter * 2. + px(24.)).max(viewport);

        let colors = DiffColors::of(cx);
        let entity = cx.entity();
        let rows = diff.clone();
        let count = diff.rows.len();

        v_flex()
            .size_full()
            .child(header)
            .child(
                div().flex_1().min_h_0().child(
                    uniform_list("diff-lines", count, move |range, _window, cx| {
                        range
                            .map(|ix| {
                                render_row(
                                    &rows,
                                    ix,
                                    &colors,
                                    gutter,
                                    content_width,
                                    stageable,
                                    &entity,
                                    cx,
                                )
                            })
                            .collect::<Vec<_>>()
                    })
                    .size_full()
                    .font_family(MONO)
                    .text_size(FONT_SIZE)
                    // Sans `Unconstrained`, les lignes sont contraintes à la
                    // largeur de la vue et le défilement horizontal n'a rien à
                    // révéler ; la largeur défilable est déduite du seul item
                    // désigné ci-dessous.
                    .with_horizontal_sizing_behavior(ListHorizontalSizingBehavior::Unconstrained)
                    .with_width_from_item(Some(diff.longest_row))
                    .track_scroll(self.diff_scroll.clone()),
                ),
            )
            .into_any_element()
    }
}

#[allow(clippy::too_many_arguments)]
fn render_row(
    diff: &Rc<Rendered>,
    index: usize,
    colors: &DiffColors,
    gutter: Pixels,
    content_width: Pixels,
    stageable: bool,
    entity: &Entity<PerchApp>,
    cx: &mut gpui::App,
) -> gpui::AnyElement {
    let Some(row) = diff.rows.get(index).copied() else {
        return div().into_any_element();
    };
    match row {
        Row::Header { hunk } => {
            let patch = diff.patches.get(hunk).cloned().unwrap_or_default();
            let entity = entity.clone();
            h_flex()
                .h(LINE_HEIGHT)
                .min_w(content_width)
                .px_2()
                .gap_2()
                .items_center()
                .whitespace_nowrap()
                .bg(colors.hunk_bg)
                .child(
                    div()
                        .text_color(cx.theme().muted_foreground)
                        .child(SharedString::from(diff.row_text(row).to_string())),
                )
                // Indexer un hunk seul n'a de sens que depuis les
                // modifications non indexées : ailleurs, ou bien tout est déjà
                // dans l'index, ou bien on regarde des commits déjà écrits.
                .when(stageable, |el| {
                    el.child(
                        Button::new(("stage-hunk", index))
                            .ghost()
                            .xsmall()
                            .icon(icon("plus"))
                            .tooltip(tr!("action-stage-hunk"))
                            .on_click(move |_, _window, cx| {
                                entity.update(cx, |this, cx| this.apply_hunk(patch.clone(), cx));
                            }),
                    )
                })
                .into_any_element()
        }
        Row::Line { hunk, line } => {
            let Some(source) = diff.file.hunks.get(hunk).and_then(|h| h.lines.get(line)) else {
                return div().into_any_element();
            };
            let (bg, fg) = match source.kind {
                DiffLineKind::Added => (Some(colors.added_bg), Some(colors.added_fg)),
                DiffLineKind::Removed => (Some(colors.removed_bg), Some(colors.removed_fg)),
                DiffLineKind::Context | DiffLineKind::NoNewline => (None, None),
            };

            let text = SharedString::from(source.text.clone());
            let styles = diff.highlights.line(hunk, line);
            // Les tabulations sont rendues telles quelles par la police : les
            // remplacer ici garderait l'alignement mais décalerait les plages
            // de coloration, calculées sur le texte d'origine.
            let content = if styles.is_empty() {
                div()
                    .when_some(fg, |el, fg| el.text_color(fg))
                    .child(text)
                    .into_any_element()
            } else {
                StyledText::new(text)
                    .with_highlights(styles.iter().cloned())
                    .into_any_element()
            };

            h_flex()
                .h(LINE_HEIGHT)
                .min_w(content_width)
                .items_center()
                .whitespace_nowrap()
                .when_some(bg, |el, bg| el.bg(bg))
                .child(number(source.old_no, gutter, colors))
                .child(number(source.new_no, gutter, colors))
                .child(
                    div()
                        .w(px(14.))
                        .flex_none()
                        .text_center()
                        .when_some(fg, |el, fg| el.text_color(fg))
                        .child(sign(source.kind)),
                )
                .child(content)
                .into_any_element()
        }
    }
}

fn number(value: Option<usize>, width: Pixels, colors: &DiffColors) -> impl IntoElement {
    div()
        .w(width)
        .flex_none()
        .text_right()
        .pr_1()
        .text_color(colors.line_number)
        .child(value.map(|n| n.to_string()).unwrap_or_default())
}

fn centered_message(text: SharedString, cx: &mut gpui::App) -> gpui::AnyElement {
    v_flex()
        .size_full()
        .items_center()
        .justify_center()
        .child(
            div()
                .text_sm()
                .text_color(cx.theme().muted_foreground)
                .child(text),
        )
        .into_any_element()
}

fn hint(text: SharedString, cx: &mut gpui::App) -> impl IntoElement {
    div()
        .p_3()
        .text_sm()
        .text_color(cx.theme().muted_foreground)
        .child(text)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::git::{DiffLine, Hunk};
    use gpui_component::highlighter::HighlightTheme as Theme;

    fn hunk(header: &str, kinds: &[DiffLineKind]) -> Hunk {
        Hunk {
            header: header.into(),
            old_start: 1,
            new_start: 1,
            lines: kinds
                .iter()
                .map(|kind| DiffLine {
                    kind: *kind,
                    old_no: Some(1),
                    new_no: Some(1),
                    text: "x".into(),
                })
                .collect(),
        }
    }

    #[test]
    fn flattens_headers_and_lines_in_order() {
        let diff = FileDiff {
            hunks: vec![
                hunk("@@ a @@", &[DiffLineKind::Context, DiffLineKind::Added]),
                hunk("@@ b @@", &[DiffLineKind::Removed]),
            ],
            binary: false,
            empty: false,
        };
        assert_eq!(
            rows(&diff),
            vec![
                Row::Header { hunk: 0 },
                Row::Line { hunk: 0, line: 0 },
                Row::Line { hunk: 0, line: 1 },
                Row::Header { hunk: 1 },
                Row::Line { hunk: 1, line: 0 },
            ]
        );
    }

    #[test]
    fn an_empty_diff_has_no_rows() {
        let diff = FileDiff::default();
        assert!(rows(&diff).is_empty());
        // La gouttière garde une largeur utilisable même sans ligne.
        assert_eq!(gutter_digits(&diff), 1);
    }

    #[test]
    fn the_longest_row_is_found_across_hunks_and_headers() {
        let mut diff = FileDiff {
            hunks: vec![
                hunk("@@ court @@", &[DiffLineKind::Context]),
                hunk("@@ b @@", &[DiffLineKind::Added]),
            ],
            binary: false,
            empty: false,
        };
        diff.hunks[1].lines[0].text = "une ligne nettement plus longue que les autres".into();
        let rendered = Rendered::new(Path::new("x.txt"), diff, &Theme::default_dark());

        assert_eq!(rendered.longest_chars, 46);
        assert_eq!(
            rendered.rows[rendered.longest_row],
            Row::Line { hunk: 1, line: 0 }
        );
    }

    #[test]
    fn highlight_runs_stay_sorted_and_disjoint() {
        // gpui convertit les plages en *longueurs* de runs consécutives, en
        // les parcourant dans l'ordre donné : une plage désordonnée ou qui en
        // chevauche une autre décale silencieusement tout ce qui suit.
        let mut diff = FileDiff {
            hunks: vec![hunk(
                "@@ a @@",
                &[DiffLineKind::Added, DiffLineKind::Context],
            )],
            binary: false,
            empty: false,
        };
        diff.hunks[0].lines[0].text = "fn calcule(x: u32) -> u32 { x + 1 }".into();
        diff.hunks[0].lines[1].text = "// un commentaire avec des accents : é à ù".into();
        let rendered = Rendered::new(Path::new("src/x.rs"), diff, &Theme::default_dark());

        for line in 0..2 {
            let text = rendered.row_text(Row::Line { hunk: 0, line });
            let mut end = 0usize;
            for (range, _) in rendered.highlights.line(0, line) {
                assert!(
                    range.start >= end,
                    "plages non triées : {range:?} après {end}"
                );
                assert!(range.start <= range.end, "plage inversée : {range:?}");
                assert!(range.end <= text.len(), "plage hors du texte : {range:?}");
                assert!(
                    text.is_char_boundary(range.start) && text.is_char_boundary(range.end),
                    "plage {range:?} coupe un caractère de « {text} »"
                );
                end = range.end;
            }
        }
    }

    #[test]
    fn the_gutter_is_sized_on_the_largest_number() {
        let mut diff = FileDiff {
            hunks: vec![hunk("@@ a @@", &[DiffLineKind::Context])],
            binary: false,
            empty: false,
        };
        diff.hunks[0].lines[0].new_no = Some(1024);
        diff.hunks[0].lines[0].old_no = Some(9);
        assert_eq!(gutter_digits(&diff), 4);
    }
}

#[cfg(test)]
mod bench {
    use super::*;

    #[test]
    fn time_rendered() {
        let Ok(spec) = std::env::var("PERCH_BENCH") else {
            return;
        };
        crate::ui::highlight::register_languages();
        let mut parts = spec.splitn(2, '|');
        let dir = std::path::PathBuf::from(parts.next().unwrap());
        let range = crate::git::DiffRange::Staged;
        let files = crate::git::diff::files(&dir, &range).unwrap();
        let theme = HighlightTheme::default_dark();

        let mut worst = (String::new(), std::time::Duration::ZERO, 0usize);
        let mut total = std::time::Duration::ZERO;
        for f in &files {
            let diff = crate::git::diff::file(&dir, &range, &f.path, 3).unwrap();
            let lines: usize = diff.hunks.iter().map(|h| h.lines.len()).sum();
            let start = std::time::Instant::now();
            let _ = Rendered::new(&f.path, diff, &theme);
            let took = start.elapsed();
            total += took;
            if took > worst.1 {
                worst = (f.path.display().to_string(), took, lines);
            }
        }
        println!(
            "{} fichiers, total {:?}, pire : {} ({} lignes) en {:?}",
            files.len(),
            total,
            worst.0,
            worst.2,
            worst.1
        );
    }
}
