//! Chercher dans un panneau.
//!
//! Presque tout ce que Claudhub affiche est une liste : des fichiers, des
//! branches, des commits, des issues, des notes, des lignes de diff. Une liste
//! qu'on ne peut pas interroger se parcourt du regard, et un projet Laravel en
//! a quarante mille entrées.
//!
//! **Un seul geste, deux comportements.** Là où la liste est libre de son
//! ordre, la recherche **filtre** : ce qui ne correspond pas disparaît, et il
//! reste ce qu'on cherchait. Là où l'ordre porte du sens — le diff, qui est le
//! fichier ; l'historique, dont le graphe relie une ligne à ses voisines —
//! elle **saute** d'une occurrence à l'autre sans rien retirer. Filtrer un
//! graphe de commits ferait pointer chaque trait sur la mauvaise ligne.
//!
//! **La casse est déduite de la requête** (`smart case`) : une requête tout en
//! minuscules ignore la casse, une requête qui porte une majuscule la respecte.
//! C'est la convention de tous les éditeurs, et elle évite un bouton de plus
//! pour un réglage qu'on change à chaque recherche.

use std::collections::HashMap;
use std::ops::Range;

use gpui::{div, prelude::*, Context, Entity, Focusable, SharedString, Window};
use gpui_component::{
    button::{Button, ButtonVariants},
    h_flex,
    input::{Input, InputEvent, InputState},
    ActiveTheme, Sizable,
};

use crate::tr;
use crate::ui::app::ClaudhubApp;
use crate::ui::icons::icon;

/// Les panneaux qui savent chercher.
///
/// Le terminal n'y est pas : son contenu est l'écran d'un programme, qui a
/// son propre `Ctrl+F` — et l'historique d'une grille alacritty n'est pas une
/// liste que nous tenions.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Pane {
    Sidebar,
    Files,
    Db,
    Changes,
    Branch,
    Branches,
    History,
    Notes,
    Sentry,
    Conflicts,
    Diff,
    /// L'éditeur intégré. Il a la recherche d'`InputState`, pas la nôtre —
    /// mais il lui faut sa clé comme aux autres, ne serait-ce que pour que
    /// `Ctrl+F` ne parte pas au panneau touché avant lui.
    Editor,
    /// La console SQL. Même chose : c'est l'éditeur de requête qui cherche.
    Console,
}

impl Pane {
    /// Le panneau saute-t-il d'une occurrence à l'autre au lieu de filtrer.
    fn jumps(self) -> bool {
        matches!(self, Pane::Diff | Pane::History)
    }

    /// Ce que la barre annonce quand elle est vide.
    fn placeholder(self) -> SharedString {
        match self {
            Pane::Diff => tr!("find-in-diff"),
            Pane::History => tr!("find-in-history"),
            _ => tr!("find-placeholder"),
        }
    }
}

/// La recherche d'un panneau : son champ, et s'il est déployé.
pub struct Finder {
    /// Créé **une fois**, à la première ouverture. Recréé au rendu, il
    /// perdrait curseur et texte dès la première frappe.
    pub input: Entity<InputState>,
    pub open: bool,
}

/// La requête correspond-elle au texte ?
pub fn matches(query: &str, haystack: &str) -> bool {
    let query = query.trim();
    if query.is_empty() {
        return true;
    }
    first_match(query, haystack, 0).is_some()
}

/// Toutes les occurrences, en décalages d'**octets** — c'est ce que gpui
/// attend pour styler un fragment de texte, et indexer en caractères casse dès
/// le premier accent.
pub fn find_all(query: &str, haystack: &str) -> Vec<Range<usize>> {
    let query = query.trim();
    let mut out = Vec::new();
    if query.is_empty() {
        return out;
    }
    let mut from = 0;
    while let Some(range) = first_match(query, haystack, from) {
        // Une occurrence vide ferait boucler : `first_match` n'en rend pas,
        // la requête n'étant jamais vide ici.
        from = range.end;
        out.push(range);
    }
    out
}

/// La première occurrence à partir d'un décalage.
///
/// Une comparaison caractère à caractère plutôt qu'une recherche dans
/// `to_lowercase()` : la mise en minuscules change la longueur en octets de
/// certains caractères, et les décalages rendus ne désigneraient plus rien
/// dans le texte d'origine.
fn first_match(query: &str, haystack: &str, from: usize) -> Option<Range<usize>> {
    let sensitive = query.chars().any(char::is_uppercase);
    let first = query.chars().next()?;
    for (start, candidate) in haystack.char_indices().skip_while(|(i, _)| *i < from) {
        if !same(candidate, first, sensitive) {
            continue;
        }
        let mut end = start;
        let mut hay = haystack[start..].chars();
        let mut ok = true;
        for wanted in query.chars() {
            match hay.next() {
                Some(c) if same(c, wanted, sensitive) => end += c.len_utf8(),
                _ => {
                    ok = false;
                    break;
                }
            }
        }
        if ok {
            return Some(start..end);
        }
    }
    None
}

