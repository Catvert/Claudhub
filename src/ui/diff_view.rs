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

    /// Le texte d'une plage de lignes, prêt à coller.
    ///
    /// Sans les marqueurs, c'est du **code** qui sort : ni `+`/`-`, ni numéros
    /// de ligne, ni en-têtes `@@`. C'est ce qu'on veut coller dans un éditeur
    /// ou dans l'invite d'un agent, et le nettoyer à la main après coup est
    /// exactement la corvée que cette vue doit éviter.
    ///
    /// Avec les marqueurs, c'est un extrait de patch : les signes de git —
    /// `-` et non le vrai signe moins de l'affichage, qui ne s'applique pas.
    pub fn copy_text(&self, from: usize, to: usize, with_markers: bool) -> String {
        let (from, to) = (from.min(to), from.max(to));
        let mut out = String::new();
        for index in from..=to {
            let Some(row) = self.rows.get(index).copied() else {
                continue;
            };
            match row {
                Row::Header { .. } => {
                    if !with_markers {
                        continue;
                    }
                    out.push_str(self.row_text(row));
                    out.push('\n');
                }
                Row::Line { hunk, line } => {
                    let Some(source) = self
                        .file
                        .hunks
                        .get(hunk)
                        .and_then(|hunk| hunk.lines.get(line))
                    else {
                        continue;
                    };
                    // « \ No newline at end of file » est une annotation de
                    // patch, pas une ligne du fichier.
                    if source.kind == DiffLineKind::NoNewline && !with_markers {
                        continue;
                    }
                    if with_markers {
                        out.push_str(patch_sign(source.kind));
                    }
                    out.push_str(&source.text);
                    out.push('\n');
                }
            }
        }
        out
    }

    /// Les indices de la première et de la dernière ligne d'un hunk.
    pub fn hunk_bounds(&self, hunk: usize) -> Option<(usize, usize)> {
        let first = self
            .rows
            .iter()
            .position(|row| matches!(row, Row::Header { hunk: h } if *h == hunk))?;
        let last = self
            .rows
            .iter()
            .rposition(|row| matches!(row, Row::Line { hunk: h, .. } if *h == hunk))
            .unwrap_or(first);
        Some((first, last))
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

/// Le signe de git, celui qui fait qu'un extrait reste un patch applicable.
fn patch_sign(kind: DiffLineKind) -> &'static str {
    match kind {
        DiffLineKind::Added => "+",
        DiffLineKind::Removed => "-",
        DiffLineKind::Context => " ",
        DiffLineKind::NoNewline => "",
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
    div, prelude::*, px, uniform_list, Context, Entity, Focusable, ListHorizontalSizingBehavior,
    Pixels, SharedString, StyledText, Window,
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

/// Hauteur d'une ligne, en proportion de la taille du texte.
///
/// Elle est fixée et non mesurée : toutes les entrées font exactement une
/// ligne, et une hauteur explicite dispense la liste virtualisée de mesurer
/// quoi que ce soit. Le facteur suit la taille choisie, faute de quoi grossir
/// le texte le ferait déborder d'une hauteur restée constante.
const LINE_SPACING: f32 = 1.5;

pub fn line_height(font_size: Pixels) -> Pixels {
    (font_size * LINE_SPACING).round()
}

impl PerchApp {
    /// Sélectionne une ligne, ou étend la sélection jusqu'à elle.
    ///
    /// Clic simple : la ligne devient l'ancre. Maj+clic : l'ancre reste et la
    /// tête se déplace, ce qui est la convention de toutes les listes et évite
    /// d'avoir à glisser sur trois cents lignes pour en attraper un bloc.
    pub(super) fn select_diff_row(&mut self, index: usize, extend: bool, cx: &mut Context<Self>) {
        let Some(state) = self.active_review_mut() else {
            return;
        };
        state.diff_selection = match (extend, state.diff_selection) {
            (true, Some((anchor, _))) => Some((anchor, index)),
            _ => Some((index, index)),
        };
        cx.notify();
    }

    /// Copie la sélection, ou tout le fichier s'il n'y en a pas.
    ///
    /// Sans sélection, `Ctrl+C` sur un diff ne peut vouloir dire qu'une chose,
    /// et refuser d'agir serait un refus poli sans raison.
    pub(super) fn copy_diff(&mut self, with_markers: bool, cx: &mut Context<Self>) {
        let Some(state) = self.active_review() else {
            return;
        };
        let Some(diff) = state.diff.clone() else {
            return;
        };
        let (from, to) = state
            .diff_selection
            .unwrap_or((0, diff.rows.len().saturating_sub(1)));
        self.copy_rows(&diff, from, to, with_markers, cx);
    }

    pub(super) fn copy_hunk(&mut self, hunk: usize, with_markers: bool, cx: &mut Context<Self>) {
        let Some(diff) = self.active_review().and_then(|state| state.diff.clone()) else {
            return;
        };
        let Some((from, to)) = diff.hunk_bounds(hunk) else {
            return;
        };
        self.copy_rows(&diff, from, to, with_markers, cx);
    }

    fn copy_rows(
        &mut self,
        diff: &Rc<Rendered>,
        from: usize,
        to: usize,
        with_markers: bool,
        cx: &mut Context<Self>,
    ) {
        let text = diff.copy_text(from, to, with_markers);
        if text.is_empty() {
            return;
        }
        let lines = text.lines().count();
        cx.write_to_clipboard(gpui::ClipboardItem::new_string(text));
        self.announce(tr!("copy-done", { count: lines }), cx);
    }

    pub(super) fn copy_diff_path(&mut self, cx: &mut Context<Self>) {
        let Some(path) = self
            .active_review()
            .and_then(|state| state.selected.clone())
        else {
            return;
        };
        let path = path.display().to_string();
        cx.write_to_clipboard(gpui::ClipboardItem::new_string(path));
        self.announce(tr!("copy-path-done"), cx);
    }

    /// Molette avec la touche système : grossir ou réduire le code relu.
    ///
    /// La liste a **déjà** défilé quand cet écouteur s'exécute — les deux sont
    /// en phase de remontée, et l'enfant est traité avant son parent. gpui
    /// n'expose pas de phase de capture pour la molette : on rend donc le
    /// décalage plutôt que d'essayer de l'empêcher, sans quoi chaque cran de
    /// zoom ferait aussi sauter la lecture de trois lignes.
    pub(super) fn on_diff_scroll(
        &mut self,
        event: &gpui::ScrollWheelEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !event.modifiers.secondary() {
            return;
        }
        let delta = event.delta.pixel_delta(window.line_height().max(px(1.)));
        let handle = self.diff_scroll.0.borrow().base_handle.clone();
        // Un seul axe bouge à la fois : c'est le comportement par défaut de
        // gpui, qui ne laisse passer que la composante dominante.
        let undo = if delta.x.abs() > delta.y.abs() {
            gpui::point(delta.x, px(0.))
        } else {
            gpui::point(px(0.), delta.y)
        };
        handle.set_offset(handle.offset() - undo);

        let steps = crate::ui::terminal_view::zoom_steps(delta.y);
        if steps != 0. {
            crate::ui::settings::Settings::update_global(cx, |s| {
                s.zoom(crate::ui::settings::Zoom::Diff, steps);
            });
        }
        cx.notify();
    }
}

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
        let stageable = state.range == DiffRange::Working;
        let diff = state.diff.clone();
        let mono = cx.theme().mono_font_family.clone();
        let font_size = px(crate::ui::settings::Settings::global(cx).diff_font_size);
        let line_height = line_height(font_size);

        let header = h_flex()
            .h(crate::ui::theme::bar_height(cx))
            .w_full()
            .px_2()
            .gap_2()
            .items_center()
            .border_b_1()
            .border_color(cx.theme().border)
            .child(icon("file-diff").xsmall())
            .child(
                div()
                    .id("diff-path")
                    .flex_1()
                    .truncate()
                    .text_sm()
                    .cursor_pointer()
                    .font_family(mono.clone())
                    .tooltip(|window, cx| {
                        gpui_component::tooltip::Tooltip::new(tr!("action-copy-path"))
                            .build(window, cx)
                    })
                    .on_click(cx.listener(|this, _, _window, cx| this.copy_diff_path(cx)))
                    .child(path.display().to_string()),
            )
            .child(
                Button::new("copy-file")
                    .ghost()
                    .xsmall()
                    .icon(icon("copy"))
                    .tooltip(tr!("action-copy-file"))
                    .on_click(cx.listener(|this, _, _window, cx| this.copy_diff(false, cx))),
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
            family: mono.clone(),
            features: Default::default(),
            weight: Default::default(),
            style: Default::default(),
            fallbacks: None,
        };
        let font_id = window.text_system().resolve_font(&font);
        let cell = window
            .text_system()
            .advance(font_id, font_size, 'M')
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
        let selection = self
            .active_review()
            .and_then(|state| state.diff_selection)
            .map(|(a, b)| (a.min(b), a.max(b)));
        let selection_bg = cx.theme().selection;

        v_flex()
            .size_full()
            .child(header)
            .child(
                div()
                    .id("diff-zoom")
                    .flex_1()
                    .min_h_0()
                    .on_scroll_wheel(cx.listener(Self::on_diff_scroll))
                    .child(
                        uniform_list("diff-lines", count, move |range, _window, cx| {
                            range
                                .map(|ix| {
                                    render_row(
                                        &rows,
                                        ix,
                                        &colors,
                                        gutter,
                                        content_width,
                                        line_height,
                                        stageable,
                                        selection.is_some_and(|(a, b)| ix >= a && ix <= b),
                                        selection_bg,
                                        &entity,
                                        cx,
                                    )
                                })
                                .collect::<Vec<_>>()
                        })
                        .size_full()
                        .font_family(mono)
                        .text_size(font_size)
                        // Sans `Unconstrained`, les lignes sont contraintes à la
                        // largeur de la vue et le défilement horizontal n'a rien à
                        // révéler ; la largeur défilable est déduite du seul item
                        // désigné ci-dessous.
                        .with_horizontal_sizing_behavior(
                            ListHorizontalSizingBehavior::Unconstrained,
                        )
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
    line_height: Pixels,
    stageable: bool,
    selected: bool,
    selection_bg: gpui::Hsla,
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
            let for_copy = entity.clone();
            let for_click = entity.clone();
            h_flex()
                .id(("hunk", index))
                .h(line_height)
                .min_w(content_width)
                .px_2()
                .gap_2()
                .items_center()
                .whitespace_nowrap()
                .bg(if selected {
                    selection_bg
                } else {
                    colors.hunk_bg
                })
                .on_mouse_down(gpui::MouseButton::Left, move |event, window, cx| {
                    select(&for_click, index, event.modifiers.shift, window, cx);
                })
                .child(
                    div()
                        .text_color(cx.theme().muted_foreground)
                        .child(SharedString::from(diff.row_text(row).to_string())),
                )
                .child(
                    Button::new(("copy-hunk", index))
                        .ghost()
                        .xsmall()
                        .icon(icon("copy"))
                        .tooltip(tr!("action-copy-hunk"))
                        .on_click(move |_, _window, cx| {
                            for_copy.update(cx, |this, cx| this.copy_hunk(hunk, false, cx));
                        }),
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

            let entity = entity.clone();
            h_flex()
                .id(("line", index))
                .h(line_height)
                .min_w(content_width)
                .items_center()
                .whitespace_nowrap()
                // La sélection remplace le fond de la ligne plutôt que de s'y
                // ajouter : gpui n'empile pas deux fonds sur un même nœud, et
                // une sélection qu'on distingue mal de l'ajout qu'elle
                // recouvre ne sert à rien.
                .when_some(bg.filter(|_| !selected), |el, bg| el.bg(bg))
                .when(selected, |el| el.bg(selection_bg))
                .on_mouse_down(gpui::MouseButton::Left, move |event, window, cx| {
                    select(&entity, index, event.modifiers.shift, window, cx);
                })
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

/// Sélectionne une ligne **et prend le focus**.
///
/// Le second point n'est pas un détail : sans lui, cliquer une ligne laisse le
/// focus au terminal, et le `Ctrl+C` qui suit part au programme qui y tourne
/// au lieu de copier ce qu'on vient de sélectionner.
fn select(
    entity: &Entity<PerchApp>,
    index: usize,
    extend: bool,
    window: &mut Window,
    cx: &mut gpui::App,
) {
    let handle = entity.read(cx).focus_handle(cx);
    window.focus(&handle);
    entity.update(cx, |this, cx| this.select_diff_row(index, extend, cx));
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
    fn copying_yields_code_and_not_a_patch() {
        let mut diff = FileDiff::default();
        diff.hunks.push(hunk(
            "@@ -1,3 +1,3 @@",
            &[
                DiffLineKind::Context,
                DiffLineKind::Removed,
                DiffLineKind::Added,
                DiffLineKind::NoNewline,
            ],
        ));
        for (ix, text) in ["garde", "avant", "après", "\\ No newline"]
            .iter()
            .enumerate()
        {
            diff.hunks[0].lines[ix].text = (*text).into();
        }
        let rendered = Rendered::new(Path::new("a.rs"), diff, &HighlightTheme::default_dark());

        // Tout le fichier, en code : ni en-tête `@@`, ni signes, ni
        // l'annotation de fin de fichier — c'est ce qu'on colle dans un
        // éditeur.
        let all = rendered.copy_text(0, rendered.rows.len() - 1, false);
        assert_eq!(all, "garde\navant\naprès\n");

        // La même plage en patch garde de quoi être appliquée.
        let patch = rendered.copy_text(0, rendered.rows.len() - 1, true);
        assert_eq!(
            patch,
            "@@ -1,3 +1,3 @@\n garde\n-avant\n+après\n\\ No newline\n"
        );

        // Une plage prise à l'envers vaut la même chose : on sélectionne
        // parfois de bas en haut.
        assert_eq!(
            rendered.copy_text(3, 1, false),
            rendered.copy_text(1, 3, false)
        );
    }

    #[test]
    fn a_hunk_knows_where_it_begins_and_ends() {
        let mut diff = FileDiff::default();
        diff.hunks
            .push(hunk("@@ -1 +1 @@", &[DiffLineKind::Context]));
        diff.hunks.push(hunk(
            "@@ -9 +9 @@",
            &[DiffLineKind::Added, DiffLineKind::Added],
        ));
        let rendered = Rendered::new(Path::new("a.rs"), diff, &HighlightTheme::default_dark());
        // Deux entrées pour le premier hunk, trois pour le second.
        assert_eq!(rendered.hunk_bounds(0), Some((0, 1)));
        assert_eq!(rendered.hunk_bounds(1), Some((2, 4)));
        assert_eq!(rendered.hunk_bounds(2), None);
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
