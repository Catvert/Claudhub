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
    checkbox::Checkbox,
    h_flex,
    input::Input,
    select::Select,
    v_flex, ActiveTheme, Disableable, Selectable, Sizable, WindowExt,
};

use crate::git::{DiffFile, DiffRange, Status, StatusCode};
use crate::runtime::Cmd;
use crate::tr;
use crate::ui::app::PerchApp;
use crate::ui::icons::icon;
use crate::ui::theme::{status_color, DiffColors};

/// Une entrée de la liste des modifications.
///
/// Les fichiers sont groupés comme dans les clients qui masquent l'index :
/// ce qui est suivi d'un côté, ce qui ne l'est pas encore de l'autre. Le
/// groupe porte sa propre case, qui indexe ou dés-indexe tout d'un coup.
#[derive(Clone)]
enum Row {
    Group(Group),
    File(FileRow),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Group {
    /// Fichiers que git suit déjà.
    Tracked,
    /// Fichiers jamais ajoutés. Les cocher, c'est les faire suivre.
    Untracked,
}

#[derive(Clone)]
struct FileRow {
    path: PathBuf,
    name: String,
    directory: String,
    /// Les deux codes de git, celui de l'index puis celui du répertoire de
    /// travail : c'est l'information exacte, et elle tient en deux caractères
    /// là où une seule case à cocher devrait mentir sur les fichiers
    /// partiellement indexés.
    index: StatusCode,
    worktree: StatusCode,
    added: usize,
    removed: usize,
    /// Ce fichier ira dans le prochain commit, au moins en partie.
    staged: bool,
    untracked: bool,
}

impl FileRow {
    /// Une partie seulement du fichier est indexée : ce que git écrit `MM`.
    fn partial(&self) -> bool {
        self.staged && !matches!(self.worktree, StatusCode::Unmodified)
    }