fn same(a: char, b: char, sensitive: bool) -> bool {
    if sensitive {
        a == b
    } else {
        a == b || a.to_lowercase().eq(b.to_lowercase())
    }
}

impl ClaudhubApp {
    /// La requête d'un panneau, vide tant que sa barre est fermée.
    ///
    /// Vide et non `None` : les appelants filtrent tous de la même façon, et
    /// une requête vide ne retire rien.
    pub(super) fn query(&self, pane: Pane, cx: &gpui::App) -> String {
        self.finders
            .get(&pane)
            .filter(|finder| finder.open)
            .map(|finder| finder.input.read(cx).value().to_string())
            .unwrap_or_default()
    }

    /// Note le panneau où le geste a eu lieu. C'est lui que `Ctrl+F` visera.
    pub(super) fn touch_pane(&mut self, pane: Pane, cx: &mut Context<Self>) {
        if self.pane != pane {
            self.pane = pane;
            cx.notify();
        }
    }

    /// Ouvre la barre du panneau visé et lui donne le focus.
    pub(super) fn open_find(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let pane = self.pane;
        // Le panneau des branches a déjà son filtre à demeure : lui en poser
        // un second au-dessus donnerait deux champs qui font la même chose.
        if pane == Pane::Branches {
            self.branch_filter.focus_handle(cx).focus(window, cx);
            return;
        }
        let input = match self.finders.get_mut(&pane) {
            Some(finder) => {
                finder.open = true;
                finder.input.clone()
            }
            None => {
                let placeholder = pane.placeholder();
                let input = cx.new(|cx| InputState::new(window, cx).placeholder(placeholder));
                // Une frappe change la liste affichée : sans cette
                // souscription, le panneau garderait l'image d'avant.
                cx.subscribe(&input, move |this, _, event, cx| match event {
                    InputEvent::Change => {
                        if pane.jumps() {
                            this.find_reset(pane);
                        }
                        cx.notify();
                    }
                    // Entrée passe à l'occurrence suivante. Une liaison
                    // clavier ne conviendrait pas : le champ traite la touche
                    // avant elle.
                    InputEvent::PressEnter { .. } => this.find_step(1, cx),
                    _ => {}
                })
                .detach();
                self.finders.insert(
                    pane,
                    Finder {
                        input: input.clone(),
                        open: true,
                    },
                );
                input
            }
        };
        input.focus_handle(cx).focus(window, cx);
        cx.notify();
    }

    /// Referme la barre du panneau visé.
    pub(super) fn close_find(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(finder) = self.finders.get_mut(&self.pane) {
            finder.open = false;
        }
        // Le focus retourne à la vue : le laisser dans un champ qu'on vient de
        // masquer rendrait les flèches de relecture inertes.
        self.focus_handle(cx).focus(window, cx);
        cx.notify();
    }

    /// L'occurrence courante change dans les panneaux qui sautent.
    pub(super) fn find_step(&mut self, delta: isize, cx: &mut Context<Self>) {
        match self.pane {
            Pane::Diff => self.step_diff_match(delta, cx),
            Pane::History => self.step_history_match(delta, cx),
            _ => {}
        }
    }

    fn find_reset(&mut self, pane: Pane) {
        if pane == Pane::Diff {
            self.diff_search.valid = false;
        }
    }

    /// La barre de recherche d'un panneau, quand elle est déployée.
    ///
    /// Elle se pose sous l'en-tête du panneau et non par-dessus la liste : un
    /// bandeau flottant recouvrirait les premières entrées, qui sont
    /// justement celles qu'une recherche fait remonter.
    pub(super) fn render_find(
        &mut self,
        pane: Pane,
        cx: &mut Context<Self>,
    ) -> Option<impl IntoElement> {
        let finder = self.finders.get(&pane).filter(|finder| finder.open)?;
        let input = finder.input.clone();
        let query = input.read(cx).value().to_string();
        let count = self.find_count(pane, &query);

        Some(
            h_flex()
                // Le contexte n'existe que sous cette barre : `Échap` ferme la
                // recherche ici et n'a rien à fermer ailleurs.
                .key_context(crate::ui::shortcuts::find_context())
                .h(crate::ui::theme::bar_height(cx))
                .w_full()
                .px_1()
                .gap_1()
                .items_center()
                .border_b_1()
                .border_color(cx.theme().border)
                .bg(cx.theme().secondary)
                .child(icon("search").xsmall())
                .child(div().flex_1().child(Input::new(&input).xsmall()))
                .when_some(count, |el, (current, total)| {
                    el.child(
                        div()
                            .px_1()
                            .text_xs()
                            .text_color(cx.theme().muted_foreground)
                            .child(SharedString::from(if total == 0 {
                                tr!("find-no-match").to_string()
                            } else {
                                format!("{}/{total}", current + 1)
                            })),
                    )
                })
                .when(pane.jumps(), |el| {
                    el.child(
                        Button::new("find-prev")
                            .ghost()
                            .xsmall()
                            .icon(icon("arrow-up"))
                            .tooltip(tr!("find-previous"))
                            .on_click(cx.listener(|this, _, _window, cx| this.find_step(-1, cx))),
                    )
                    .child(
                        Button::new("find-next")
                            .ghost()
                            .xsmall()
                            .icon(icon("arrow-down"))
                            .tooltip(tr!("find-next"))
                            .on_click(cx.listener(|this, _, _window, cx| this.find_step(1, cx))),
                    )
                })
                .child(
                    Button::new("find-close")
                        .ghost()
                        .xsmall()
                        .icon(icon("x"))
                        .tooltip(tr!("find-close"))
                        .on_click(cx.listener(|this, _, window, cx| this.close_find(window, cx))),
                ),
        )
    }

