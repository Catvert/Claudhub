//! La revue : liste des fichiers touchés, et le diff du fichier choisi.
//!
//! Quatre domaines de comparaison, choisis par les onglets en tête de liste :
//! les modifications non indexées, l'index, tout le checkout contre HEAD, et
//! la branche entière depuis sa divergence d'avec sa base. Le dernier est
//! celui qui sert à relire le travail d'un agent avant de le pousser.

use std::path::{Path, PathBuf};

use gpui::{div, prelude::*, px, uniform_list, Context, SharedString, Window};
use gpui_component::{
    button::{Button, ButtonVariants},
    h_flex,
    input::Input,
    v_flex, ActiveTheme, Disableable, Selectable, Sizable, WindowExt,
};

use crate::git::{DiffFile, DiffRange, Status, StatusCode};
use crate::runtime::Cmd;
use crate::tr;
use crate::ui::app::PerchApp;
use crate::ui::icons::icon;
use crate::ui::theme::{status_color, DiffColors};

/// Ce que montre une entrée de la liste : le fichier, ses deux codes d'état,
/// et de quel côté il peut basculer.
struct Row {
    path: PathBuf,
    name: String,
    directory: String,
    code: StatusCode,
    added: usize,
    removed: usize,
    staged: bool,
}

impl PerchApp {
    pub(super) fn render_file_list(
        &mut self,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let Some(worktree) = self.active.clone() else {
            return v_flex()
                .size_full()
                .items_center()
                .justify_center()
                .child(
                    div()
                        .text_sm()
                        .text_color(cx.theme().muted_foreground)
                        .child(tr!("no-worktree")),
                )
                .into_any_element();
        };

        let Some(state) = self.review.get(&worktree) else {
            return div().into_any_element();
        };
        let range = state.range.clone();
        let selected = state.selected.clone();
        let rows = self.rows(cx);
        let staged_count = rows.iter().filter(|r| r.staged).count();
        let can_commit = staged_count > 0;

        v_flex()
            .size_full()
            .border_r_1()
            .border_color(cx.theme().border)
            .child(self.render_range_tabs(&range, cx))
            .child(
                div()
                    .flex_1()
                    .min_h_0()
                    .when(rows.is_empty(), |el| {
                        el.child(
                            div()
                                .p_3()
                                .text_xs()
                                .text_color(cx.theme().muted_foreground)
                                .child(tr!("review-clean")),
                        )
                    })
                    // Liste virtualisée : une revue de branche touche couramment
                    // plusieurs centaines de fichiers, et reconstruire autant de
                    // lignes — chacune avec ses deux boutons — à chaque frame suffit
                    // à faire tomber l'interface à quelques images par seconde.
                    .when(!rows.is_empty(), |el| {
                        let rows = std::rc::Rc::new(rows);
                        let entity = cx.entity();
                        let colors = DiffColors::of(cx);
                        let count = rows.len();
                        el.child(
                            uniform_list("file-list", count, move |range, _window, cx| {
                                range
                                    .map(|ix| {
                                        render_file_row(
                                            &rows,
                                            ix,
                                            &worktree,
                                            selected.as_deref(),
                                            &colors,
                                            &entity,
                                            cx,
                                        )
                                    })
                                    .collect::<Vec<_>>()
                            })
                            .size_full()
                            .track_scroll(self.file_scroll.clone()),
                        )
                    }),
            )
            .child(self.render_commit_box(can_commit, staged_count, cx))
            .into_any_element()
    }

    /// Les entrées de la liste pour le domaine courant.
    ///
    /// Le statut est la source pour les deux premiers onglets — lui seul
    /// distingue index et répertoire de travail — et `--numstat` pour les deux
    /// autres, qui portent sur des commits et n'ont pas de notion d'index.
    fn rows(&self, _cx: &Context<Self>) -> Vec<Row> {
        match self.active_review() {
            Some(state) => rows_for(&state.range, &state.status, &state.files),
            None => Vec::new(),
        }
    }

