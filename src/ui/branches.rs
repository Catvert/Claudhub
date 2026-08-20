//! Panneau des branches.
//!
//! Il sert à deux choses : basculer le worktree courant sur une autre branche,
//! et créer un worktree à partir d'une branche existante — le geste de départ
//! d'une relecture, quand le travail d'un agent est arrivé sur une branche
//! qu'on n'a pas encore déployée.
//!
//! La liste est virtualisée et filtrable. Un dépôt vivant a des dizaines de
//! branches : les reconstruire toutes à chaque frame — deux boutons chacune —
//! coûte cher pour des lignes qu'on ne voit pas, et les parcourir du regard
//! pour en trouver une dont on connaît le nom est ce qu'un champ de recherche
//! évite.

use std::path::PathBuf;

use gpui::{div, prelude::*, uniform_list, Context, SharedString, Window};
use gpui_component::{
    button::{Button, ButtonVariants},
    h_flex,
    input::Input,
    v_flex, ActiveTheme, Disableable, Sizable, StyledExt,
};

use crate::git::{Branch, BranchKind};
use crate::runtime::Cmd;
use crate::tr;
use crate::ui::app::ClaudhubApp;
use crate::ui::icons::icon;

/// Une entrée de la liste.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Row {
    /// L'en-tête d'un groupe : les locales d'abord, les distantes ensuite.
    Group(BranchKind),
    Branch(BranchRow),
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct BranchRow {
    name: String,
    kind: BranchKind,
    is_head: bool,
    /// Ce que la branche porte, en une ligne : son dernier sujet et sa date.
    detail: String,
    ahead: usize,
    behind: usize,
    /// Worktree qui la détient déjà. Git refuse deux checkouts de la même
    /// branche : le dire avant d'essayer vaut mieux qu'une erreur.
    taken_by: Option<PathBuf>,
}

impl BranchRow {
    /// Ni déployable ici, ni déployable ailleurs : elle est déjà quelque part.
    fn taken(&self) -> bool {
        self.taken_by.is_some() && !self.is_head
    }
}

/// Met les branches en liste, filtrées et groupées.
///
/// Fonction libre et testée : c'est la seule décision de cette vue — laquelle
/// apparaît, sous quel groupe.
fn rows_for(branches: &[Branch], filter: &str) -> Vec<Row> {
    let needle = filter.trim().to_lowercase();
    let mut rows = Vec::new();
    for kind in [BranchKind::Local, BranchKind::Remote] {
        let matching: Vec<Row> = branches
            .iter()
            .filter(|branch| branch.kind == kind)
            .filter(|branch| needle.is_empty() || branch.name.to_lowercase().contains(&needle))
            .map(|branch| {
                Row::Branch(BranchRow {
                    name: branch.name.clone(),
                    kind: branch.kind,
                    is_head: branch.is_head,
                    detail: detail(branch),
                    ahead: branch.upstream.as_ref().map(|up| up.ahead).unwrap_or(0),
                    behind: branch.upstream.as_ref().map(|up| up.behind).unwrap_or(0),
                    taken_by: branch.checked_out_at.clone(),
                })
            })
            .collect();
        // Un groupe vide n'a pas d'en-tête : sur une recherche qui ne trouve
        // que des distantes, un titre « Locales » suivi de rien se lit comme
        // un défaut d'affichage.
        if !matching.is_empty() {
            rows.push(Row::Group(kind));
            rows.extend(matching);
        }
    }
    rows
}

/// La seconde ligne : le sujet du dernier commit, puis sa date.
///
/// Les morceaux vides sont écartés plutôt que d'être séparés par un point
/// médian qui n'entoure rien — un dépôt fraîchement cloné n'a pas toujours de
/// sujet à montrer.
fn detail(branch: &Branch) -> String {
    [branch.subject.as_str(), branch.date.as_str()]
        .into_iter()
        .filter(|part| !part.trim().is_empty())
        .collect::<Vec<_>>()
        .join(" · ")
}

