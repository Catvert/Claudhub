//! Le sélecteur de base de comparaison.
//!
//! Une entrée n'est pas qu'un nom. Choisir entre `dev`, `origin/dev` et
//! `wt/dev-2` demande de savoir laquelle a bougé en dernier et ce qu'elle
//! porte — sans quoi il faut sortir de Claudhub pour interroger git avant de
//! pouvoir cliquer. Ces informations sont déjà lues avec la liste des
//! branches : les afficher ne coûte aucune commande de plus.
//!
//! Elles sont montrées dans l'entrée elle-même plutôt que dans une bulle
//! d'aide : on parcourt une liste du regard, et une information qui demande de
//! s'arrêter sur chaque ligne pour la révéler n'aide pas à comparer.

use gpui::{div, prelude::*, px, App, IntoElement, SharedString, Window};
use gpui_component::{h_flex, select::SelectItem, v_flex, ActiveTheme};

use crate::git::{Branch, BranchKind};
use crate::tr;

/// Une branche telle que le sélecteur la propose.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BaseChoice {
    pub name: SharedString,
    pub subject: SharedString,
    pub author: SharedString,
    pub date: SharedString,
    pub remote: bool,
    /// Vrai quand c'est la branche déployée dans le worktree qu'on regarde :
    /// la comparer à elle-même ne donnerait rien.
    pub is_head: bool,
}

impl BaseChoice {
    pub fn of(branch: &Branch) -> Self {
        Self {
            name: SharedString::from(branch.name.clone()),
            subject: SharedString::from(branch.subject.clone()),
            author: SharedString::from(branch.author.clone()),
            date: SharedString::from(branch.date.clone()),
            remote: branch.kind == BranchKind::Remote,
            is_head: branch.is_head,
        }
    }

    /// La seconde ligne : ce que la branche porte, et depuis quand.
    ///
    /// Les morceaux vides sont écartés plutôt que d'être remplacés par un
    /// tiret : un dépôt fraîchement cloné n'a pas d'auteur relatif à montrer,
    /// et une ponctuation qui n'entoure rien se lit comme une donnée perdue.
    pub fn detail(&self) -> String {
        [
            self.subject.as_ref(),
            self.author.as_ref(),
            self.date.as_ref(),
        ]
        .into_iter()
        .filter(|part| !part.trim().is_empty())
        .collect::<Vec<_>>()
        .join(" · ")
    }
}

impl SelectItem for BaseChoice {
    type Value = SharedString;

    fn title(&self) -> SharedString {
        self.name.clone()
    }

    fn value(&self) -> &Self::Value {
        &self.name
    }

    fn render(&self, _: &mut Window, cx: &mut App) -> impl IntoElement {
        let muted = cx.theme().muted_foreground;
        let detail = self.detail();
        v_flex()
            .w_full()
            .min_w_0()
            .gap_0p5()
            .child(
                h_flex()
                    .w_full()
                    .min_w_0()
                    .gap_1()
                    .items_center()
                    .child(div().flex_1().min_w_0().truncate().child(self.name.clone()))
                    .when(self.remote, |el| el.child(tag(tr!("branch-remote"), cx)))
                    .when(self.is_head, |el| el.child(tag(tr!("branch-here"), cx))),
            )
            .when(!detail.is_empty(), |el| {
                el.child(
                    div()
                        .w_full()
                        .min_w_0()
                        .truncate()
                        .text_xs()
                        .text_color(muted)
                        .child(detail),
                )
            })
    }
}

fn tag(label: SharedString, cx: &App) -> impl IntoElement {
    div()
        .flex_none()
        .px_1()
        .rounded(cx.theme().radius)
        .bg(cx.theme().secondary)
        .text_xs()
        .text_color(cx.theme().muted_foreground)
        .child(label)
}

/// Largeur du menu. Deux lignes de texte demandent de la place ; sous cette
/// largeur, le sujet du commit est tronqué au point de ne plus rien dire.
pub const MENU_WIDTH: gpui::Pixels = px(420.);

#[cfg(test)]
mod tests {
    use super::*;

    fn choice(subject: &str, author: &str, date: &str) -> BaseChoice {
        BaseChoice {
            name: "dev".into(),
            subject: subject.to_string().into(),
            author: author.to_string().into(),
            date: date.to_string().into(),
            remote: false,
            is_head: false,
        }
    }

    #[test]
    fn the_detail_line_joins_what_it_has() {
        assert_eq!(
            choice("Corrige le rendu", "Zoé", "il y a 2 heures").detail(),
            "Corrige le rendu · Zoé · il y a 2 heures"
        );
    }

    #[test]
    fn an_empty_part_does_not_leave_its_punctuation_behind() {
        // Une branche sans auteur lisible ne doit pas produire « sujet ·  · hier ».
        assert_eq!(choice("Sujet", "", "hier").detail(), "Sujet · hier");
        assert_eq!(choice("", "", "").detail(), "");
    }
}