    fn render_range_tabs(&self, range: &DiffRange, cx: &mut Context<Self>) -> impl IntoElement {
        // La base vient de git, jamais d'un nom supposé : proposer « main » à
        // un dépôt qui s'appelle autrement produit un `unknown revision` au
        // premier clic. Tant qu'elle est inconnue — ou que c'est la branche
        // déployée ici, qui n'aurait rien à se comparer — l'onglet reste
        // présent mais inactif, plutôt que de disparaître et de faire sauter
        // les trois autres.
        let base = self.active_review().and_then(|r| r.base.clone());
        let branch_range = base
            .clone()
            .map(|base| DiffRange::Branch { base })
            .unwrap_or(DiffRange::Head);
        let branch_label = match &base {
            Some(base) => tr!("range-branch", { base: base }),
            None => tr!("range-branch-none"),
        };
        // Les deux premiers onglets portent leur compte. C'est ce qui évite
        // d'ouvrir Perch sur une liste vide sans comprendre que tout attend
        // dans l'index : le nombre se lit sans cliquer. Les deux autres
        // portent sur des commits et leur compte demanderait une commande git
        // de plus par onglet et par rafraîchissement, pour une information
        // dont on ne se sert pas au même moment.
        let (unstaged, staged) = self
            .active_review()
            .map(|r| (r.status.unstaged().count(), r.status.staged().count()))
            .unwrap_or((0, 0));
        let count = |label: SharedString, n: usize| -> SharedString {
            if n == 0 {
                label
            } else {
                SharedString::from(format!("{label} {n}"))
            }
        };
        let tabs: [(DiffRange, SharedString, bool); 4] = [
            (
                DiffRange::Unstaged,
                count(tr!("range-unstaged"), unstaged),
                true,
            ),
            (DiffRange::Staged, count(tr!("range-staged"), staged), true),
            (DiffRange::Head, tr!("range-head"), true),
            (branch_range, branch_label, base.is_some()),
        ];
        h_flex()
            .h(px(30.))
            .w_full()
            .px_1()
            .gap_1()
            .items_center()
            .border_b_1()
            .border_color(cx.theme().border)
            .children(
                tabs.into_iter()
                    .enumerate()
                    .map(|(ix, (target, label, enabled))| {
                        let selected = enabled && *range == target;
                        Button::new(("range", ix))
                            .ghost()
                            .xsmall()
                            .label(label)
                            .selected(selected)
                            .disabled(!enabled)
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.set_range(target.clone(), cx);
                            }))
                    }),
            )
    }

    fn render_commit_box(
        &self,
        can_commit: bool,
        staged: usize,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        v_flex()
            .w_full()
            .p_2()
            .gap_1()
            .border_t_1()
            .border_color(cx.theme().border)
            .child(Input::new(&self.commit_input).h(px(64.)))
            .child(
                h_flex()
                    .gap_2()
                    .items_center()
                    .child(
                        div()
                            .flex_1()
                            .text_xs()
                            .text_color(cx.theme().muted_foreground)
                            .child(tr!("commit-staged-count", { count: staged })),
                    )
                    .child(
                        Button::new("commit")
                            .primary()
                            .xsmall()
                            .icon(icon("git-commit-horizontal"))
                            .label(tr!("action-commit"))
                            .disabled(!can_commit)
                            .on_click(cx.listener(|this, _, _, cx| this.commit(false, cx))),
                    ),
            )
    }

    /// Valide ce qui est dans l'index. `amend` reprend le commit précédent.
    pub(super) fn commit(&mut self, amend: bool, cx: &mut Context<Self>) {
        let Some(worktree) = self.active.clone() else {
            return;
        };
        let message = self.commit_input.read(cx).value().to_string();
        if message.trim().is_empty() && !amend {
            return;
        }
        self.git.send(Cmd::Commit {
            worktree,
            message,
            amend,
            all: false,
        });
        cx.notify();
    }
}

impl PerchApp {
    /// Demande confirmation avant de jeter des modifications.
    ///
    /// Seule action de Perch qui détruit du travail sans que git en garde une
    /// copie : ni `reflog` ni `stash` ne rattrapent un `restore --worktree`.
    /// D'où le dialogue, même si tout le reste de l'interface agit au clic.
    fn confirm_discard(
        &mut self,
        worktree: PathBuf,
        path: PathBuf,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let label = path.display().to_string();
        let entity = cx.entity();
        window.open_dialog(cx, move |dialog, _window, _cx| {
            let (worktree, path, entity) = (worktree.clone(), path.clone(), entity.clone());
            dialog
                .title(tr!("discard-title"))
                .child(
                    v_flex()
                        .gap_1()
                        .child(div().text_sm().child(label.clone()))
                        .child(div().text_xs().child(tr!("discard-warning"))),
                )
                .confirm()
                .on_ok(move |_, _window, cx| {
                    entity.update(cx, |this, cx| {
                        this.git.send(Cmd::Discard {
                            worktree: worktree.clone(),
                            paths: vec![path.clone()],
                        });
                        cx.notify();
                    });
                    true
                })
        });
    }

    pub(super) fn apply_hunk(&mut self, patch: String, cx: &mut Context<Self>) {
        let Some(worktree) = self.active.clone() else {
            return;
        };
        self.git.send(Cmd::ApplyHunk {
            worktree,
            patch,
            reverse: false,
        });
        cx.notify();
    }
}

