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
        self.diff_scroll
            .scroll_to_item(head, gpui::ScrollStrategy::Top);
        cx.notify();
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
        let settings = crate::ui::settings::Settings::global(cx);
        let (split, whole_file) = (settings.diff_split, settings.diff_whole_file);
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
        let text_width = cell * diff.longest_chars as f32 + px(24.);
        // En deux colonnes, chacune est taillée pour la plus longue ligne du
        // fichier — et non pour la moitié de la vue. Les tailler à la vue
        // couperait le code ou le renverrait à la ligne, alors que le tout
        // reste atteignable par le défilement horizontal, qui emmène les deux
        // colonnes ensemble et garde donc les versions en regard.
        let column = ((text_width + gutter).max(viewport / 2.)).max(px(80.));
        let content_width = if split {
            column * 2.
        } else {
            (text_width + gutter * 2.).max(viewport)
        };

        let colors = DiffColors::of(cx);
        let entity = cx.entity();
        let rows = diff.clone();
        let count = diff.len(split);
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
                    .on_mouse_up(
                        gpui::MouseButton::Left,
                        cx.listener(|this, _, _window, _cx| this.end_diff_drag()),
                    )
                    .child(
                        uniform_list("diff-lines", count, move |range, _window, cx| {
                            range
                                .map(|ix| {
                                    let selected =
                                        selection.is_some_and(|(a, b)| ix >= a && ix <= b);
                                    if split {
                                        render_split_row(
                                            &rows,
                                            ix,
                                            &colors,
                                            gutter,
                                            column,
                                            line_height,
                                            stageable,
                                            selected,
                                            selection_bg,
                                            &entity,
                                            cx,
                                        )
                                    } else {
                                        render_row(
                                            &rows,
                                            ix,
                                            &colors,
                                            gutter,
                                            content_width,
                                            line_height,
                                            stageable,
                                            selected,
                                            selection_bg,
                                            &entity,
                                            cx,
                                        )
                                    }
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
                        // En deux colonnes, toutes les entrées ont la même
                        // largeur — celle des deux colonnes réunies — et
                        // n'importe laquelle mesure donc la bonne.
                        .with_width_from_item(Some(if split { 0 } else { diff.longest_row }))
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
        Row::Header { hunk } => render_header(
            diff,
            index,
            hunk,
            colors,
            content_width,
            line_height,
            stageable,
            selected,
            selection_bg,
            entity,
            cx,
        ),
        Row::Line { hunk, line } => {
            let Some(source) = diff.file.hunks.get(hunk).and_then(|h| h.lines.get(line)) else {
                return div().into_any_element();
            };
            let (bg, fg) = line_colors(source.kind, colors);
            let content = line_content(diff, hunk, line, fg);

            let for_drag = entity.clone();
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
                .on_mouse_move(move |event, _window, cx| drag(&for_drag, index, event, cx))
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

/// L'en-tête `@@ … @@`, avec ses boutons. Le même dans les deux modes : il
/// porte sur le hunk entier, qui n'a pas deux versions.
#[allow(clippy::too_many_arguments)]
fn render_header(
    diff: &Rc<Rendered>,
    index: usize,
    hunk: usize,
    colors: &DiffColors,
    content_width: Pixels,
    line_height: Pixels,
    stageable: bool,
    selected: bool,
    selection_bg: gpui::Hsla,
    entity: &Entity<PerchApp>,
    cx: &mut gpui::App,
) -> gpui::AnyElement {
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
) -> gpui::AnyElement {
    let Some(source) = diff.file.hunks.get(hunk).and_then(|h| h.lines.get(line)) else {
        return div().into_any_element();
    };
    let text = SharedString::from(source.text.clone());
    let styles = diff.highlights.line(hunk, line);
    if styles.is_empty() {
        div()
            .when_some(fg, |el, fg| el.text_color(fg))
            .child(text)
            .into_any_element()
    } else {
        StyledText::new(text)
            .with_highlights(styles.iter().cloned())
            .into_any_element()
    }
}

/// Une entrée de la vue en deux colonnes.
#[allow(clippy::too_many_arguments)]
fn render_split_row(
    diff: &Rc<Rendered>,
    index: usize,
    colors: &DiffColors,
    gutter: Pixels,
    column: Pixels,
    line_height: Pixels,
    stageable: bool,
    selected: bool,
    selection_bg: gpui::Hsla,
    entity: &Entity<PerchApp>,
    cx: &mut gpui::App,
) -> gpui::AnyElement {
    let Some(row) = diff.split.get(index).copied() else {
        return div().into_any_element();
    };
    let (old, new) = match row {
        SplitRow::Header { hunk, .. } => {
            return render_header(
                diff,
                index,
                hunk,
                colors,
                column * 2.,
                line_height,
                stageable,
                selected,
                selection_bg,
                entity,
                cx,
            )
        }
        SplitRow::Pair { old, new } => (old, new),
    };

    let for_drag = entity.clone();
    let for_click = entity.clone();
    h_flex()
        .id(("pair", index))
        .h(line_height)
        .items_center()
        .whitespace_nowrap()
        .on_mouse_down(gpui::MouseButton::Left, move |event, window, cx| {
            select(&for_click, index, event.modifiers.shift, window, cx);
        })
        .on_mouse_move(move |event, _window, cx| drag(&for_drag, index, event, cx))
        .child(half(
            diff,
            old,
            Column::Old,
            colors,
            gutter,
            column,
            selected,
            selection_bg,
        ))
        .child(half(
            diff,
            new,
            Column::New,
            colors,
            gutter,
            column,
            selected,
            selection_bg,
        ))
        .into_any_element()
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
    gutter: Pixels,
    column: Pixels,
    selected: bool,
    selection_bg: gpui::Hsla,
) -> gpui::AnyElement {
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
    h_flex()
        .w(column)
        .flex_none()
        .h_full()
        .items_center()
        .whitespace_nowrap()
        .overflow_hidden()
        .when_some(bg.filter(|_| !selected), |el, bg| el.bg(bg))
        .when(selected, |el| el.bg(selection_bg))
        // Une seule gouttière par colonne : chacune montre sa propre version,
        // et y répéter les deux numéros ferait payer deux fois la largeur pour
        // une information que la colonne d'en face porte déjà.
        .child(number(number_of, gutter, colors))
        .child(
            div()
                .w(px(14.))
                .flex_none()
                .text_center()
                .when_some(fg, |el, fg| el.text_color(fg))
                .child(sign(kind)),
        )
        .child(line_content(diff, hunk, line, fg))
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
    entity: &Entity<PerchApp>,
    index: usize,
    extend: bool,
    window: &mut Window,
    cx: &mut gpui::App,
) {
    let handle = entity.read(cx).focus_handle(cx);
    window.focus(&handle);
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
fn drag(entity: &Entity<PerchApp>, index: usize, event: &gpui::MouseMoveEvent, cx: &mut gpui::App) {
    if event.pressed_button != Some(gpui::MouseButton::Left) {
        entity.update(cx, |this, _| this.end_diff_drag());
        return;
    }
    entity.update(cx, |this, cx| this.drag_diff_row(index, cx));
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
