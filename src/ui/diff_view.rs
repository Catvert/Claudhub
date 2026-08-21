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
    /// La même chose en deux colonnes, appariée ligne à ligne. Construite en
    /// même temps que le reste plutôt qu'au premier passage en mode « split » :
    /// un `Rendered` est partagé par `Rc` et ne se modifie plus, et le coût
    /// est sans commune mesure avec celui de la coloration.
    pub split: Vec<SplitRow>,
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
    /// La longueur de chaque entrée, en caractères, dans l'ordre de `rows`.
    ///
    /// C'est ce qui donne la hauteur d'une ligne repliée sans retoucher au
    /// texte : la police du diff est à chasse fixe, un caractère vaut une
    /// colonne, et le nombre de lignes visibles d'une ligne longue est une
    /// division. Calculée une fois ici parce que la hauteur se recalcule à
    /// chaque changement de largeur — un glissement de séparateur en produit
    /// une par image — et que reparcourir le texte du fichier à chaque fois
    /// coûterait ce que la virtualisation économise.
    pub row_chars: Vec<usize>,
}

impl Rendered {
    pub fn new(path: &Path, file: FileDiff, theme: &HighlightTheme) -> Self {
        let rows = rows(&file);
        let (longest_row, longest_chars) = longest(&file, &rows);
        let row_chars = rows.iter().map(|row| row_width(&file, *row)).collect();
        Self {
            row_chars,
            highlights: DiffHighlights::compute(path, &file, theme),
            patches: hunk_patches(path, &file),
            gutter_digits: gutter_digits(&file),
            longest_row,
            longest_chars,
            split: split_rows(&file, &rows),
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

    /// Le nombre d'entrées de la liste affichée.
    pub fn len(&self, split: bool) -> usize {
        if split {
            self.split.len()
        } else {
            self.rows.len()
        }
    }

    /// Ramène une plage de la liste en deux colonnes à la liste unifiée.
    ///
    /// C'est la liste unifiée qui porte l'ordre du fichier : appariées, les
    /// lignes supprimées et ajoutées se retrouvent sur la même entrée, et une
    /// copie doit rendre les deux dans l'ordre où git les écrit.
    pub fn unified_span(&self, from: usize, to: usize) -> Option<(usize, usize)> {
        let (from, to) = (from.min(to), from.max(to));
        let mut bounds: Option<(usize, usize)> = None;
        for row in self
            .split
            .get(from..=to.min(self.split.len().saturating_sub(1)))?
        {
            for index in row.unified() {
                bounds = Some(match bounds {
                    Some((a, b)) => (a.min(index), b.max(index)),
                    None => (index, index),
                });
            }
        }
        bounds
    }

    /// L'indice, dans la liste **affichée**, d'une ligne désignée par son
    /// hunk et son rang.
    ///
    /// Une occurrence de recherche porte sur le texte du fichier, pas sur une
    /// entrée de liste : c'est la traduction qui manque entre les deux, et
    /// elle dépend de la disposition — appariées, une suppression et l'ajout
    /// qui lui répond tiennent sur une même entrée en deux colonnes.
    pub fn display_row(&self, hunk: usize, line: usize, split: bool) -> Option<usize> {
        let unified = self.rows.iter().position(
            |row| matches!(row, Row::Line { hunk: h, line: l } if *h == hunk && *l == line),
        )?;
        if !split {
            return Some(unified);
        }
        self.split
            .iter()
            .position(|row| row.unified().any(|index| index == unified))
    }

    /// Les indices des en-têtes de hunk dans la liste affichée, en ordre
    /// croissant.
    pub fn headers(&self, split: bool) -> Vec<usize> {
        if split {
            self.split
                .iter()
                .enumerate()
                .filter(|(_, row)| matches!(row, SplitRow::Header { .. }))
                .map(|(index, _)| index)
                .collect()
        } else {
            self.rows
                .iter()
                .enumerate()
                .filter(|(_, row)| matches!(row, Row::Header { .. }))
                .map(|(index, _)| index)
                .collect()
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
        let width = row_width(file, *row);
        if width > best.1 {
            best = (index, width);
        }
    }
    best
}

/// La longueur d'une entrée, en caractères et non en octets : c'est une
/// largeur à l'écran, et un accent y compte pour un.
fn row_width(file: &FileDiff, row: Row) -> usize {
    let text = match row {
        Row::Header { hunk } => file.hunks.get(hunk).map(|h| h.header.as_str()),
        Row::Line { hunk, line } => file
            .hunks
            .get(hunk)
            .and_then(|h| h.lines.get(line))
            .map(|l| l.text.as_str()),
    };
    text.map(|t| t.chars().count()).unwrap_or(0)
}

// — Le retour à la ligne des deux colonnes ————————————————————————————

/// Combien de lignes visibles occupe une entrée large de `chars` caractères.
///
/// Le repli se fait **à la colonne**, comme dans un terminal, et non aux
/// espaces : c'est ce qui rend la hauteur calculable à l'avance. Le shaper de
/// gpui, lui, coupe aux mots, et une hauteur devinée qui ne tombe pas juste
/// laisserait les lignes se recouvrir — la liste virtualisée réserve
/// exactement ce qu'on lui annonce.
pub fn wrapped_lines(chars: usize, cols: usize) -> usize {
    if cols == 0 {
        return 1;
    }
    chars.div_ceil(cols).max(1)
}

/// La hauteur de chaque entrée de la vue en deux colonnes, en lignes.
///
/// Une paire fait la hauteur de la plus haute de ses deux moitiés : les deux
/// versions restent en regard, ce qui est tout l'intérêt de cette vue.
pub fn split_heights(diff: &Rendered, cols: usize) -> Vec<usize> {
    diff.split
        .iter()
        .map(|row| match row {
            SplitRow::Header { .. } => 1,
            SplitRow::Pair { old, new } => [old, new]
                .into_iter()
                .flatten()
                .map(|index| wrapped_lines(diff.row_chars.get(*index).copied().unwrap_or(0), cols))
                .max()
                .unwrap_or(1),
        })
        .collect()
}

/// Les octets d'une tranche de colonnes.
///
/// En caractères et non en octets : une ligne accentuée se replierait sinon
/// une colonne trop tôt, et au milieu d'un caractère.
fn char_span(text: &str, from: usize, to: usize) -> std::ops::Range<usize> {
    let mut start = text.len();
    let mut end = text.len();
    for (count, (offset, _)) in text.char_indices().enumerate() {
        if count == from {
            start = offset;
        }
        if count == to {
            end = offset;
            break;
        }
    }
    start.min(end)..end
}

/// Les plages d'une tranche, ramenées au début de celle-ci.
///
/// Elles restent **triées et disjointes**, l'invariant que gpui ne vérifie pas
/// et dont la violation décale tout ce qui suit — le découpage ne fait que
/// rogner des plages déjà dans cet ordre.
fn slice_runs<T: Clone>(
    runs: &[(std::ops::Range<usize>, T)],
    span: &std::ops::Range<usize>,
) -> Vec<(std::ops::Range<usize>, T)> {
    runs.iter()
        .filter_map(|(range, style)| {
            let start = range.start.max(span.start);
            let end = range.end.min(span.end);
            (start < end).then(|| (start - span.start..end - span.start, style.clone()))
        })
        .collect()
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

/// Une entrée de la liste en deux colonnes.
///
/// Les indices sont ceux de la liste **unifiée** : elle reste la référence —
/// pour le texte, la coloration et la copie —, et les deux colonnes n'en sont
/// qu'un autre agencement.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SplitRow {
    Header {
        hunk: usize,
        row: usize,
    },
    /// Une ligne de gauche, une de droite, ou une seule des deux : un ajout
    /// sans suppression en face laisse la gauche vide, et réciproquement.
    Pair {
        old: Option<usize>,
        new: Option<usize>,
    },
}

impl SplitRow {
    /// Les entrées de la liste unifiée que cette entrée-ci recouvre.
    pub fn unified(self) -> impl Iterator<Item = usize> {
        let (a, b) = match self {
            SplitRow::Header { row, .. } => (Some(row), None),
            SplitRow::Pair { old, new } => (old, new),
        };
        a.into_iter().chain(b)
    }
}

/// Apparie les deux versions d'un diff.
///
/// Un bloc de suppressions suivi d'un bloc d'ajouts est ce que git écrit pour
/// une modification : les apparier rang par rang remet en face les deux
/// versions d'une même ligne, ce qui est tout l'intérêt de la vue en colonnes.
/// Quand les blocs n'ont pas la même hauteur, le plus court se termine par des
/// cases vides — il n'y a rien à montrer en face.
pub fn split_rows(diff: &FileDiff, rows: &[Row]) -> Vec<SplitRow> {
    let mut out = Vec::new();
    let mut olds: Vec<usize> = Vec::new();
    let mut news: Vec<usize> = Vec::new();
    for (index, row) in rows.iter().enumerate() {
        match *row {
            Row::Header { hunk } => {
                pair_up(&mut olds, &mut news, &mut out);
                out.push(SplitRow::Header { hunk, row: index });
            }
            Row::Line { hunk, line } => {
                let kind = diff
                    .hunks
                    .get(hunk)
                    .and_then(|h| h.lines.get(line))
                    .map(|l| l.kind);
                match kind {
                    Some(DiffLineKind::Removed) => olds.push(index),
                    Some(DiffLineKind::Added) => news.push(index),
                    // Une ligne de contexte appartient aux deux versions : elle
                    // ferme le bloc en cours et occupe les deux colonnes.
                    _ => {
                        pair_up(&mut olds, &mut news, &mut out);
                        out.push(SplitRow::Pair {
                            old: Some(index),
                            new: Some(index),
                        });
                    }
                }
            }
        }
    }
    pair_up(&mut olds, &mut news, &mut out);
    out
}

fn pair_up(olds: &mut Vec<usize>, news: &mut Vec<usize>, out: &mut Vec<SplitRow>) {
    for i in 0..olds.len().max(news.len()) {
        out.push(SplitRow::Pair {
            old: olds.get(i).copied(),
            new: news.get(i).copied(),
        });
    }
    olds.clear();
    news.clear();
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
    div, prelude::*, px, uniform_list, App, Context, Entity, Focusable,
    ListHorizontalSizingBehavior, Pixels, SharedString, StyledText, Window,
};
use gpui_component::{
    button::{Button, ButtonVariants},
    h_flex,
    menu::ContextMenuExt,
    v_flex, v_virtual_list, ActiveTheme, Selectable, Sizable,
};

use crate::git::DiffRange;
use crate::tr;
use crate::ui::app::ClaudhubApp;
use crate::ui::icons::icon;
use crate::ui::theme::DiffColors;

/// Hauteur d'une ligne, en proportion de la taille du texte.
///
/// Elle est fixée et non mesurée : toutes les entrées font exactement une
/// ligne, et une hauteur explicite dispense la liste virtualisée de mesurer
/// quoi que ce soit. Le facteur suit la taille choisie, faute de quoi grossir
/// le texte le ferait déborder d'une hauteur restée constante.
const LINE_SPACING: f32 = 1.5;

/// La barre de défilement du diff, et donc la clé de son lissage : une seule
/// valeur pour les deux, voir `ui::scroll`.
const DIFF_SCROLL: &str = "diff-lines-bar";

/// Un déplacement qui ne dépend pas de la ligne courante — ou qui n'en dépend
/// que par une hauteur de vue.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Jump {
    Start,
    End,
    PageUp,
    PageDown,
}

pub fn line_height(font_size: Pixels) -> Pixels {
    (font_size * LINE_SPACING).round()
}

impl ClaudhubApp {
    /// Sélectionne une ligne, ou étend la sélection jusqu'à elle.
    ///
    /// Clic simple : la ligne devient l'ancre. Maj+clic : l'ancre reste et la
    /// tête se déplace, ce qui est la convention de toutes les listes et évite
    /// d'avoir à glisser sur trois cents lignes pour en attraper un bloc.
    pub(super) fn select_diff_row(&mut self, index: usize, extend: bool, cx: &mut Context<Self>) {
        let Some(state) = self.active_review_mut() else {
            return;
        };
        let next = match (extend, state.diff_selection) {
            (true, Some((anchor, _))) => Some((anchor, index)),
            _ => Some((index, index)),
        };
        // Un glissement passe par cette fonction à chaque ligne survolée :
        // sans ce garde, chaque pixel de mouvement redemanderait un rendu de
        // toute la liste pour une sélection qui n'a pas bougé.
        if state.diff_selection == next {
            return;
        }
        state.diff_selection = next;
        cx.notify();
    }

    /// Étend la sélection pendant un glissement.
    pub(super) fn drag_diff_row(&mut self, index: usize, cx: &mut Context<Self>) {
        if !self.diff_dragging {
            return;
        }
        self.select_diff_row(index, true, cx);
    }

    pub(super) fn end_diff_drag(&mut self) {
        self.diff_dragging = false;
    }

    /// Bascule entre une colonne et deux.
    ///
    /// La sélection est abandonnée : ses indices désignent la liste affichée,
    /// et les deux listes ne comptent pas les mêmes entrées.
    pub(super) fn toggle_diff_split(&mut self, cx: &mut Context<Self>) {
        crate::ui::settings::Settings::update_global(cx, |s| s.diff_split = !s.diff_split);
        if let Some(state) = self.active_review_mut() {
            state.diff_selection = None;
        }
        cx.notify();
    }

    /// Renvoie les lignes longues à la ligne, ou les laisse courir.
    ///
    /// La sélection tombe, comme au changement de mode : ses indices désignent
    /// la liste affichée, et les deux listes n'ont pas la même géométrie.
    pub(super) fn toggle_diff_wrap(&mut self, cx: &mut Context<Self>) {
        crate::ui::settings::Settings::update_global(cx, |s| s.diff_wrap = !s.diff_wrap);
        cx.notify();
    }

    /// Bascule entre « tout le fichier » et les seules modifications.
    ///
    /// Le diff est relu : les lignes élidées ne sont nulle part dans ce qu'on
    /// a en mémoire, git seul sait ce qu'elles contenaient.
    pub(super) fn toggle_whole_file(&mut self, cx: &mut Context<Self>) {
        crate::ui::settings::Settings::update_global(cx, |s| {
            s.diff_whole_file = !s.diff_whole_file
        });
        self.reload_diff(cx);
    }

    /// Sélectionne tout le diff affiché.
    pub(super) fn select_whole_diff(&mut self, cx: &mut Context<Self>) {
        let split = crate::ui::settings::Settings::global(cx).diff_split;
        let Some(last) = self
            .active_review()
            .and_then(|state| state.diff.as_ref())
            .map(|diff| diff.len(split).saturating_sub(1))
        else {
            return;
        };
        let Some(state) = self.active_review_mut() else {
            return;
        };
        state.diff_selection = Some((0, last));
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
        // La copie part toujours de la liste unifiée : c'est elle qui porte
        // l'ordre du fichier. En deux colonnes, la sélection y est ramenée.
        let split = crate::ui::settings::Settings::global(cx).diff_split;
        let (from, to) = match (split, state.diff_selection) {
            (true, Some((a, b))) => match diff.unified_span(a, b) {
                Some(span) => span,
                None => return,
            },
            (false, Some(span)) => span,
            (_, None) => (0, diff.rows.len().saturating_sub(1)),
        };
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

    /// Recalcule les occurrences du diff affiché, si la requête ou le diff a
    /// changé.
    ///
    /// Appelée au rendu, mais elle ne travaille qu'aux changements : la
    /// comparaison d'une chaîne par frame est ce que coûte le fait de ne pas
    /// avoir à prévenir tous les endroits d'où une requête peut changer.
    pub(super) fn refresh_diff_search(&mut self, query: &str) {
        if self.diff_search.valid && self.diff_search.query == query {
            return;
        }
        let mut hits = Vec::new();
        if let Some(diff) = self.active_review().and_then(|state| state.diff.clone()) {
            for (h, hunk) in diff.file.hunks.iter().enumerate() {
                for (l, line) in hunk.lines.iter().enumerate() {
                    hits.extend(
                        crate::ui::find::find_all(query, &line.text)
                            .into_iter()
                            .map(|range| crate::ui::find::Hit {
                                hunk: h,
                                line: l,
                                range,
                            }),
                    );
                }
            }
        }
        // Rangées par ligne parce que c'est ainsi que le rendu les consulte,
        // et qu'il le fait pour chaque ligne visible de chaque frame.
        let mut by_line: std::collections::HashMap<(usize, usize), Vec<std::ops::Range<usize>>> =
            std::collections::HashMap::new();
        for hit in &hits {
            by_line
                .entry((hit.hunk, hit.line))
                .or_default()
                .push(hit.range.clone());
        }
        self.diff_search = crate::ui::find::DiffSearch {
            query: query.to_string(),
            valid: true,
            hits: std::rc::Rc::new(hits),
            by_line: std::rc::Rc::new(by_line),
            current: 0,
        };
    }

    /// Passe à l'occurrence suivante ou précédente, en bouclant.
    ///
    /// Elle boucle, contrairement à la relecture au clavier qui bute aux deux
    /// bouts : une recherche qui s'arrête à la dernière occurrence oblige à
    /// remonter à la main pour revoir la première, alors qu'on cherche
    /// justement à faire le tour de ce qu'on a trouvé.
    pub(super) fn step_diff_match(&mut self, delta: isize, cx: &mut Context<Self>) {
        let query = self.query(crate::ui::find::Pane::Diff, cx);
        self.refresh_diff_search(&query);
        let total = self.diff_search.hits.len();
        if total == 0 {
            return;
        }
        let current =
            (self.diff_search.current as isize + delta).rem_euclid(total as isize) as usize;
        self.diff_search.current = current;
        let Some(hit) = self.diff_search.hits.get(current).cloned() else {
            return;
        };
        let split = crate::ui::settings::Settings::global(cx).diff_split;
        let row = self
            .active_review()
            .and_then(|state| state.diff.as_ref())
            .and_then(|diff| diff.display_row(hit.hunk, hit.line, split));
        let Some(row) = row else {
            return;
        };
        if let Some(state) = self.active_review_mut() {
            state.diff_selection = Some((row, row));
        }
        self.reveal_diff_row(row, gpui::ScrollStrategy::Center, cx);
        cx.notify();
    }

    /// Déplace la sélection d'une ligne, et la ramène dans la vue.
    ///
    /// `extend` garde l'ancre en place : c'est Maj+flèche, qui prend un bloc
    /// de lignes au clavier comme le glissement le prend à la souris.
    pub(super) fn step_diff_row(&mut self, delta: isize, extend: bool, cx: &mut Context<Self>) {
        let split = crate::ui::settings::Settings::global(cx).diff_split;
        let Some(len) = self
            .active_review()
            .and_then(|state| state.diff.as_ref())
            .map(|diff| diff.len(split))
        else {
            return;
        };
        let current = self.active_review().and_then(|state| state.diff_selection);
        let Some(head) = step(current.map(|(_, head)| head), delta, len) else {
            return;
        };
        let anchor = match current {
            Some((anchor, _)) if extend => anchor,
            _ => head,
        };
        self.move_diff_selection(anchor, head, cx);
    }

    /// Va d'un bout du fichier à l'autre, ou d'une hauteur de vue.
    ///
    /// La hauteur d'une page est celle du panneau, mesurée à la frame
    /// précédente : c'est ce que « page » veut dire pour l'œil, et un nombre
    /// de lignes fixé d'avance vaudrait le double une fois la police
    /// grossie.
    pub(super) fn jump_diff(&mut self, jump: Jump, cx: &mut Context<Self>) {
        let split = crate::ui::settings::Settings::global(cx).diff_split;
        let page = self.page_rows(cx);
        let Some(len) = self
            .active_review()
            .and_then(|state| state.diff.as_ref())
            .map(|diff| diff.len(split))
        else {
            return;
        };
        if len == 0 {
            return;
        }
        let last = len - 1;
        let current = self
            .active_review()
            .and_then(|state| state.diff_selection)
            .map(|(_, head)| head)
            .unwrap_or(0);
        let target = match jump {
            Jump::Start => 0,
            Jump::End => last,
            Jump::PageUp => current.saturating_sub(page),
            Jump::PageDown => (current + page).min(last),
        };
        self.move_diff_selection(target, target, cx);
    }

    /// Combien de lignes tiennent dans la vue.
    ///
    /// Au moins une : une vue jamais peinte n'a pas de bornes, et une page de
    /// zéro ligne ferait d'une touche un geste sans effet.
    fn page_rows(&self, cx: &App) -> usize {
        let height = self.diff_base_handle(cx).bounds().size.height;
        let line = line_height(px(crate::ui::settings::Settings::global(cx).diff_font_size));
        ((f32::from(height) / f32::from(line)) as usize).max(1)
    }

    /// Saute au hunk précédent ou suivant, et au fichier voisin une fois le
    /// dernier passé.
    ///
    /// Relire, c'est passer d'une modification à l'autre : les lignes de
    /// contexte entre deux hunks n'ont, elles, rien à montrer. Et une revue ne
    /// s'arrête pas au bout d'un fichier — la même flèche continue dans le
    /// suivant, où elle entre par le bout d'où elle vient.
    pub(super) fn step_diff_hunk(&mut self, delta: isize, cx: &mut Context<Self>) {
        let split = crate::ui::settings::Settings::global(cx).diff_split;
        let headers = self
            .active_review()
            .and_then(|state| state.diff.as_ref())
            .map(|diff| diff.headers(split))
            .unwrap_or_default();
        let from = self
            .active_review()
            .and_then(|state| state.diff_selection)
            .map(|(_, head)| head);
        match next_header(&headers, from, delta) {
            Some(target) => self.move_diff_selection(target, target, cx),
            None => self.step_file_to_a_hunk(delta, cx),
        }
    }

    /// Passe au fichier voisin et y note par quel bout entrer.
    ///
    /// La sélection ne peut pas être posée ici : le diff n'arrivera qu'après
    /// la commande git. C'est `Evt::FileDiff` qui la consomme.
    fn step_file_to_a_hunk(&mut self, delta: isize, cx: &mut Context<Self>) {
        let before = self
            .active_review()
            .and_then(|state| state.selected.clone());
        self.step_file(delta, cx);
        let Some(state) = self.active_review_mut() else {
            return;
        };
        // Rien n'a bougé — on était déjà au bout de la revue : ne pas armer un
        // saut qui s'appliquerait au prochain fichier ouvert à la souris.
        if state.selected == before {
            return;
        }
        state.pending_jump = Some(if delta > 0 {
            crate::ui::app::Jump::First
        } else {
            crate::ui::app::Jump::Last
        });
    }

    fn move_diff_selection(&mut self, anchor: usize, head: usize, cx: &mut Context<Self>) {
        let Some(state) = self.active_review_mut() else {
            return;
        };
        state.diff_selection = Some((anchor, head));
        // Défilement non strict : une ligne déjà visible ne fait pas sauter la
        // vue, ce qui laisse le regard où il est tant qu'on ne sort pas de
        // l'écran.
        self.reveal_diff_row(head, gpui::ScrollStrategy::Top, cx);
        cx.notify();
    }

    /// La molette du diff : le zoom quand la touche système est enfoncée, le
    /// défilement lissé sinon.
    ///
    /// Un seul écouteur pour les deux, et il ne peut pas en être autrement :
    /// le zoom et le lissage veulent tous deux **rendre** le saut que gpui
    /// vient d'appliquer, et deux écouteurs le rendraient deux fois.
    ///
    /// La liste a en effet **déjà** défilé quand cet écouteur s'exécute — les
    /// deux sont en phase de remontée, et l'enfant est traité avant son
    /// parent. gpui n'expose pas de phase de capture pour la molette : on rend
    /// donc le décalage plutôt que d'essayer de l'empêcher, sans quoi chaque
    /// cran de zoom ferait aussi sauter la lecture de trois lignes.
    pub(super) fn on_diff_scroll(
        &mut self,
        event: &gpui::ScrollWheelEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let handle = self.diff_base_handle(cx);
        if !event.modifiers.secondary() {
            if self
                .motion(DIFF_SCROLL.into(), crate::ui::motion::Axes::Both)
                .on_wheel(&handle, event, window)
            {
                cx.notify();
            }
            return;
        }
        // Un zoom au milieu d'un défilement lissé : la transition viserait une
        // position calculée sur des lignes qui n'ont plus la même hauteur.
        self.motion(DIFF_SCROLL.into(), crate::ui::motion::Axes::Both)
            .cancel();
        let delta = event.delta.pixel_delta(window.line_height().max(px(1.)));
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

impl ClaudhubApp {
    /// Le centre de l'écran d'édition : le fichier ouvert, ou de quoi savoir
    /// qu'il faut en choisir un.
    ///
    /// Il ne partage plus la place du diff : ce sont deux écrans, et l'onglet
    /// dit ce qu'il porte sans avoir à changer de nom.
    pub(super) fn render_editor_panel(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        match self.render_editor(window, cx) {
            Some(editor) => editor.into_any_element(),
            None => centered_message(tr!("editor-pick-a-file"), cx),
        }
    }

    /// Le centre de l'écran des bases : la console, ou de quoi savoir d'où
    /// elle s'ouvre.
    pub(super) fn render_console_panel(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        if self.db_console_open() {
            return self.render_db_console(window, cx).into_any_element();
        }
        centered_message(tr!("db-open-a-console"), cx)
    }

    pub(super) fn render_diff(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        // Les occurrences sont recalculées ici plutôt qu'à chaque endroit d'où
        // la requête peut changer : la comparaison d'une chaîne par frame est
        // le prix de n'avoir personne à prévenir.
        let query = self.query(crate::ui::find::Pane::Diff, cx);
        self.refresh_diff_search(&query);
        let find = self.render_find(crate::ui::find::Pane::Diff, cx);
        let Some(state) = self.active_review() else {
            return div().into_any_element();
        };
        let Some(path) = state.selected.clone() else {
            return centered_message(tr!("review-pick-a-file"), cx);
        };
        let stageable = state.range == DiffRange::Working;
        let diff = state.diff.clone();
        let settings = crate::ui::settings::Settings::global(cx);
        let (split, whole_file) = (settings.diff_split, settings.diff_whole_file);
        let wrap = split && settings.diff_wrap;
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
                Button::new("diff-whole-file")
                    .ghost()
                    .xsmall()
                    // L'icône dit l'état courant, comme la bascule de
                    // l'arborescence : le fichier entier, ou ses seules
                    // modifications.
                    .icon(icon(if whole_file { "file-text" } else { "file-diff" }))
                    .tooltip(if whole_file {
                        tr!("diff-hunks-only")
                    } else {
                        tr!("diff-whole-file")
                    })
                    .on_click(cx.listener(|this, _, _window, cx| this.toggle_whole_file(cx))),
            )
            .child(
                Button::new("diff-split")
                    .ghost()
                    .xsmall()
                    .icon(icon(if split { "columns-2" } else { "list" }))
                    .tooltip(if split {
                        tr!("diff-unified")
                    } else {
                        tr!("diff-split")
                    })
                    .on_click(cx.listener(|this, _, _window, cx| this.toggle_diff_split(cx))),
            )
            // Le repli n'a de sens qu'en deux colonnes : en une seule, la
            // ligne dispose de toute la largeur. Un bouton qui ne changerait
            // rien vaut mieux caché qu'inerte.
            .when(split, |el| {
                el.child(
                    Button::new("diff-wrap")
                        .ghost()
                        .xsmall()
                        .selected(wrap)
                        .icon(icon("wrap-text"))
                        .tooltip(if wrap {
                            tr!("diff-nowrap")
                        } else {
                            tr!("diff-wrap")
                        })
                        .on_click(cx.listener(|this, _, _window, cx| this.toggle_diff_wrap(cx))),
                )
            })
            .child(
                Button::new("copy-file")
                    .ghost()
                    .xsmall()
                    .icon(icon("copy"))
                    .tooltip(tr!("action-copy-file"))
                    .on_click(cx.listener(|this, _, _window, cx| this.copy_diff(false, cx))),
            )
            // Les deux gestes de la relecture annotée, à côté de la copie : ce
            // sont les trois choses qu'on fait d'une sélection de code.
            .child(
                Button::new("annotate")
                    .ghost()
                    .xsmall()
                    .icon(icon("reply"))
                    .tooltip(tr!("note-add"))
                    .on_click(
                        cx.listener(|this, _, window, cx| this.annotate_selection(window, cx)),
                    ),
            )
            .child(
                Button::new("ask-agent")
                    .ghost()
                    .xsmall()
                    .icon(icon("bot"))
                    .tooltip(tr!("note-ask-title"))
                    .on_click(
                        cx.listener(|this, _, window, cx| this.ask_about_selection(window, cx)),
                    ),
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
        // Le lissage avance d'une frame. L'ordre avec la construction de la
        // liste est libre : le décalage n'est lu qu'à la mise en page.
        let base = self.diff_base_handle(cx);
        self.motion(DIFF_SCROLL.into(), crate::ui::motion::Axes::Both)
            .advance(&base, window);
        // La largeur du contenu tient compte du viewport mesuré à la frame
        // précédente : sans ce plancher, le fond coloré d'une ligne modifiée
        // s'arrêterait au bout de son texte au lieu de traverser la vue.
        //
        // Au tout premier diff, cette mesure n'existe pas encore. On redemande
        // alors une frame : sans elle, la vue garde sa largeur de départ
        // jusqu'au prochain événement — le balayage de fond, deux secondes
        // plus tard —, ce qui se voit d'autant plus que le repli calcule ses
        // colonnes dessus. Les diffs suivants partent de la largeur retenue.
        let measured = base.bounds().size.width;
        if measured > px(1.) {
            self.diff_width = measured;
            self.diff_measures = 0;
        } else if self.diff_measures < 4 {
            self.diff_measures += 1;
            window.request_animation_frame();
        }
        let viewport = if measured > px(1.) {
            measured
        } else {
            self.diff_width
        };
        let text_width = cell * diff.longest_chars as f32 + px(24.);
        // En deux colonnes, chacune est taillée pour la plus longue ligne du
        // fichier — et non pour la moitié de la vue. Les tailler à la vue
        // couperait le code ou le renverrait à la ligne, alors que le tout
        // reste atteignable par le défilement horizontal, qui emmène les deux
        // colonnes ensemble et garde donc les versions en regard.
        // Repliées, les colonnes font la moitié de la vue et rien d'autre :
        // c'est tout l'objet du repli, ne plus avoir à défiler pour lire une
        // ligne longue.
        let column = if wrap {
            // La marge de note (3 px) appartient à l'entrée, pas aux
            // colonnes : l'oublier ferait déborder la ligne de trois pixels,
            // qu'aucune barre ne révélerait puisque le repli en supprime une.
            ((viewport - px(3.)) / 2.).max(px(80.))
        } else {
            ((text_width + gutter).max(viewport / 2.)).max(px(80.))
        };
        let content_width = if split {
            column * 2.
        } else {
            (text_width + gutter * 2.).max(viewport)
        };
        // Les colonnes de texte d'une moitié : ce qui reste une fois la
        // gouttière, le signe et la marge de note pris. Zéro quand rien ne se
        // replie, ce que `half` lit comme « laisse la ligne courir ».
        let cols = if wrap {
            (f32::from((column - gutter - px(20.)).max(px(0.))) / f32::from(cell)) as usize
        } else {
            0
        };
        let cols = if wrap { cols.max(8) } else { 0 };

        let colors = DiffColors::of(cx);
        let entity = cx.entity();
        let rows = diff.clone();
        let count = diff.len(split);
        let selection = self
            .active_review()
            .and_then(|state| state.diff_selection)
            .map(|(a, b)| (a.min(b), a.max(b)));
        let selection_bg = cx.theme().selection;
        // Les lignes annotées sont calculées à l'arrivée du diff : la
        // fermeture ci-dessous tourne pour chaque ligne visible à chaque
        // frame, et n'y a donc rien à chercher.
        let marks = self
            .active_review()
            .map(|state| state.note_marks.clone())
            .unwrap_or_default();
        let note_color = cx.theme().warning;
        // Les occurrences sont rangées par ligne à chaque changement de
        // requête ou de diff, et la fermeture ci-dessous ne fait que les
        // consulter : elle tourne pour chaque ligne visible de chaque frame.
        let search = SearchPaint {
            by_line: self.diff_search.by_line.clone(),
            current: self.diff_search.hits.get(self.diff_search.current).cloned(),
            color: crate::ui::find::highlight_color(false, cx),
            current_color: crate::ui::find::highlight_color(true, cx),
        };

        // Une entrée, quelle que soit la liste qui la demande : les deux
        // branches ci-dessous ne diffèrent que par la façon de réserver la
        // hauteur, pas par ce qu'elles peignent.
        let build = move |ix: usize, cx: &mut gpui::App| {
            let selected = selection.is_some_and(|(a, b)| ix >= a && ix <= b);
            let style = RowStyle {
                line_height,
                gutter,
                stageable,
                selected,
                selection_bg,
                annotated: marks.at(ix, split),
                note_color,
            };
            if split {
                render_split_row(
                    &rows, ix, &colors, column, cols, &style, &search, &entity, cx,
                )
            } else {
                render_row(
                    &rows,
                    ix,
                    &colors,
                    content_width,
                    &style,
                    &search,
                    &entity,
                    cx,
                )
            }
        };

        // Repliée, la vue en deux colonnes n'a plus des entrées de même
        // hauteur : une ligne longue en occupe trois, celle d'en face une
        // seule. `uniform_list` trouve l'intervalle visible par une division
        // et ne sait donc pas la peindre ; `v_virtual_list` parcourt un
        // vecteur de tailles, qu'on lui donne. C'est le seul endroit où le
        // surcoût se justifie — et il n'y a plus rien à défiler
        // horizontalement, ce que cette liste ne saurait pas faire.
        let list = if wrap {
            let heights = split_heights(&diff, cols);
            let sizes = std::rc::Rc::new(
                heights
                    .into_iter()
                    .map(|lines| gpui::size(px(0.), line_height * lines as f32))
                    .collect::<Vec<_>>(),
            );
            crate::ui::scroll::vertical(
                DIFF_SCROLL,
                &self.diff_wrap_scroll,
                v_virtual_list(
                    cx.entity(),
                    "diff-lines-wrapped",
                    sizes,
                    move |_, range, _window, cx| range.map(|ix| build(ix, cx)).collect::<Vec<_>>(),
                )
                .size_full()
                .font_family(mono)
                .text_size(font_size)
                .track_scroll(&self.diff_wrap_scroll),
            )
        } else {
            crate::ui::scroll::both(
                DIFF_SCROLL,
                &self.diff_scroll,
                uniform_list("diff-lines", count, move |range, _window, cx| {
                    range.map(|ix| build(ix, cx)).collect::<Vec<_>>()
                })
                .size_full()
                .font_family(mono)
                .text_size(font_size)
                // Sans `Unconstrained`, les lignes sont contraintes à la
                // largeur de la vue et le défilement horizontal n'a rien à
                // révéler ; la largeur défilable est déduite du seul item
                // désigné ci-dessous.
                .with_horizontal_sizing_behavior(ListHorizontalSizingBehavior::Unconstrained)
                // En deux colonnes, toutes les entrées ont la même largeur —
                // celle des deux colonnes réunies — et n'importe laquelle
                // mesure donc la bonne.
                .with_width_from_item(Some(if split { 0 } else { diff.longest_row }))
                .track_scroll(&self.diff_scroll.clone()),
            )
        };

        v_flex()
            .size_full()
            .child(header)
            .children(find)
            .child(
                div()
                    .id("diff-zoom")
                    .flex_1()
                    .min_h_0()
                    .on_scroll_wheel(cx.listener(Self::on_diff_scroll))
                    .on_mouse_up(
                        gpui::MouseButton::Left,
                        cx.listener(|this, _, _window, _cx| this.end_diff_drag()),
                    )
                    .child(list)
                    // Le clic droit porte les gestes qui n'ont pas de bouton
                    // sous la main : on vient de sélectionner des lignes, et
                    // remonter à la barre d'en-tête pour agir dessus est un
                    // aller-retour de trop.
                    .context_menu({
                        let entity = cx.entity();
                        move |menu, _window, _cx| {
                            let (note, ask) = (entity.clone(), entity.clone());
                            let edit = entity.clone();
                            let (copy, patch) = (entity.clone(), entity.clone());
                            menu.item(
                                gpui_component::menu::PopupMenuItem::new(tr!("note-add"))
                                    .icon(icon("message-square-plus"))
                                    .on_click(move |_, window, cx| {
                                        note.update(cx, |this, cx| {
                                            this.annotate_selection(window, cx)
                                        });
                                    }),
                            )
                            .item(
                                gpui_component::menu::PopupMenuItem::new(tr!("note-ask-title"))
                                    .icon(icon("bot"))
                                    .on_click(move |_, window, cx| {
                                        ask.update(cx, |this, cx| {
                                            this.ask_about_selection(window, cx)
                                        });
                                    }),
                            )
                            .item(
                                gpui_component::menu::PopupMenuItem::new(tr!("editor-external"))
                                    .icon(icon("external-link"))
                                    .on_click(move |_, _window, cx| {
                                        edit.update(cx, |this, cx| this.open_diff_externally(cx));
                                    }),
                            )
                            .separator()
                            .item(
                                gpui_component::menu::PopupMenuItem::new(tr!("action-copy-file"))
                                    .icon(icon("copy"))
                                    .on_click(move |_, _window, cx| {
                                        copy.update(cx, |this, cx| this.copy_diff(false, cx));
                                    }),
                            )
                            .item(
                                gpui_component::menu::PopupMenuItem::new(tr!("action-copy-patch"))
                                    .icon(icon("file-diff"))
                                    .on_click(move |_, _window, cx| {
                                        patch.update(cx, |this, cx| this.copy_diff(true, cx));
                                    }),
                            )
                        }
                    }),
            )
            .into_any_element()
    }
}

/// Ce que la recherche pose sur les lignes du diff.
///
/// Vide la plupart du temps, et c'est ce qui compte : sans requête,
/// `by_line` l'est aussi, `marks` rend une tranche vide sans rien allouer, et
/// la coloration passe exactement par le chemin qu'elle prenait avant.
#[derive(Clone)]
pub struct SearchPaint {
    pub by_line: crate::ui::find::MatchesByLine,
    /// L'occurrence courante, peinte plus vivement que les autres : dans un
    /// fichier qui en compte quarante, « où en suis-je » est la question.
    pub current: Option<crate::ui::find::Hit>,
    pub color: gpui::Hsla,
    pub current_color: gpui::Hsla,
}

impl SearchPaint {
    fn marks(&self, hunk: usize, line: usize) -> Vec<(std::ops::Range<usize>, gpui::Hsla)> {
        let Some(ranges) = self.by_line.get(&(hunk, line)) else {
            return Vec::new();
        };
        ranges
            .iter()
            .map(|range| {
                let current = self
                    .current
                    .as_ref()
                    .is_some_and(|hit| hit.hunk == hunk && hit.line == line && hit.range == *range);
                (
                    range.clone(),
                    if current {
                        self.current_color
                    } else {
                        self.color
                    },
                )
            })
            .collect()
    }
}

/// Ce qui change d'une entrée à l'autre sans venir du diff : l'état de la
/// sélection, l'annotation, la géométrie.
///
/// Un agrégat plutôt que huit paramètres : ils traversaient trois fonctions,
/// et le compilateur ne dit rien quand deux booléens voisins s'échangent.
pub struct RowStyle {
    pub line_height: Pixels,
    /// Largeur de la colonne des numéros de ligne.
    pub gutter: Pixels,
    pub stageable: bool,
    pub selected: bool,
    pub selection_bg: gpui::Hsla,
    /// Une note porte sur cette entrée.
    pub annotated: bool,
    pub note_color: gpui::Hsla,
}

#[allow(clippy::too_many_arguments)]
fn render_row(
    diff: &Rc<Rendered>,
    index: usize,
    colors: &DiffColors,
    content_width: Pixels,
    style: &RowStyle,
    search: &SearchPaint,
    entity: &Entity<ClaudhubApp>,
    cx: &mut gpui::App,
) -> gpui::AnyElement {
    let Some(row) = diff.rows.get(index).copied() else {
        return div().into_any_element();
    };
    let (selected, selection_bg) = (style.selected, style.selection_bg);
    match row {
        Row::Header { hunk } => {
            render_header(diff, index, hunk, colors, content_width, style, entity, cx)
        }
        Row::Line { hunk, line } => {
            let Some(source) = diff.file.hunks.get(hunk).and_then(|h| h.lines.get(line)) else {
                return div().into_any_element();
            };
            let (bg, fg) = line_colors(source.kind, colors);
            let content = line_content(diff, hunk, line, fg, &search.marks(hunk, line), None);

            let for_drag = entity.clone();
            let entity = entity.clone();
            h_flex()
                .id(("line", index))
                .h(style.line_height)
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
                .on_mouse_move(move |event, _window, cx| drag(&for_drag, index, event, cx))
                // Le marqueur d'annotation est un filet dans la marge, avant
                // les numéros : il doit se voir sans déplacer une colonne, et
                // la gouttière est la seule place que le défilement horizontal
                // ne fait pas sortir de la vue.
                .child(note_mark(style))
                .child(number(source.old_no, style.gutter, colors))
                .child(number(source.new_no, style.gutter, colors))
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

/// L'en-tête `@@ … @@`, avec ses boutons. Le même dans les deux modes : il
/// porte sur le hunk entier, qui n'a pas deux versions.
#[allow(clippy::too_many_arguments)]
fn render_header(
    diff: &Rc<Rendered>,
    index: usize,
    hunk: usize,
    colors: &DiffColors,
    content_width: Pixels,
    style: &RowStyle,
    entity: &Entity<ClaudhubApp>,
    cx: &mut gpui::App,
) -> gpui::AnyElement {
    let (selected, selection_bg, line_height, stageable) = (
        style.selected,
        style.selection_bg,
        style.line_height,
        style.stageable,
    );
    let patch = diff.patches.get(hunk).cloned().unwrap_or_default();
    let entity = entity.clone();
    let for_copy = entity.clone();
    let for_click = entity.clone();
    let for_drag = entity.clone();
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
        .on_mouse_move(move |event, _window, cx| drag(&for_drag, index, event, cx))
        .child(
            div()
                .text_color(cx.theme().muted_foreground)
                .child(SharedString::from(
                    diff.row_text(Row::Header { hunk }).to_string(),
                )),
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
        // Indexer un hunk seul n'a de sens que depuis les modifications non
        // indexées : ailleurs, ou bien tout est déjà dans l'index, ou bien on
        // regarde des commits déjà écrits.
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

fn line_colors(
    kind: DiffLineKind,
    colors: &DiffColors,
) -> (Option<gpui::Hsla>, Option<gpui::Hsla>) {
    match kind {
        DiffLineKind::Added => (Some(colors.added_bg), Some(colors.added_fg)),
        DiffLineKind::Removed => (Some(colors.removed_bg), Some(colors.removed_fg)),
        DiffLineKind::Context | DiffLineKind::NoNewline => (None, None),
    }
}

/// Le texte d'une ligne, coloré s'il l'est.
///
/// Les tabulations sont rendues telles quelles par la police : les remplacer
/// ici garderait l'alignement mais décalerait les plages de coloration,
/// calculées sur le texte d'origine.
fn line_content(
    diff: &Rc<Rendered>,
    hunk: usize,
    line: usize,
    fg: Option<gpui::Hsla>,
    marks: &[(std::ops::Range<usize>, gpui::Hsla)],
    // `span` : la tranche de colonnes à montrer, quand la ligne est repliée.
    span: Option<(usize, usize)>,
) -> gpui::AnyElement {
    let Some(source) = diff.file.hunks.get(hunk).and_then(|h| h.lines.get(line)) else {
        return div().into_any_element();
    };
    let (text, styles, marks) = match span {
        None => (
            SharedString::from(source.text.clone()),
            diff.highlights.line(hunk, line).to_vec(),
            marks.to_vec(),
        ),
        Some((from, to)) => {
            let bytes = char_span(&source.text, from, to);
            (
                SharedString::from(source.text[bytes.clone()].to_string()),
                slice_runs(diff.highlights.line(hunk, line), &bytes),
                slice_runs(marks, &bytes),
            )
        }
    };
    let (styles, marks) = (styles.as_slice(), marks.as_slice());
    if marks.is_empty() {
        return if styles.is_empty() {
            div()
                .when_some(fg, |el, fg| el.text_color(fg))
                .child(text)
                .into_any_element()
        } else {
            StyledText::new(text)
                .with_highlights(styles.iter().cloned())
                .into_any_element()
        };
    }
    // La couleur d'ajout ou de suppression reste portée par le conteneur
    // quand la grammaire n'a rien à dire : sans elle, une ligne trouvée dans
    // un fichier sans grammaire perdrait sa teinte de diff.
    div()
        .when_some(fg.filter(|_| styles.is_empty()), |el, fg| el.text_color(fg))
        .child(StyledText::new(text).with_highlights(crate::ui::highlight::overlay(styles, marks)))
        .into_any_element()
}

/// Une entrée de la vue en deux colonnes.
#[allow(clippy::too_many_arguments)]
fn render_split_row(
    diff: &Rc<Rendered>,
    index: usize,
    colors: &DiffColors,
    column: Pixels,
    cols: usize,
    style: &RowStyle,
    search: &SearchPaint,
    entity: &Entity<ClaudhubApp>,
    cx: &mut gpui::App,
) -> gpui::AnyElement {
    let Some(row) = diff.split.get(index).copied() else {
        return div().into_any_element();
    };
    let (old, new) = match row {
        SplitRow::Header { hunk, .. } => {
            return render_header(diff, index, hunk, colors, column * 2., style, entity, cx)
        }
        SplitRow::Pair { old, new } => (old, new),
    };

    // La hauteur de l'entrée est celle de la plus haute des deux moitiés :
    // c'est elle qu'on a annoncée à la liste, qui réserve exactement ce qu'on
    // lui dit.
    let lines = if cols == 0 {
        1
    } else {
        [old, new]
            .into_iter()
            .flatten()
            .map(|index| wrapped_lines(diff.row_chars.get(index).copied().unwrap_or(0), cols))
            .max()
            .unwrap_or(1)
    };
    let for_drag = entity.clone();
    let for_click = entity.clone();
    h_flex()
        .id(("pair", index))
        .h(style.line_height * lines as f32)
        .items_start()
        .whitespace_nowrap()
        .on_mouse_down(gpui::MouseButton::Left, move |event, window, cx| {
            select(&for_click, index, event.modifiers.shift, window, cx);
        })
        .on_mouse_move(move |event, _window, cx| drag(&for_drag, index, event, cx))
        .child(note_mark(style))
        .child(half(
            diff,
            old,
            Column::Old,
            colors,
            style,
            column,
            cols,
            lines,
            search,
        ))
        .child(half(
            diff,
            new,
            Column::New,
            colors,
            style,
            column,
            cols,
            lines,
            search,
        ))
        .into_any_element()
}

/// Le filet de marge d'une ligne annotée.
///
/// Toujours présent, coloré seulement quand il y a une note : une largeur qui
/// apparaît et disparaît décalerait tout le contenu d'une ligne à l'autre.
fn note_mark(style: &RowStyle) -> impl IntoElement {
    div()
        .w(px(3.))
        .flex_none()
        .h_full()
        .when(style.annotated, |el| el.bg(style.note_color))
}

/// Une moitié de ligne : son numéro, son signe et son texte.
///
/// Sans ligne à montrer — un ajout n'a rien en face —, la moitié reste vide et
/// grisée : c'est ce qui rend visible que la modification n'a pas de
/// contrepartie de ce côté-là.
/// Laquelle des deux versions une colonne montre.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Column {
    Old,
    New,
}

#[allow(clippy::too_many_arguments)]
fn half(
    diff: &Rc<Rendered>,
    row: Option<usize>,
    side: Column,
    colors: &DiffColors,
    style: &RowStyle,
    column: Pixels,
    // `cols` : colonnes de texte avant le repli, zéro quand la ligne ne se
    // replie pas et que le défilement horizontal s'en charge.
    cols: usize,
    // `lines` : lignes visibles de l'entrée, la plus haute des deux moitiés.
    lines: usize,
    search: &SearchPaint,
) -> gpui::AnyElement {
    let (gutter, selected, selection_bg) = (style.gutter, style.selected, style.selection_bg);
    let source = row
        .and_then(|index| diff.rows.get(index).copied())
        .and_then(|row| match row {
            Row::Line { hunk, line } => Some((hunk, line)),
            Row::Header { .. } => None,
        })
        .and_then(|(hunk, line)| {
            let source = diff.file.hunks.get(hunk)?.lines.get(line)?;
            Some((hunk, line, source))
        });

    let Some((hunk, line, source)) = source else {
        return div()
            .w(column)
            .flex_none()
            .h_full()
            .bg(if selected {
                selection_bg
            } else {
                colors.absent_bg
            })
            .into_any_element();
    };

    let (bg, fg) = line_colors(source.kind, colors);
    // Le numéro de *cette* version : une ligne de contexte en a deux, et
    // afficher le même des deux côtés ferait mentir la colonne de gauche dès
    // que le fichier a gagné ou perdu des lignes plus haut.
    let number_of = match side {
        Column::Old => source.old_no.or(source.new_no),
        Column::New => source.new_no.or(source.old_no),
    };
    let kind = source.kind;
    let marks = search.marks(hunk, line);
    let line_height = style.line_height;
    // Repliée, la moitié devient une pile de lignes de hauteur fixe : la
    // gouttière et le signe restent sur la première, alignés en haut, et les
    // suivantes sont la suite du texte. La hauteur de l'entrée est ainsi
    // exactement celle qu'on a annoncée à la liste.
    let wrapped = cols > 0;
    h_flex()
        .w(column)
        .flex_none()
        .h_full()
        .map(|el| {
            if wrapped {
                el.items_start()
            } else {
                el.items_center()
            }
        })
        .whitespace_nowrap()
        .overflow_hidden()
        .when_some(bg.filter(|_| !selected), |el, bg| el.bg(bg))
        .when(selected, |el| el.bg(selection_bg))
        // Une seule gouttière par colonne : chacune montre sa propre version,
        // et y répéter les deux numéros ferait payer deux fois la largeur pour
        // une information que la colonne d'en face porte déjà.
        .child(number(number_of, gutter, colors).when(wrapped, |el| el.h(line_height)))
        .child(
            div()
                .w(px(14.))
                .flex_none()
                .text_center()
                .when(wrapped, |el| el.h(line_height))
                .when_some(fg, |el, fg| el.text_color(fg))
                .child(sign(kind)),
        )
        .map(|el| {
            if !wrapped {
                return el.child(line_content(diff, hunk, line, fg, &marks, None));
            }
            // L'indice de l'entrée, que `row_chars` indexe : le retrouver en
            // parcourant `rows` coûterait un balayage du fichier par moitié
            // de ligne visible, à chaque frame.
            let chars = row
                .and_then(|index| diff.row_chars.get(index).copied())
                .unwrap_or(0);
            let own = wrapped_lines(chars, cols);
            el.child(
                v_flex()
                    .flex_1()
                    .min_w_0()
                    .children((0..lines).map(|segment| {
                        div().h(line_height).map(|el| {
                            if segment < own {
                                el.child(line_content(
                                    diff,
                                    hunk,
                                    line,
                                    fg,
                                    &marks,
                                    Some((segment * cols, (segment + 1) * cols)),
                                ))
                            } else {
                                el
                            }
                        })
                    })),
            )
        })
        .into_any_element()
}

/// Où va la sélection après une flèche.
///
/// Sans sélection, la première flèche part de l'extrémité vers laquelle elle
/// pointe : vers le bas depuis la première ligne, vers le haut depuis la
/// dernière. Aux bords, elle bute plutôt que de faire le tour — dépasser la
/// fin d'un fichier pour revenir à son début n'est jamais ce qu'on voulait.
fn step(current: Option<usize>, delta: isize, len: usize) -> Option<usize> {
    if len == 0 {
        return None;
    }
    let last = len as isize - 1;
    let next = match current {
        Some(index) => index as isize + delta,
        None if delta > 0 => 0,
        None => last,
    };
    Some(next.clamp(0, last) as usize)
}

/// L'en-tête de hunk suivant ou précédent, à partir d'une position.
fn next_header(headers: &[usize], from: Option<usize>, delta: isize) -> Option<usize> {
    match from {
        // Strictement au-delà : sans quoi, partir d'un en-tête y resterait.
        Some(index) if delta > 0 => headers.iter().find(|h| **h > index).copied(),
        Some(index) => headers.iter().rev().find(|h| **h < index).copied(),
        None if delta > 0 => headers.first().copied(),
        None => headers.last().copied(),
    }
}

/// Sélectionne une ligne **et prend le focus**.
///
/// Le second point n'est pas un détail : sans lui, cliquer une ligne laisse le
/// focus au terminal, et le `Ctrl+C` qui suit part au programme qui y tourne
/// au lieu de copier ce qu'on vient de sélectionner.
fn select(
    entity: &Entity<ClaudhubApp>,
    index: usize,
    extend: bool,
    window: &mut Window,
    cx: &mut gpui::App,
) {
    let handle = entity.read(cx).focus_handle(cx);
    window.focus(&handle, cx);
    entity.update(cx, |this, cx| {
        this.diff_dragging = true;
        this.select_diff_row(index, extend, cx);
    });
}

/// Étend la sélection au passage de la souris, bouton enfoncé.
///
/// Le bouton est revérifié ici et pas seulement à l'enfoncement : un
/// relâchement hors de la fenêtre n'envoie aucun événement, et sans cette
/// condition la sélection continuerait de suivre le curseur après coup.
fn drag(
    entity: &Entity<ClaudhubApp>,
    index: usize,
    event: &gpui::MouseMoveEvent,
    cx: &mut gpui::App,
) {
    if event.pressed_button != Some(gpui::MouseButton::Left) {
        entity.update(cx, |this, _| this.end_diff_drag());
        return;
    }
    entity.update(cx, |this, cx| this.drag_diff_row(index, cx));
}

fn number(value: Option<usize>, width: Pixels, colors: &DiffColors) -> gpui::Div {
    div()
        .w(width)
        .flex_none()
        .text_right()
        .pr_1()
        .text_color(colors.line_number)
        .child(value.map(|n| n.to_string()).unwrap_or_default())
}

/// L'état vide de la vue de diff.
///
/// Une icône et un mot au centre plutôt qu'une phrase grise en haut à gauche :
/// un panneau vide sans repère visuel se lit comme un panneau cassé, surtout
/// au premier lancement où c'est la première chose qu'on voit.
fn centered_message(text: SharedString, cx: &mut gpui::App) -> gpui::AnyElement {
    v_flex()
        .size_full()
        .items_center()
        .justify_center()
        .gap_2()
        .child(
            icon("file-diff")
                .large()
                .text_color(cx.theme().muted_foreground.opacity(0.4)),
        )
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

    /// L'appariement est toute la vue en colonnes : un bloc de suppressions
    /// suivi d'un bloc d'ajouts remet les deux versions en regard.
    #[test]
    fn the_two_columns_face_each_other() {
        let diff = FileDiff {
            hunks: vec![hunk(
                "@@ a @@",
                &[
                    DiffLineKind::Context,
                    DiffLineKind::Removed,
                    DiffLineKind::Removed,
                    DiffLineKind::Added,
                    DiffLineKind::Context,
                ],
            )],
            binary: false,
            empty: false,
        };
        let rows = rows(&diff);
        assert_eq!(
            split_rows(&diff, &rows),
            vec![
                SplitRow::Header { hunk: 0, row: 0 },
                SplitRow::Pair {
                    old: Some(1),
                    new: Some(1)
                },
                SplitRow::Pair {
                    old: Some(2),
                    new: Some(4)
                },
                // Deux suppressions pour un seul ajout : la seconde n'a rien en
                // face, et la case de droite reste vide.
                SplitRow::Pair {
                    old: Some(3),
                    new: None
                },
                SplitRow::Pair {
                    old: Some(5),
                    new: Some(5)
                },
            ]
        );
    }

    #[test]
    fn a_column_selection_comes_back_to_the_file_order() {
        let diff = FileDiff {
            hunks: vec![hunk(
                "@@ a @@",
                &[
                    DiffLineKind::Removed,
                    DiffLineKind::Added,
                    DiffLineKind::Context,
                ],
            )],
            binary: false,
            empty: false,
        };
        let rendered = Rendered::new(Path::new("x.txt"), diff, &Theme::default_dark());
        // La paire (suppression, ajout) recouvre deux lignes de la liste
        // unifiée : les copier doit rendre les deux, dans l'ordre de git.
        assert_eq!(rendered.unified_span(1, 1), Some((1, 2)));
        assert_eq!(rendered.unified_span(0, 2), Some((0, 3)));
        assert_eq!(rendered.headers(true), vec![0]);
        assert_eq!(rendered.headers(false), vec![0]);
        assert_eq!(rendered.len(false), 4);
        assert_eq!(
            rendered.len(true),
            3,
            "l'ajout et la suppression tiennent sur une entrée"
        );
    }

    #[test]
    fn arrows_stop_at_the_edges() {
        assert_eq!(step(Some(3), 1, 10), Some(4));
        assert_eq!(step(Some(0), -1, 10), Some(0), "butée haute");
        assert_eq!(step(Some(9), 1, 10), Some(9), "butée basse");
        // Sans sélection, la flèche part de l'extrémité vers laquelle elle va.
        assert_eq!(step(None, 1, 10), Some(0));
        assert_eq!(step(None, -1, 10), Some(9));
        assert_eq!(step(Some(0), 1, 0), None, "rien à parcourir");
    }

    /// `None` n'est pas un refus : c'est le signal qu'il n'y a plus de hunk
    /// dans ce fichier, et donc qu'il faut passer au voisin.
    #[test]
    fn hunk_jumps_never_stay_put() {
        let headers = [0usize, 12, 40];
        assert_eq!(next_header(&headers, Some(0), 1), Some(12));
        assert_eq!(next_header(&headers, Some(13), 1), Some(40));
        assert_eq!(
            next_header(&headers, Some(40), 1),
            None,
            "après le dernier hunk, on change de fichier"
        );
        assert_eq!(next_header(&headers, Some(13), -1), Some(12));
        assert_eq!(next_header(&headers, Some(12), -1), Some(0));
        assert_eq!(
            next_header(&headers, Some(0), -1),
            None,
            "et avant le premier"
        );
        assert_eq!(next_header(&[], None, 1), None, "un fichier sans hunk");
        assert_eq!(next_header(&headers, None, 1), Some(0));
        assert_eq!(next_header(&headers, None, -1), Some(40));
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

    /// La hauteur annoncée à la liste doit tomber juste : elle réserve
    /// exactement ce qu'on lui dit, et une ligne de trop recouvre la suivante.
    #[test]
    fn a_long_line_takes_as_many_lines_as_it_needs() {
        assert_eq!(wrapped_lines(0, 80), 1, "une ligne vide occupe sa ligne");
        assert_eq!(wrapped_lines(80, 80), 1);
        assert_eq!(wrapped_lines(81, 80), 2);
        assert_eq!(wrapped_lines(240, 80), 3);
        // Sans colonne connue — la vue n'a pas encore été peinte —, rien ne se
        // replie plutôt que de diviser par zéro.
        assert_eq!(wrapped_lines(240, 0), 1);
    }

    /// Une paire fait la hauteur de la plus haute de ses deux moitiés : les
    /// deux versions doivent rester en regard, ce qui est tout l'intérêt de
    /// cette vue.
    #[test]
    fn a_pair_is_as_tall_as_its_tallest_half() {
        let mut hunk = hunk("@@ a @@", &[DiffLineKind::Removed, DiffLineKind::Added]);
        hunk.lines[0].text = "x".repeat(10);
        hunk.lines[1].text = "x".repeat(45);
        let diff = FileDiff {
            hunks: vec![hunk],
            binary: false,
            empty: false,
        };
        let rendered = Rendered::new(Path::new("x.txt"), diff, &Theme::default_dark());
        // L'en-tête, puis la paire : la suppression tient sur une ligne, son
        // ajout en demande trois, et l'entière en fait trois.
        assert_eq!(split_heights(&rendered, 20), vec![1, 3]);
        assert_eq!(
            split_heights(&rendered, 0),
            vec![1, 1],
            "sans repli, une ligne"
        );
    }

    /// Une tranche se compte en **caractères** : en octets, une ligne
    /// accentuée se couperait une colonne trop tôt, et au milieu d'un
    /// caractère — ce qui panique.
    #[test]
    fn a_slice_counts_characters_and_not_bytes() {
        let text = "éàü1234";
        assert_eq!(char_span(text, 0, 3), 0..6);
        assert_eq!(&text[char_span(text, 0, 3)], "éàü");
        assert_eq!(&text[char_span(text, 3, 5)], "12");
        // Au-delà de la fin, la tranche s'arrête au texte.
        assert_eq!(&text[char_span(text, 5, 99)], "34");
        assert_eq!(char_span(text, 99, 120), text.len()..text.len());
    }

    /// Les plages d'une tranche restent triées et disjointes, et repartent de
    /// zéro : c'est l'invariant que gpui ne vérifie pas.
    #[test]
    fn sliced_runs_are_moved_back_to_the_start() {
        let runs = vec![(0..4, 'a'), (6..10, 'b'), (12..20, 'c')];
        assert_eq!(slice_runs(&runs, &(5..14)), vec![(1..5, 'b'), (7..9, 'c')]);
        assert!(slice_runs(&runs, &(4..6)).is_empty(), "rien à cheval");
        assert_eq!(slice_runs(&runs, &(0..2)), vec![(0..2, 'a')]);
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