impl ClaudhubApp {
    pub(super) fn render_branches(
        &mut self,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let Some(worktree) = self.active.clone() else {
            return div().into_any_element();
        };
        let Some(repo) = self.repo_of(&worktree) else {
            return div().into_any_element();
        };
        let main = repo.main.clone();
        let filter = self.branch_filter.read(cx).value().to_string();
        let rows = std::rc::Rc::new(rows_for(&repo.branches, &filter));

        let header = h_flex()
            .h(crate::ui::theme::bar_height(cx))
            .w_full()
            .px_1()
            .gap_1()
            .items_center()
            .border_b_1()
            .border_color(cx.theme().border)
            .child(
                div()
                    .flex_1()
                    .child(Input::new(&self.branch_filter).xsmall()),
            )
            .child(
                Button::new("new-branch")
                    .ghost()
                    .xsmall()
                    .icon(icon("plus"))
                    .tooltip(tr!("branch-new"))
                    .on_click(cx.listener(|this, _, window, cx| {
                        this.prompt_new_branch(window, cx);
                    })),
            );

        if rows.is_empty() {
            return v_flex()
                .size_full()
                .child(header)
                .child(
                    div()
                        .p_3()
                        .text_xs()
                        .text_color(cx.theme().muted_foreground)
                        .child(tr!("branch-none")),
                )
                .into_any_element();
        }

        let entity = cx.entity();
        let count = rows.len();
        // **Une seule hauteur pour toutes les entrées**, en-têtes de groupe
        // compris. `uniform_list` mesure un élément et réserve sa hauteur pour
        // tous : donner une hauteur d'une ligne à l'en-tête et de deux aux
        // branches faisait déborder chaque branche sur la suivante, les noms
        // venant se dessiner par-dessus les détails de la ligne précédente.
        let height = crate::ui::theme::tall_row_height(cx);

        v_flex()
            .size_full()
            .child(header)
            .child(
                div().flex_1().min_h_0().child(crate::ui::scroll::vertical(
                    "branch-list-bar",
                    &self.branch_scroll,
                    uniform_list("branch-list", count, move |visible, _window, cx| {
                        visible
                            .map(|ix| match rows.get(ix) {
                                Some(Row::Group(kind)) => render_group(*kind, height, cx),
                                Some(Row::Branch(row)) => {
                                    render_branch(row, ix, &worktree, &main, height, &entity, cx)
                                }
                                None => div().into_any_element(),
                            })
                            .collect::<Vec<_>>()
                    })
                    .size_full()
                    .track_scroll(self.branch_scroll.clone()),
                )),
            )
            .into_any_element()
    }

    fn prompt_new_branch(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.open_text_dialog(
            tr!("branch-new"),
            tr!("branch-new-placeholder"),
            window,
            cx,
            |this, name, _window, cx| {
                let name = name.trim().to_string();
                if name.is_empty() {
                    return;
                }
                let Some(worktree) = this.active.clone() else {
                    return;
                };
                this.git.send(Cmd::CreateBranch {
                    worktree,
                    name,
                    from: None,
                });
                cx.notify();
            },
        );
    }

    /// Déploie une branche existante dans un worktree neuf.
    ///
    /// Le dossier prend le nom de la branche, les barres obliques devenant des
    /// tirets : `origin/feat/x` ne peut pas être un nom de dossier.
    fn worktree_from_branch(&mut self, main: PathBuf, branch: String, cx: &mut Context<Self>) {
        let local = branch
            .strip_prefix("origin/")
            .unwrap_or(&branch)
            .to_string();
        let slug = local.replace('/', "-");
        let repo_name = main
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| "repo".into());
        let root = main
            .parent()
            .map(|p| p.join(format!("{repo_name}-wt")))
            .unwrap_or_else(|| main.join("worktrees"));
        self.git.send(Cmd::AddWorktree {
            main,
            path: root.join(slug),
            branch: local,
            from: None,
        });
        cx.notify();
    }
}

fn render_group(kind: BranchKind, height: gpui::Pixels, cx: &mut gpui::App) -> gpui::AnyElement {
    // Le titre se pose en bas de sa bande plutôt qu'en son milieu : il annonce
    // ce qui suit, et le coller à sa liste dit mieux ce qu'il regroupe qu'un
    // texte flottant au centre d'une hauteur qu'il n'occupe pas.
    h_flex()
        .h(height)
        .w_full()
        .px_2()
        .pb_1()
        .items_end()
        .bg(cx.theme().secondary)
        .text_xs()
        .font_semibold()
        .text_color(cx.theme().muted_foreground)
        .child(match kind {
            BranchKind::Local => tr!("branches-local"),
            BranchKind::Remote => tr!("branches-remote"),
        })
        .into_any_element()
}

