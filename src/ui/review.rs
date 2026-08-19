//! La revue : liste des fichiers touchés, et le diff du fichier choisi.
//!
//! Quatre domaines de comparaison, choisis par les onglets en tête de liste :
//! les modifications non indexées, l'index, tout le checkout contre HEAD, et
//! la branche entière depuis sa divergence d'avec sa base. Le dernier est
//! celui qui sert à relire le travail d'un agent avant de le pousser.

use std::path::PathBuf;

use gpui::{div, prelude::*, px, Context, SharedString, Window};
use gpui_component::{
    button::{Button, ButtonVariants},
    h_flex,
    input::Input,
    v_flex, ActiveTheme, Disableable, Selectable, Sizable, WindowExt,
};

use crate::git::{DiffLineKind, DiffRange, StatusCode};
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
                    .id("file-list")
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .when(rows.is_empty(), |el| {
                        el.child(
                            div()
                                .p_3()
                                .text_xs()
                                .text_color(cx.theme().muted_foreground)
                                .child(tr!("review-clean")),
                        )
                    })
                    .children(rows.into_iter().enumerate().map(|(ix, row)| {
                        let is_selected = selected.as_deref() == Some(row.path.as_path());
                        let for_click = row.path.clone();
                        let for_toggle = row.path.clone();
                        let worktree_click = worktree.clone();
                        let worktree_toggle = worktree.clone();
                        let worktree_discard = worktree.clone();
                        let staged = row.staged;
                        h_flex()
                            .id(("file", ix))
                            .h(px(28.))
                            .px_2()
                            .gap_2()
                            .items_center()
                            .cursor_pointer()
                            .when(is_selected, |el| el.bg(cx.theme().accent))
                            .hover(|s| s.bg(cx.theme().accent.opacity(0.5)))
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.open_file(worktree_click.clone(), for_click.clone(), cx);
                            }))
                            .child(
                                div()
                                    .w(px(12.))
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
                                    .child(div().truncate().text_sm().child(row.name))
                                    .child(
                                        div()
                                            .truncate()
                                            .text_xs()
                                            .text_color(cx.theme().muted_foreground)
                                            .child(row.directory),
                                    ),
                            )
                            .when(row.added > 0, |el| {
                                el.child(
                                    div()
                                        .text_xs()
                                        .text_color(DiffColors::of(cx).added_fg)
                                        .child(format!("+{}", row.added)),
                                )
                            })
                            .when(row.removed > 0, |el| {
                                el.child(
                                    div()
                                        .text_xs()
                                        .text_color(DiffColors::of(cx).removed_fg)
                                        .child(format!("−{}", row.removed)),
                                )
                            })
                            .when(!staged, |el| {
                                let for_discard = row.path.clone();
                                let worktree = worktree_discard.clone();
                                el.child(
                                    Button::new(("discard", ix))
                                        .ghost()
                                        .xsmall()
                                        .icon(icon("undo-2"))
                                        .tooltip(tr!("action-discard"))
                                        .on_click(cx.listener(move |this, _, window, cx| {
                                            this.confirm_discard(
                                                worktree.clone(),
                                                for_discard.clone(),
                                                window,
                                                cx,
                                            );
                                        })),
                                )
                            })
                            .child(
                                Button::new(("toggle-stage", ix))
                                    .ghost()
                                    .xsmall()
                                    .icon(icon(if staged { "arrow-down-to-line" } else { "plus" }))
                                    .tooltip(if staged {
                                        tr!("action-unstage")
                                    } else {
                                        tr!("action-stage")
                                    })
                                    .on_click(cx.listener(move |this, _, _, cx| {
                                        let paths = vec![for_toggle.clone()];
                                        let worktree = worktree_toggle.clone();
                                        this.git.send(if staged {
                                            Cmd::Unstage { worktree, paths }
                                        } else {
                                            Cmd::Stage { worktree, paths }
                                        });
                                        cx.notify();
                                    })),
                            )
                    })),
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
        let Some(state) = self.active_review() else {
            return Vec::new();
        };
        let volumes: std::collections::HashMap<&PathBuf, (usize, usize)> = state
            .files
            .iter()
            .map(|f| (&f.path, (f.added, f.removed)))
            .collect();

        match state.range {
            DiffRange::Unstaged => state
                .status
                .unstaged()
                .map(|f| Row {
                    path: f.path.clone(),
                    name: f.file_name(),
                    directory: f.directory(),
                    code: f.worktree,
                    added: volumes.get(&f.path).map(|v| v.0).unwrap_or(0),
                    removed: volumes.get(&f.path).map(|v| v.1).unwrap_or(0),
                    staged: false,
                })
                .collect(),
            DiffRange::Staged => state
                .status
                .staged()
                .map(|f| Row {
                    path: f.path.clone(),
                    name: f.file_name(),
                    directory: f.directory(),
                    code: f.index,
                    added: volumes.get(&f.path).map(|v| v.0).unwrap_or(0),
                    removed: volumes.get(&f.path).map(|v| v.1).unwrap_or(0),
                    staged: true,
                })
                .collect(),
            _ => state
                .files
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

    fn render_range_tabs(&self, range: &DiffRange, cx: &mut Context<Self>) -> impl IntoElement {
        let base = self
            .active_review()
            .and_then(|r| r.base.clone())
            .unwrap_or_else(|| "main".into());
        let tabs: [(DiffRange, SharedString); 4] = [
            (DiffRange::Unstaged, tr!("range-unstaged")),
            (DiffRange::Staged, tr!("range-staged")),
            (DiffRange::Head, tr!("range-head")),
            (
                DiffRange::Branch { base: base.clone() },
                tr!("range-branch", { base: base }),
            ),
        ];
        h_flex()
            .h(px(30.))
            .w_full()
            .px_1()
            .gap_1()
            .items_center()
            .border_b_1()
            .border_color(cx.theme().border)
            .children(tabs.into_iter().enumerate().map(|(ix, (target, label))| {
                let selected = *range == target;
                Button::new(("range", ix))
                    .ghost()
                    .xsmall()
                    .label(label)
                    .selected(selected)
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.set_range(target.clone(), cx);
                    }))
            }))
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

    pub(super) fn render_diff(
        &mut self,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let colors = DiffColors::of(cx);
        let Some(state) = self.active_review() else {
            return div().into_any_element();
        };
        let Some(path) = state.selected.clone() else {
            return v_flex()
                .size_full()
                .items_center()
                .justify_center()
                .child(
                    div()
                        .text_sm()
                        .text_color(cx.theme().muted_foreground)
                        .child(tr!("review-pick-a-file")),
                )
                .into_any_element();
        };

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
                    .font_family("JetBrains Mono")
                    .child(path.display().to_string()),
            );

        let Some(diff) = state.diff.as_ref() else {
            return v_flex()
                .size_full()
                .child(header)
                .child(
                    div()
                        .p_3()
                        .text_xs()
                        .text_color(cx.theme().muted_foreground)
                        .child(tr!("review-loading")),
                )
                .into_any_element();
        };

        if diff.binary {
            return v_flex()
                .size_full()
                .child(header)
                .child(
                    div()
                        .p_3()
                        .text_sm()
                        .text_color(cx.theme().muted_foreground)
                        .child(tr!("review-binary")),
                )
                .into_any_element();
        }

        // Largeur de la gouttière : deux numéros de ligne en chasse fixe. Elle
        // est calculée sur le plus grand numéro du fichier, sinon un fichier de
        // mille lignes décale sa gouttière à mi-parcours.
        let stageable_hunks = state.range == DiffRange::Unstaged;
        // Les patchs sont construits ici, hors des fermetures de rendu : elles
        // ne peuvent pas emprunter `state` et le diff en même temps.
        let patches: Vec<String> = diff
            .hunks
            .iter()
            .map(|hunk| crate::git::diff::hunk_patch(&path, None, hunk, false))
            .collect();

        let width = diff
            .hunks
            .iter()
            .flat_map(|h| h.lines.iter())
            .filter_map(|l| l.new_no.or(l.old_no))
            .max()
            .unwrap_or(1)
            .to_string()
            .len();
        let gutter = px(width as f32 * 8.0 + 8.0);

        v_flex()
            .size_full()
            .child(header)
            .child(
                div()
                    .id("diff-scroll")
                    .flex_1()
                    .min_h_0()
                    .overflow_scroll()
                    .font_family("JetBrains Mono")
                    .text_size(px(12.))
                    .children(diff.hunks.iter().enumerate().map(|(hix, hunk)| {
                        v_flex()
                            .w_full()
                            .child(
                                h_flex()
                                    .w_full()
                                    .px_2()
                                    .py_0p5()
                                    .items_center()
                                    .gap_2()
                                    .bg(colors.hunk_bg)
                                    .child(
                                        div()
                                            .flex_1()
                                            .truncate()
                                            .text_color(cx.theme().muted_foreground)
                                            .child(hunk.header.clone()),
                                    )
                                    // Indexer un hunk seul n'a de sens que
                                    // depuis les modifications non indexées :
                                    // ailleurs, ou bien tout est déjà dans
                                    // l'index, ou bien on regarde des commits.
                                    .when(stageable_hunks, |el| {
                                        let patch = patches[hix].clone();
                                        el.child(
                                            Button::new(("stage-hunk", hix))
                                                .ghost()
                                                .xsmall()
                                                .icon(icon("plus"))
                                                .tooltip(tr!("action-stage-hunk"))
                                                .on_click(cx.listener(move |this, _, _, cx| {
                                                    this.apply_hunk(patch.clone(), cx);
                                                })),
                                        )
                                    }),
                            )
                            .children(hunk.lines.iter().enumerate().map(|(lix, line)| {
                                let (bg, fg, sign) = match line.kind {
                                    DiffLineKind::Added => {
                                        (Some(colors.added_bg), Some(colors.added_fg), "+")
                                    }
                                    DiffLineKind::Removed => {
                                        (Some(colors.removed_bg), Some(colors.removed_fg), "−")
                                    }
                                    DiffLineKind::Context => (None, None, " "),
                                    DiffLineKind::NoNewline => (None, None, " "),
                                };
                                h_flex()
                                    .id(("line", hix * 10_000 + lix))
                                    .w_full()
                                    .when_some(bg, |el, bg| el.bg(bg))
                                    .child(
                                        div()
                                            .w(gutter)
                                            .flex_none()
                                            .text_right()
                                            .pr_1()
                                            .text_color(colors.line_number)
                                            .child(
                                                line.old_no
                                                    .map(|n| n.to_string())
                                                    .unwrap_or_default(),
                                            ),
                                    )
                                    .child(
                                        div()
                                            .w(gutter)
                                            .flex_none()
                                            .text_right()
                                            .pr_1()
                                            .text_color(colors.line_number)
                                            .child(
                                                line.new_no
                                                    .map(|n| n.to_string())
                                                    .unwrap_or_default(),
                                            ),
                                    )
                                    .child(div().w(px(12.)).flex_none().child(sign))
                                    .child(
                                        div()
                                            .flex_1()
                                            .when_some(fg, |el, fg| el.text_color(fg))
                                            // Les espaces significatifs d'un
                                            // diff ne doivent pas être avalés
                                            // par le rendu.
                                            .child(line.text.replace('\t', "    ")),
                                    )
                            }))
                    })),
            )
            .into_any_element()
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

    fn apply_hunk(&mut self, patch: String, cx: &mut Context<Self>) {
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