/// Rend une ligne de la liste des fichiers.
///
/// Fonction libre parce que la fermeture d'une liste virtualisée ne reçoit pas
/// la vue : elle capture l'entité et repasse par `update` pour agir, comme le
/// font les gestionnaires de dialogue.
#[allow(clippy::too_many_arguments)]
fn render_file_row(
    rows: &std::rc::Rc<Vec<Row>>,
    index: usize,
    worktree: &Path,
    selected: Option<&Path>,
    colors: &DiffColors,
    entity: &gpui::Entity<PerchApp>,
    cx: &mut gpui::App,
) -> gpui::AnyElement {
    let Some(row) = rows.get(index) else {
        return div().into_any_element();
    };
    let is_selected = selected == Some(row.path.as_path());
    let staged = row.staged;

    h_flex()
        .id(("file", index))
        .h(px(28.))
        .px_2()
        .gap_2()
        .items_center()
        .cursor_pointer()
        .when(is_selected, |el| el.bg(cx.theme().accent))
        .hover(|s| s.bg(cx.theme().accent.opacity(0.5)))
        .on_click({
            let (entity, worktree, path) =
                (entity.clone(), worktree.to_path_buf(), row.path.clone());
            move |_, _window, cx| {
                entity.update(cx, |this, cx| {
                    this.open_file(worktree.clone(), path.clone(), cx)
                });
            }
        })
        .child(
            div()
                .w(px(12.))
                .flex_none()
                .text_xs()
                .font_family("JetBrains Mono")
                .text_color(status_color(row.code, cx))
                .child(row.code.letter()),
        )
        .child(
            h_flex()
                .flex_1()
                .min_w_0()
                .gap_1()
                .items_baseline()
                .child(div().truncate().text_sm().child(row.name.clone()))
                .child(
                    div()
                        .truncate()
                        .text_xs()
                        .text_color(cx.theme().muted_foreground)
                        .child(row.directory.clone()),
                ),
        )
        .when(row.added > 0, |el| {
            el.child(
                div()
                    .flex_none()
                    .text_xs()
                    .text_color(colors.added_fg)
                    .child(format!("+{}", row.added)),
            )
        })
        .when(row.removed > 0, |el| {
            el.child(
                div()
                    .flex_none()
                    .text_xs()
                    .text_color(colors.removed_fg)
                    .child(format!("−{}", row.removed)),
            )
        })
        .when(!staged, |el| {
            let (entity, worktree, path) =
                (entity.clone(), worktree.to_path_buf(), row.path.clone());
            el.child(
                Button::new(("discard", index))
                    .ghost()
                    .xsmall()
                    .icon(icon("undo-2"))
                    .tooltip(tr!("action-discard"))
                    .on_click(move |_, window, cx| {
                        entity.update(cx, |this, cx| {
                            this.confirm_discard(worktree.clone(), path.clone(), window, cx)
                        });
                    }),
            )
        })
        .child({
            let (entity, worktree, path) =
                (entity.clone(), worktree.to_path_buf(), row.path.clone());
            Button::new(("toggle-stage", index))
                .ghost()
                .xsmall()
                .icon(icon(if staged { "arrow-down-to-line" } else { "plus" }))
                .tooltip(if staged {
                    tr!("action-unstage")
                } else {
                    tr!("action-stage")
                })
                .on_click(move |_, _window, cx| {
                    entity.update(cx, |this, cx| {
                        let paths = vec![path.clone()];
                        let worktree = worktree.clone();
                        this.git.send(if staged {
                            Cmd::Unstage { worktree, paths }
                        } else {
                            Cmd::Stage { worktree, paths }
                        });
                        cx.notify();
                    });
                })
        })
        .into_any_element()
}