    /// Le compte affiché par la barre.
    ///
    /// Seul le diff le porte : c'est la seule liste dont on ne voie pas
    /// l'effet de la recherche — un filtre laisse ce qu'il a trouvé sous les
    /// yeux, alors qu'une occurrence peut être à quatre mille lignes de là.
    /// L'historique, lui, éteint ce qui ne correspond pas : le compte se lit
    /// à l'écran.
    fn find_count(&mut self, pane: Pane, query: &str) -> Option<(usize, usize)> {
        if pane != Pane::Diff || query.trim().is_empty() {
            return None;
        }
        self.refresh_diff_search(query);
        Some((self.diff_search.current, self.diff_search.hits.len()))
    }
}

/// Les occurrences trouvées dans le diff affiché.
///
/// Elles sont calculées à chaque changement de requête et à chaque arrivée de
/// diff, **jamais au rendu** : la fermeture d'une liste virtualisée tourne
/// pour chaque ligne visible à chaque frame.
#[derive(Default)]
pub struct DiffSearch {
    /// La requête pour laquelle `hits` a été calculé.
    pub query: String,
    /// Faux quand un nouveau diff est arrivé : les décalages portent sur un
    /// texte qui n'est plus à l'écran.
    pub valid: bool,
    /// Les occurrences dans l'ordre du fichier.
    pub hits: std::rc::Rc<Vec<Hit>>,
    /// Les mêmes, rangées par ligne : c'est ainsi que le rendu les consulte,
    /// et il le fait pour chaque ligne visible.
    pub by_line: MatchesByLine,
    pub current: usize,
}

/// Les occurrences d'une ligne, rangées par `(hunk, ligne)`.
pub type MatchesByLine = std::rc::Rc<HashMap<(usize, usize), Vec<Range<usize>>>>;

/// Une occurrence : la ligne du diff où elle est, et sa place dans son texte.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Hit {
    pub hunk: usize,
    pub line: usize,
    pub range: Range<usize>,
}

/// Le fond d'une occurrence, posé par-dessus la coloration syntaxique.
pub fn highlight_color(current: bool, cx: &gpui::App) -> gpui::Hsla {
    if current {
        cx.theme().warning
    } else {
        cx.theme().warning.opacity(0.35)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_all_lowercase_query_ignores_case() {
        assert!(matches("todo", "TODO: rewrite"));
        assert!(matches("REWRITE", "TODO: REWRITE"));
    }

    #[test]
    fn a_query_with_a_capital_respects_it() {
        assert!(!matches("Todo", "todo: rewrite"));
        assert!(matches("Todo", "Todo: rewrite"));
    }

    #[test]
    fn an_empty_query_matches_everything() {
        assert!(matches("", "n'importe quoi"));
        assert!(matches("   ", "n'importe quoi"));
        assert!(find_all("", "n'importe quoi").is_empty());
    }

    /// Les décalages sont en octets : une recherche insensible à la casse ne
    /// doit pas les décaler d'un accent.
    #[test]
    fn offsets_are_byte_offsets_even_past_an_accent() {
        let text = "été chaud";
        let hits = find_all("chaud", text);
        assert_eq!(hits, vec![6..11]);
        assert_eq!(&text[hits[0].clone()], "chaud");
    }

    #[test]
    fn a_repeated_needle_is_found_every_time() {
        assert_eq!(find_all("ab", "abcab"), vec![0..2, 3..5]);
    }

    /// Deux occurrences qui se chevauchent ne sont pas rendues deux fois : les
    /// plages doivent rester disjointes pour que gpui les accepte.
    #[test]
    fn overlapping_occurrences_do_not_overlap_in_the_result() {
        assert_eq!(find_all("aa", "aaaa"), vec![0..2, 2..4]);
    }

    #[test]
    fn a_needle_longer_than_the_line_is_not_found() {
        assert!(find_all("abcdef", "abc").is_empty());
    }
}