#[allow(clippy::too_many_arguments)]
fn render_branch(
    row: &BranchRow,
    index: usize,
    worktree: &std::path::Path,
    main: &std::path::Path,
    height: gpui::Pixels,
    entity: &gpui::Entity<ClaudhubApp>,
    cx: &mut gpui::App,
) -> gpui::AnyElement {
    let muted = cx.theme().muted_foreground;
    let taken = row.taken();
    let detail = if let Some(path) = row.taken_by.as_ref().filter(|_| !row.is_head) {
        // Où elle est déployée importe plus que ce qu'elle porte : c'est ce qui
        // explique pourquoi les deux boutons sont éteints.
        format!(
            "{} {}",
            tr!("branch-checked-out"),
            path.file_name().unwrap_or_default().to_string_lossy()
        )
    } else {
        row.detail.clone()
    };

    h_flex()
        .id(("branch", index))
        .h(height)
        .w_full()
        .px_2()
        .gap_2()
        .items_center()
        .whitespace_nowrap()
        .overflow_hidden()
        .when(row.is_head, |el| el.bg(cx.theme().accent))
        .hover(|s| s.bg(cx.theme().accent.opacity(0.4)))
        .child(
            icon(match row.kind {
                BranchKind::Local => "git-branch",
                BranchKind::Remote => "download",
            })
            .xsmall()
            .text_color(muted),
        )
        .child(
            v_flex()
                .flex_1()
                .min_w_0()
                .child(
                    div()
                        .truncate()
                        .text_sm()
                        .when(row.is_head, |el| el.font_semibold())
                        .child(SharedString::from(row.name.clone())),
                )
                .when(!detail.is_empty(), |el| {
                    el.child(
                        div()
                            .truncate()
                            .text_xs()
                            .text_color(muted)
                            .child(SharedString::from(detail)),
                    )
                }),
        )
        // Le retard avant l'avance : c'est ce qu'il faut intégrer avant de
        // pouvoir pousser.
        .when(row.behind > 0, |el| {
            el.child(
                div()
                    .flex_none()
                    .text_xs()
                    .text_color(muted)
                    .child(format!("↓{}", row.behind)),
            )
        })
        .when(row.ahead > 0, |el| {
            el.child(
                div()
                    .flex_none()
                    .text_xs()
                    .text_color(muted)
                    .child(format!("↑{}", row.ahead)),
            )
        })
        .when(!row.is_head, |el| {
            let (for_checkout, for_worktree) = (row.name.clone(), row.name.clone());
            let (checkout_target, main) = (worktree.to_path_buf(), main.to_path_buf());
            let (checkout_entity, worktree_entity) = (entity.clone(), entity.clone());
            el.child(
                Button::new(("checkout", index))
                    .ghost()
                    .xsmall()
                    .icon(icon("check"))
                    .tooltip(tr!("branch-checkout"))
                    .disabled(taken)
                    .on_click(move |_, _window, cx| {
                        checkout_entity.update(cx, |this, cx| {
                            this.git.send(Cmd::Checkout {
                                worktree: checkout_target.clone(),
                                branch: for_checkout.clone(),
                            });
                            cx.notify();
                        });
                    }),
            )
            .child(
                Button::new(("worktree-from", index))
                    .ghost()
                    .xsmall()
                    .icon(icon("folder-open"))
                    .tooltip(tr!("branch-new-worktree"))
                    .disabled(taken)
                    .on_click(move |_, _window, cx| {
                        worktree_entity.update(cx, |this, cx| {
                            this.worktree_from_branch(main.clone(), for_worktree.clone(), cx)
                        });
                    }),
            )
        })
        .into_any_element()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::git::Upstream;

    fn branch(name: &str, kind: BranchKind) -> Branch {
        Branch {
            name: name.into(),
            kind,
            is_head: false,
            date: "hier".into(),
            subject: "Un commit".into(),
            author: "Zoé".into(),
            upstream: None,
            checked_out_at: None,
        }
    }

    #[test]
    fn locals_come_first_each_under_its_own_heading() {
        let branches = vec![
            branch("main", BranchKind::Local),
            branch("origin/feature", BranchKind::Remote),
            branch("wt/essai", BranchKind::Local),
        ];
        let names: Vec<String> = rows_for(&branches, "")
            .into_iter()
            .map(|row| match row {
                Row::Group(BranchKind::Local) => "== locales".into(),
                Row::Group(BranchKind::Remote) => "== distantes".into(),
                Row::Branch(row) => row.name,
            })
            .collect();
        assert_eq!(
            names,
            vec![
                "== locales",
                "main",
                "wt/essai",
                "== distantes",
                "origin/feature"
            ]
        );
    }

    #[test]
    fn the_filter_ignores_case_and_drops_empty_headings() {
        let branches = vec![
            branch("main", BranchKind::Local),
            branch("origin/Feature-X", BranchKind::Remote),
        ];
        let rows = rows_for(&branches, "feature");
        // Plus aucune locale ne correspond : son en-tête disparaît avec elle,
        // sans quoi un titre suivi de rien se lit comme un défaut d'affichage.
        assert_eq!(
            rows,
            vec![
                Row::Group(BranchKind::Remote),
                match rows_for(&branches, "")
                    .into_iter()
                    .find(|r| matches!(r, Row::Branch(b) if b.name == "origin/Feature-X"))
                {
                    Some(row) => row,
                    None => panic!("la branche distante devrait exister"),
                }
            ]
        );
    }

    #[test]
    fn the_detail_line_skips_what_it_does_not_have() {
        let mut b = branch("main", BranchKind::Local);
        assert_eq!(detail(&b), "Un commit · hier");
        b.subject = String::new();
        assert_eq!(detail(&b), "hier");
        b.date = String::new();
        assert_eq!(detail(&b), "");
    }

    #[test]
    fn divergence_comes_from_the_upstream() {
        let mut b = branch("main", BranchKind::Local);
        b.upstream = Some(Upstream {
            name: "origin/main".into(),
            ahead: 2,
            behind: 3,
        });
        let rows = rows_for(std::slice::from_ref(&b), "");
        let Some(Row::Branch(row)) = rows.into_iter().nth(1) else {
            panic!("une branche");
        };
        assert_eq!((row.ahead, row.behind), (2, 3));
    }
}