    fn codes(&self) -> String {
        let index = self.index.letter();
        let worktree = self.worktree.letter();
        if self.untracked {
            "?".into()
        } else if index.trim().is_empty() {
            worktree.to_string()
        } else if worktree.trim().is_empty() {
            index.to_string()
        } else {
            format!("{index}{worktree}")
        }
    }
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
        let staged_count = rows
            .iter()
            .filter(|row| matches!(row, Row::File(file) if file.staged))
            .count();
        let can_commit = staged_count > 0 && matches!(range, DiffRange::Working);

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
                        // Seules les modifications en cours se cochent : sur un
                        // commit déjà écrit, il n'y a rien à indexer.
                        let checkable = matches!(range, DiffRange::Working);
                        el.child(
                            uniform_list("file-list", count, move |visible, _window, cx| {
                                visible
                                    .map(|ix| {
                                        render_row(
                                            &rows,
                                            ix,
                                            &worktree,
                                            selected.as_deref(),
                                            &colors,
                                            checkable,
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
        // l'autre.
        let base = self.active_review().and_then(|r| r.base.clone());
        let branch_range = base
            .clone()
            .map(|base| DiffRange::Branch { base })
            .unwrap_or(DiffRange::Working);
        // L'onglet ne nomme plus la base : le sélecteur juste à côté la porte,
        // et la répéter donnait deux fois la même information sur une ligne
        // qui n'en a pas la place.
        let branch_label = match &base {
            Some(_) => tr!("range-branch"),
            None => tr!("range-branch-none"),
        };
        // Le compte se lit sans cliquer : c'est ce qui évite d'ouvrir Perch
        // sur une liste vide sans comprendre où sont passées les
        // modifications.
        let changed = self
            .active_review()
            .map(|r| {
                r.status
                    .files
                    .iter()
                    .filter(|f| !matches!(f.index, StatusCode::Ignored))
                    .count()
            })
            .unwrap_or(0);
        let working_label = if changed == 0 {
            tr!("range-working")
        } else {
            SharedString::from(format!("{} {changed}", tr!("range-working")))
        };

        // Un commit choisi dans l'historique n'a pas d'onglet à lui : il
        // occupe la liste jusqu'à ce qu'on revienne à l'un des deux domaines.
        let showing_commit = matches!(range, DiffRange::Commit { .. });
        let tabs: [(DiffRange, SharedString, bool); 2] = [
            (DiffRange::Working, working_label, true),
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
                        let selected = enabled && !showing_commit && *range == target;
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
            .child(div().flex_1())
            // Le choix de la base vit à côté de son onglet : la branche
            // d'intégration devinée est un point de départ, pas une fatalité —
            // on compare aussi bien à `dev`, à une autre branche de travail ou
            // à une distante.
            .child(
                Select::new(&self.base_select)
                    .xsmall()
                    .title_prefix(tr!("range-base-prefix"))
                    .placeholder(tr!("range-base-placeholder"))
                    .menu_width(px(280.)),
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

    /// Coche ou décoche des fichiers, c'est-à-dire les indexe ou les retire de
    /// l'index. C'est le seul geste d'indexation que l'interface propose : la
    /// case remplace les deux listes que git distingue.
    pub(super) fn set_staged(
        &mut self,
        worktree: PathBuf,
        paths: Vec<PathBuf>,
        staged: bool,
        cx: &mut Context<Self>,
    ) {
        if paths.is_empty() {
            return;
        }
        self.git.send(if staged {
            Cmd::Stage { worktree, paths }
        } else {
            Cmd::Unstage { worktree, paths }
        });
        cx.notify();
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

/// Rend une ligne de la liste : un en-tête de groupe ou un fichier.
///
/// Fonction libre parce que la fermeture d'une liste virtualisée ne reçoit pas
/// la vue : elle capture l'entité et repasse par `update` pour agir, comme le
/// font les gestionnaires de dialogue.
#[allow(clippy::too_many_arguments)]
fn render_row(
    rows: &std::rc::Rc<Vec<Row>>,
    index: usize,
    worktree: &Path,
    selected: Option<&Path>,
    colors: &DiffColors,
    checkable: bool,
    entity: &gpui::Entity<PerchApp>,
    cx: &mut gpui::App,
) -> gpui::AnyElement {
    match rows.get(index) {
        Some(Row::Group(group)) => render_group(rows, index, *group, worktree, entity, cx),
        Some(Row::File(file)) => render_file(
            file, index, worktree, selected, colors, checkable, entity, cx,
        ),
        None => div().into_any_element(),
    }
}

fn render_group(
    rows: &std::rc::Rc<Vec<Row>>,
    index: usize,
    group: Group,
    worktree: &Path,
    entity: &gpui::Entity<PerchApp>,
    cx: &mut gpui::App,
) -> gpui::AnyElement {
    let checked = group_checked(rows, group);
    let paths = group_paths(rows, group);
    let count = paths.len();
    let label = match group {
        Group::Tracked => tr!("group-tracked"),
        Group::Untracked => tr!("group-untracked"),
    };
    let (entity, worktree) = (entity.clone(), worktree.to_path_buf());

    h_flex()
        .h(px(26.))
        .w_full()
        .px_2()
        .gap_2()
        .items_center()
        .bg(cx.theme().secondary)
        .child(
            Checkbox::new(("group", index))
                .checked(checked)
                .on_click(move |_, _window, cx| {
                    entity.update(cx, |this, cx| {
                        this.set_staged(worktree.clone(), paths.clone(), !checked, cx)
                    });
                }),
        )
        .child(
            div()
                .flex_1()
                .text_xs()
                .font_weight(gpui::FontWeight::SEMIBOLD)
                .text_color(cx.theme().muted_foreground)
                .child(format!("{label} ({count})")),
        )
        .into_any_element()
}

#[allow(clippy::too_many_arguments)]
fn render_file(
    row: &FileRow,
    index: usize,
    worktree: &Path,
    selected: Option<&Path>,
    colors: &DiffColors,
    checkable: bool,
    entity: &gpui::Entity<PerchApp>,
    cx: &mut gpui::App,
) -> gpui::AnyElement {
    let is_selected = selected == Some(row.path.as_path());
    let staged = row.staged;

    h_flex()
        .id(("file", index))
        .h(px(28.))
        .px_2()
        .gap_2()
        .items_center()
        .cursor_pointer()
        .whitespace_nowrap()
        .overflow_hidden()
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
        // Cocher, c'est indexer. Les domaines qui portent sur des commits déjà
        // écrits n'ont rien à cocher : la case y serait un bouton qui ment.
        .when(checkable, |el| {
            let (entity, worktree, path) =
                (entity.clone(), worktree.to_path_buf(), row.path.clone());
            el.child(Checkbox::new(("stage", index)).checked(staged).on_click(
                move |_, _window, cx| {
                    entity.update(cx, |this, cx| {
                        this.set_staged(worktree.clone(), vec![path.clone()], !staged, cx)
                    });
                },
            ))
        })
        .child(
            div()
                .w(px(20.))
                .flex_none()
                .text_xs()
                .font_family("JetBrains Mono")
                .text_color(status_color(
                    if row.untracked {
                        StatusCode::Untracked
                    } else if staged {
                        row.index
                    } else {
                        row.worktree
                    },
                    cx,
                ))
                .child(row.codes()),
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
        // Un fichier dont une partie seulement est indexée : la case cochée ne
        // suffit pas à le dire, et c'est précisément le cas où l'on croit
        // valider tout un fichier alors qu'on n'en valide que la moitié.
        .when(row.partial(), |el| {
            el.child(
                div()
                    .flex_none()
                    .px_1()
                    .rounded(cx.theme().radius)
                    .bg(cx.theme().warning.opacity(0.18))
                    .text_xs()
                    .text_color(cx.theme().warning)
                    .child(tr!("file-partially-staged")),
            )
        })
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
        .when(checkable && !row.untracked, |el| {
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
        .into_any_element()
}

/// Les entrées de la liste pour un domaine de revue donné.
///
/// Fonction libre parce que c'est la seule vraie décision de cette vue — quel
/// fichier apparaît, dans quel groupe, coché ou non — et qu'elle se teste sans
/// fenêtre.
///
/// Le statut est la source pour les modifications en cours : lui seul
/// distingue ce qui est indexé de ce qui ne l'est pas, distinction que la case
/// à cocher restitue. Les autres domaines portent sur des commits, qui n'ont
/// pas de notion d'index, et viennent de `--numstat`.
fn rows_for(range: &DiffRange, status: &Status, files: &[DiffFile]) -> Vec<Row> {
    let volumes: std::collections::HashMap<&PathBuf, (usize, usize)> = files
        .iter()
        .map(|f| (&f.path, (f.added, f.removed)))
        .collect();
    let volume = |path: &PathBuf| volumes.get(path).copied().unwrap_or((0, 0));

    match range {
        DiffRange::Working => {
            let mut tracked = Vec::new();
            let mut untracked = Vec::new();
            for file in &status.files {
                if matches!(file.index, StatusCode::Ignored) {
                    continue;
                }
                let (added, removed) = volume(&file.path);
                let row = FileRow {
                    path: file.path.clone(),
                    name: file.file_name(),
                    directory: file.directory(),
                    index: file.index,
                    worktree: file.worktree,
                    added,
                    removed,
                    staged: file.is_staged(),
                    untracked: file.is_untracked(),
                };
                if row.untracked {
                    untracked.push(row);
                } else {
                    tracked.push(row);
                }
            }

            let mut rows = Vec::new();
            if !tracked.is_empty() {
                rows.push(Row::Group(Group::Tracked));
                rows.extend(tracked.into_iter().map(Row::File));
            }
            if !untracked.is_empty() {
                rows.push(Row::Group(Group::Untracked));
                rows.extend(untracked.into_iter().map(Row::File));
            }
            rows
        }
        DiffRange::Branch { .. } | DiffRange::Commit { .. } => files
            .iter()
            .map(|f| {
                Row::File(FileRow {
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
                    index: if f.removed == 0 {
                        StatusCode::Added
                    } else if f.added == 0 {
                        StatusCode::Deleted
                    } else {
                        StatusCode::Modified
                    },
                    worktree: StatusCode::Unmodified,
                    added: f.added,
                    removed: f.removed,
                    // Un commit est déjà écrit : rien à cocher.
                    staged: true,
                    untracked: false,
                })
            })
            .collect(),
    }
}

/// Les fichiers d'un groupe, pour les cases qui agissent sur tout le lot.
fn group_paths(rows: &[Row], group: Group) -> Vec<PathBuf> {
    let mut inside = false;
    let mut paths = Vec::new();
    for row in rows {
        match row {
            Row::Group(g) => inside = *g == group,
            Row::File(file) if inside => paths.push(file.path.clone()),
            Row::File(_) => {}
        }
    }
    paths
}

/// Vrai si tout le groupe est déjà indexé.
fn group_checked(rows: &[Row], group: Group) -> bool {
    let mut inside = false;
    let mut any = false;
    for row in rows {
        match row {
            Row::Group(g) => inside = *g == group,
            Row::File(file) if inside => {
                any = true;
                if !file.staged {
                    return false;
                }
            }
            Row::File(_) => {}
        }
    }
    any
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

    fn files_of(rows: &[Row]) -> Vec<&FileRow> {
        rows.iter()
            .filter_map(|row| match row {
                Row::File(file) => Some(file),
                Row::Group(_) => None,
            })
            .collect()
    }

    fn groups_of(rows: &[Row]) -> Vec<Group> {
        rows.iter()
            .filter_map(|row| match row {
                Row::Group(group) => Some(*group),
                Row::File(_) => None,
            })
            .collect()
    }

    #[test]
    fn staged_and_unstaged_files_share_one_list() {
        // Le point de la fusion : plus deux domaines à recoudre mentalement,
        // une seule liste où la case dit ce qui partira au prochain commit.
        let status = status(vec![
            file("indexe.rs", StatusCode::Modified, StatusCode::Unmodified),
            file("modifie.rs", StatusCode::Unmodified, StatusCode::Modified),
            file("nouveau.rs", StatusCode::Untracked, StatusCode::Untracked),
        ]);
        let rows = rows_for(&DiffRange::Working, &status, &[]);
        let files = files_of(&rows);

        assert_eq!(files.len(), 3);
        assert!(files.iter().find(|f| f.name == "indexe.rs").unwrap().staged);
        assert!(
            !files
                .iter()
                .find(|f| f.name == "modifie.rs")
                .unwrap()
                .staged
        );
        assert!(
            !files
                .iter()
                .find(|f| f.name == "nouveau.rs")
                .unwrap()
                .staged
        );

        // Les fichiers jamais ajoutés forment leur propre groupe : les cocher
        // ne veut pas dire la même chose que pour un fichier déjà suivi.
        assert_eq!(groups_of(&rows), vec![Group::Tracked, Group::Untracked]);
    }

    #[test]
    fn a_partially_staged_file_says_so() {
        // `MM` : une case cochée laisserait croire que tout le fichier part.
        let status = status(vec![file(
            "moitie.rs",
            StatusCode::Modified,
            StatusCode::Modified,
        )]);
        let rows = rows_for(&DiffRange::Working, &status, &[]);
        let file = files_of(&rows)[0];
        assert!(file.staged);
        assert!(file.partial(), "l'indexation partielle doit être signalée");
        assert_eq!(file.codes(), "MM");
    }

    #[test]
    fn the_codes_show_what_git_says() {
        let status = status(vec![
            file("ajoute.rs", StatusCode::Added, StatusCode::Unmodified),
            file("efface.rs", StatusCode::Unmodified, StatusCode::Deleted),
            file("neuf.rs", StatusCode::Untracked, StatusCode::Untracked),
        ]);
        let rows = rows_for(&DiffRange::Working, &status, &[]);
        let files = files_of(&rows);
        assert_eq!(files[0].codes(), "A");
        assert_eq!(files[1].codes(), "D");
        assert_eq!(files[2].codes(), "?");
    }

    #[test]
    fn an_empty_group_is_not_shown() {
        let status = status(vec![file(
            "suivi.rs",
            StatusCode::Modified,
            StatusCode::Unmodified,
        )]);
        let rows = rows_for(&DiffRange::Working, &status, &[]);
        assert_eq!(groups_of(&rows), vec![Group::Tracked]);
    }

    #[test]
    fn a_group_is_checked_only_when_all_of_it_is() {
        let mixed = status(vec![
            file("un.rs", StatusCode::Modified, StatusCode::Unmodified),
            file("deux.rs", StatusCode::Unmodified, StatusCode::Modified),
        ]);
        let rows = rows_for(&DiffRange::Working, &mixed, &[]);
        assert!(!group_checked(&rows, Group::Tracked));
        assert_eq!(group_paths(&rows, Group::Tracked).len(), 2);

        let everything = status(vec![
            file("un.rs", StatusCode::Modified, StatusCode::Unmodified),
            file("deux.rs", StatusCode::Added, StatusCode::Unmodified),
        ]);
        let rows = rows_for(&DiffRange::Working, &everything, &[]);
        assert!(group_checked(&rows, Group::Tracked));
    }

    #[test]
    fn a_group_checkbox_only_covers_its_own_files() {
        let status = status(vec![
            file("suivi.rs", StatusCode::Modified, StatusCode::Unmodified),
            file("neuf.rs", StatusCode::Untracked, StatusCode::Untracked),
        ]);
        let rows = rows_for(&DiffRange::Working, &status, &[]);
        assert_eq!(
            group_paths(&rows, Group::Untracked),
            vec![PathBuf::from("neuf.rs")]
        );
        assert_eq!(
            group_paths(&rows, Group::Tracked),
            vec![PathBuf::from("suivi.rs")]
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
        let rows = rows_for(&DiffRange::Working, &status, &files);
        let row = files_of(&rows)[0];
        assert_eq!((row.added, row.removed), (12, 3));

        // Sans `--numstat` encore arrivé, la ligne s'affiche quand même.
        let rows = rows_for(&DiffRange::Working, &status, &[]);
        assert_eq!(
            (files_of(&rows)[0].added, files_of(&rows)[0].removed),
            (0, 0)
        );
    }

    #[test]
    fn commit_ranges_come_from_the_file_list_alone() {
        // Aucun statut : une revue de commits ne parle que de ce que git a
        // déjà écrit, et rien n'y est à cocher.
        let files = vec![DiffFile {
            path: PathBuf::from("dossier/ajoute.rs"),
            original: None,
            added: 5,
            removed: 0,
            binary: false,
        }];
        let rows = rows_for(
            &DiffRange::Commit {
                id: "abc".into(),
                parent: None,
            },
            &Status::default(),
            &files,
        );
        assert!(groups_of(&rows).is_empty(), "pas de groupes sur un commit");
        let row = files_of(&rows)[0];
        assert_eq!(row.name, "ajoute.rs");
        assert_eq!(row.directory, "dossier");
        assert_eq!(row.index, StatusCode::Added);
        assert!(!row.partial());
    }
}