/// Les entrées de la liste pour un domaine de revue donné.
///
/// Fonction libre parce que c'est la seule vraie décision de la vue de revue —
/// quel fichier apparaît de quel côté — et qu'elle se teste sans fenêtre.
///
/// Le statut est la source pour les deux premiers domaines : lui seul
/// distingue l'index du répertoire de travail, et un fichier peut être des
/// deux côtés à la fois. Les deux autres portent sur des commits, qui n'ont
/// pas de notion d'index, et viennent donc de `--numstat`.
fn rows_for(range: &DiffRange, status: &Status, files: &[DiffFile]) -> Vec<Row> {
    let volumes: std::collections::HashMap<&PathBuf, (usize, usize)> = files
        .iter()
        .map(|f| (&f.path, (f.added, f.removed)))
        .collect();
    let volume = |path: &PathBuf| volumes.get(path).copied().unwrap_or((0, 0));

    match range {
        DiffRange::Unstaged => status
            .unstaged()
            .map(|f| {
                let (added, removed) = volume(&f.path);
                Row {
                    path: f.path.clone(),
                    name: f.file_name(),
                    directory: f.directory(),
                    code: f.worktree,
                    added,
                    removed,
                    staged: false,
                }
            })
            .collect(),
        DiffRange::Staged => status
            .staged()
            .map(|f| {
                let (added, removed) = volume(&f.path);
                Row {
                    path: f.path.clone(),
                    name: f.file_name(),
                    directory: f.directory(),
                    code: f.index,
                    added,
                    removed,
                    staged: true,
                }
            })
            .collect(),
        DiffRange::Head | DiffRange::Branch { .. } | DiffRange::Commit { .. } => files
            .iter()
            .map(|f| Row {
                path: f.path.clone(),
                name: f
                    .path
                    .file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_default(),
                directory: f
                    .path
                    .parent()
                    .filter(|p| !p.as_os_str().is_empty())
                    .map(|p| p.display().to_string())
                    .unwrap_or_default(),
                code: if f.removed == 0 {
                    StatusCode::Added
                } else if f.added == 0 {
                    StatusCode::Deleted
                } else {
                    StatusCode::Modified
                },
                added: f.added,
                removed: f.removed,
                // Un commit est déjà écrit : rien à indexer.
                staged: true,
            })
            .collect(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::git::FileStatus;

    fn file(path: &str, index: StatusCode, worktree: StatusCode) -> FileStatus {
        FileStatus {
            path: PathBuf::from(path),
            original: None,
            index,
            worktree,
        }
    }

    fn status(files: Vec<FileStatus>) -> Status {
        Status {
            files,
            ..Status::default()
        }
    }

    #[test]
    fn a_fully_staged_repository_fills_the_index_tab_and_leaves_the_other_empty() {
        // Le cas d'un dépôt où tout a été indexé avant d'ouvrir Perch : la vue
        // par défaut est légitimement vide, et tout doit se trouver dans
        // l'index — pas l'inverse.
        let status = status(vec![
            file("a.php", StatusCode::Modified, StatusCode::Unmodified),
            file("b.php", StatusCode::Added, StatusCode::Unmodified),
        ]);

        assert!(rows_for(&DiffRange::Unstaged, &status, &[]).is_empty());

        let staged = rows_for(&DiffRange::Staged, &status, &[]);
        assert_eq!(staged.len(), 2);
        assert_eq!(staged[0].code, StatusCode::Modified);
        assert_eq!(staged[1].code, StatusCode::Added);
        assert!(staged.iter().all(|r| r.staged));
    }

    #[test]
    fn a_file_staged_then_modified_appears_on_both_sides() {
        let status = status(vec![file(
            "src/x.rs",
            StatusCode::Modified,
            StatusCode::Modified,
        )]);
        assert_eq!(rows_for(&DiffRange::Unstaged, &status, &[]).len(), 1);
        assert_eq!(rows_for(&DiffRange::Staged, &status, &[]).len(), 1);
    }

    #[test]
    fn an_untracked_file_is_not_in_the_index() {
        let status = status(vec![file(
            "nouveau.txt",
            StatusCode::Untracked,
            StatusCode::Untracked,
        )]);
        assert_eq!(rows_for(&DiffRange::Unstaged, &status, &[]).len(), 1);
        assert!(
            rows_for(&DiffRange::Staged, &status, &[]).is_empty(),
            "un fichier jamais ajouté n'a rien dans l'index"
        );
    }

    #[test]
    fn volumes_come_from_numstat_and_default_to_zero() {
        let status = status(vec![file(
            "a.rs",
            StatusCode::Modified,
            StatusCode::Unmodified,
        )]);
        let files = vec![DiffFile {
            path: PathBuf::from("a.rs"),
            original: None,
            added: 12,
            removed: 3,
            binary: false,
        }];
        let rows = rows_for(&DiffRange::Staged, &status, &files);
        assert_eq!((rows[0].added, rows[0].removed), (12, 3));

        // Sans `--numstat` encore arrivé, la ligne s'affiche quand même.
        let rows = rows_for(&DiffRange::Staged, &status, &[]);
        assert_eq!((rows[0].added, rows[0].removed), (0, 0));
    }

    #[test]
    fn commit_ranges_come_from_the_file_list_alone() {
        // Aucun statut : une revue de branche ne parle que de commits.
        let files = vec![DiffFile {
            path: PathBuf::from("dossier/ajoute.rs"),
            original: None,
            added: 5,
            removed: 0,
            binary: false,
        }];
        let rows = rows_for(&DiffRange::Head, &Status::default(), &files);
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].name, "ajoute.rs");
        assert_eq!(rows[0].directory, "dossier");
        assert_eq!(rows[0].code, StatusCode::Added);
    }
}
