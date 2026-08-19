//! Barre latérale : les dépôts et leurs worktrees.
//!
//! C'est le sélecteur principal de l'application. Tout le reste de la fenêtre
//! — revue, terminaux, branches — parle du worktree choisi ici.

use std::path::PathBuf;

use gpui::{div, prelude::*, px, Context, Window};
use gpui_component::{
    button::{Button, ButtonVariants},
    h_flex, v_flex, ActiveTheme, Sizable, StyledExt, WindowExt,
};

use crate::runtime::Cmd;
use crate::tr;
use crate::ui::app::PerchApp;
use crate::ui::icons::icon;

/// Ce qu'une ligne de la barre latérale affiche d'un worktree.
///
/// Le compte de fichiers modifiés n'est connu que des worktrees déjà visités —
/// le statut n'est lu qu'à l'ouverture. Il vaut `None` ailleurs, et une pastille
/// absente se lit « on ne sait pas encore », ce qui est vrai, là où un zéro
/// affirmerait à tort qu'il n'y a rien à relire.
struct WorktreeRow {
    path: PathBuf,
    label: String,
    branch: Option<String>,
    is_main: bool,
    prunable: bool,
    dirty: Option<usize>,
}

impl PerchApp {
    pub(super) fn render_sidebar(
        &mut self,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let repos: Vec<_> = self
            .repos
            .iter()
            .enumerate()
            .map(|(ix, repo)| {
                (
                    ix,
                    repo.main.clone(),
                    repo.name.clone(),
                    repo.collapsed,
                    repo.worktrees
                        .iter()
                        .map(|w| WorktreeRow {
                            path: w.path.clone(),
                            label: w.label(),
                            branch: w.branch.clone(),
                            is_main: w.is_main,
                            prunable: w.prunable,
                            dirty: self
                                .review
                                .get(&w.path)
                                .map(|review| review.status.files.len()),
                        })
                        .collect::<Vec<_>>(),
                )
            })
            .collect();
        let active = self.active.clone();
        let empty = repos.is_empty();

        v_flex()
            .size_full()
            .min_w(px(160.))
            .bg(cx.theme().sidebar)
            .border_r_1()
            .border_color(cx.theme().border)
            .child(
                h_flex()
                    .py_1()
                    .px_2()
                    .gap_1()
                    .items_center()
                    .justify_between()
                    .child(
                        div()
                            .text_xs()
                            .font_semibold()
                            .text_color(cx.theme().muted_foreground)
                            .child(tr!("sidebar-repositories")),
                    )
                    .child(
                        Button::new("add-repo")
                            .ghost()
                            .xsmall()
                            .icon(icon("plus"))
                            .tooltip(tr!("sidebar-open-repository"))
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.prompt_open_repository(window, cx);
                            })),
                    ),
            )
            .child(
                div()
                    .id("sidebar-scroll")
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    // Au premier lancement, c'est tout ce qu'on voit : un
                    // texte gris ne dit pas quoi faire, un bouton si.
                    .when(empty, |el| {
                        el.child(
                            v_flex()
                                .p_4()
                                .gap_2()
                                .items_center()
                                .child(
                                    icon("folder")
                                        .large()
                                        .text_color(cx.theme().muted_foreground.opacity(0.4)),
                                )
                                .child(
                                    div()
                                        .text_xs()
                                        .text_center()
                                        .text_color(cx.theme().muted_foreground)
                                        .child(tr!("sidebar-empty")),
                                )
                                .child(
                                    Button::new("open-first-repo")
                                        .outline()
                                        .small()
                                        .label(tr!("sidebar-open-repository"))
                                        .on_click(cx.listener(|this, _, window, cx| {
                                            this.prompt_open_repository(window, cx);
                                        })),
                                ),
                        )
                    })
                    .children(
                        repos
                            .into_iter()
                            .map(|(ix, main, name, collapsed, worktrees)| {
                                let main_for_add = main.clone();
                                v_flex()
                                    .child(
                                        h_flex()
                                            .id(("repo", ix))
                                            .py_1()
                                            .px_2()
                                            .gap_1()
                                            .items_center()
                                            .cursor_pointer()
                                            .hover(|s| s.bg(cx.theme().sidebar_accent.opacity(0.6)))
                                            .on_click(cx.listener(move |this, _, _, cx| {
                                                if let Some(repo) = this.repos.get_mut(ix) {
                                                    repo.collapsed = !repo.collapsed;
                                                }
                                                cx.notify();
                                            }))
                                            .child(
                                                icon(if collapsed {
                                                    "chevron-right"
                                                } else {
                                                    "chevron-down"
                                                })
                                                .xsmall(),
                                            )
                                            .child(
                                                div()
                                                    .flex_1()
                                                    .truncate()
                                                    .text_sm()
                                                    .font_semibold()
                                                    .child(name),
                                            )
                                            .child(
                                                Button::new(("add-worktree", ix))
                                                    .ghost()
                                                    .xsmall()
                                                    .icon(icon("plus"))
                                                    .tooltip(tr!("sidebar-new-worktree"))
                                                    .on_click(cx.listener(
                                                        move |this, _, window, cx| {
                                                            this.prompt_new_worktree(
                                                                main_for_add.clone(),
                                                                window,
                                                                cx,
                                                            );
                                                        },
                                                    )),
                                            ),
                                    )
                                    .when(!collapsed, |el| {
                                        el.children(worktrees.into_iter().enumerate().map(
                                            |(wix, worktree)| {
                                                let WorktreeRow {
                                                    path,
                                                    label,
                                                    branch,
                                                    is_main,
                                                    prunable,
                                                    dirty,
                                                } = worktree;
                                                let selected =
                                                    active.as_deref() == Some(path.as_path());
                                                let for_click = path.clone();
                                                let for_remove = path.clone();
                                                let repo_main = main.clone();
                                                h_flex()
                                                    .id(("worktree", ix * 1000 + wix))
                                                    // Pas de hauteur fixe : la
                                                    // ligne porte deux lignes de
                                                    // texte, et une hauteur figée
                                                    // les faisait déborder sur la
                                                    // ligne suivante dès qu'on
                                                    // grossissait la police.
                                                    .py_1()
                                                    .pl_5()
                                                    .pr_1()
                                                    .gap_1()
                                                    .items_center()
                                                    .cursor_pointer()
                                                    .border_l_2()
                                                    .border_color(gpui::transparent_black())
                                                    .when(selected, |el| {
                                                        el.bg(cx.theme().sidebar_accent)
                                                            .border_color(cx.theme().primary)
                                                            .text_color(
                                                                cx.theme()
                                                                    .sidebar_accent_foreground,
                                                            )
                                                    })
                                                    .hover(|s| {
                                                        s.bg(cx.theme().sidebar_accent.opacity(0.5))
                                                    })
                                                    .on_click(cx.listener(
                                                        move |this, _, window, cx| {
                                                            this.select_worktree(
                                                                for_click.clone(),
                                                                window,
                                                                cx,
                                                            );
                                                        },
                                                    ))
                                                    .child(
                                                        icon(if is_main {
                                                            "folder"
                                                        } else {
                                                            "git-branch"
                                                        })
                                                        .xsmall(),
                                                    )
                                                    .child(
                                                        v_flex()
                                                            .flex_1()
                                                            .min_w_0()
                                                            .child(
                                                                div()
                                                                    .truncate()
                                                                    .text_sm()
                                                                    .when(selected, |el| {
                                                                        el.font_semibold()
                                                                    })
                                                                    .child(label),
                                                            )
                                                            .when_some(branch, |el, branch| {
                                                                el.child(
                                                                    div()
                                                                        .truncate()
                                                                        .text_xs()
                                                                        .text_color(
                                                                            cx.theme()
                                                                                .muted_foreground,
                                                                        )
                                                                        .child(branch),
                                                                )
                                                            }),
                                                    )
                                                    // Combien de fichiers ce
                                                    // worktree a en chantier : la
                                                    // question qu'on se pose en
                                                    // parcourant la liste.
                                                    .when_some(
                                                        dirty.filter(|n| *n > 0),
                                                        |el, count| {
                                                            el.child(
                                                                div()
                                                                    .flex_none()
                                                                    .px_1()
                                                                    .rounded(cx.theme().radius)
                                                                    .bg(cx
                                                                        .theme()
                                                                        .primary
                                                                        .opacity(0.18))
                                                                    .text_xs()
                                                                    .text_color(cx.theme().primary)
                                                                    .child(count.to_string()),
                                                            )
                                                        },
                                                    )
                                                    .when(prunable, |el| {
                                                        el.child(
                                                            icon("alert-circle")
                                                                .xsmall()
                                                                .text_color(cx.theme().warning),
                                                        )
                                                    })
                                                    // Le worktree principal ne se
                                                    // retire pas : git refuse, et
                                                    // proposer le bouton reviendrait
                                                    // à promettre une erreur.
                                                    .when(!is_main, |el| {
                                                        el.child(
                                                            Button::new((
                                                                "rm-worktree",
                                                                ix * 1000 + wix,
                                                            ))
                                                            .ghost()
                                                            .xsmall()
                                                            .icon(icon("trash-2"))
                                                            .tooltip(tr!("sidebar-remove-worktree"))
                                                            .on_click(cx.listener(
                                                                move |this, _, window, cx| {
                                                                    this.confirm_remove_worktree(
                                                                        repo_main.clone(),
                                                                        for_remove.clone(),
                                                                        window,
                                                                        cx,
                                                                    );
                                                                },
                                                            )),
                                                        )
                                                    })
                                            },
                                        ))
                                    })
                            }),
                    ),
            )
    }

    /// Ouvre un dépôt. Le sélecteur de dossier natif est asynchrone : la
    /// réponse revient dans une tâche, d'où le `spawn`.
    fn prompt_open_repository(&mut self, _window: &mut Window, cx: &mut Context<Self>) {
        let paths = cx.prompt_for_paths(gpui::PathPromptOptions {
            files: false,
            directories: true,
            multiple: true,
            prompt: None,
        });
        cx.spawn(async move |this, cx| {
            let Ok(Ok(Some(paths))) = paths.await else {
                return; // annulé
            };
            let _ = this.update(cx, |this, cx| {
                for path in paths {
                    this.git.send(Cmd::OpenRepo(path));
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn prompt_new_worktree(&mut self, main: PathBuf, window: &mut Window, cx: &mut Context<Self>) {
        self.open_text_dialog(
            tr!("worktree-new-title"),
            tr!("worktree-new-placeholder"),
            window,
            cx,
            move |this, name, cx| {
                let name = name.trim();
                if name.is_empty() {
                    return;
                }
                // Le worktree est créé à côté du dépôt, dans `<dépôt>-wt/<nom>`
                // — la convention de `wt`, pour que les deux outils voient les
                // mêmes dossiers.
                let repo_name = main
                    .file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_else(|| "repo".into());
                let root = main
                    .parent()
                    .map(|p| p.join(format!("{repo_name}-wt")))
                    .unwrap_or_else(|| main.join("worktrees"));
                this.git.send(Cmd::AddWorktree {
                    main: main.clone(),
                    path: root.join(name),
                    branch: format!("wt/{name}"),
                    from: None,
                });
                cx.notify();
            },
        );
    }

    fn confirm_remove_worktree(
        &mut self,
        main: PathBuf,
        path: PathBuf,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let label = path.display().to_string();
        let entity = cx.entity();
        window.open_dialog(cx, move |dialog, _window, _cx| {
            let (main, path, entity) = (main.clone(), path.clone(), entity.clone());
            dialog
                .title(tr!("worktree-remove-title"))
                .child(div().text_sm().child(label.clone()))
                .confirm()
                .on_ok(move |_, _window, cx| {
                    entity.update(cx, |this, cx| {
                        this.git.send(Cmd::RemoveWorktree {
                            main: main.clone(),
                            path: path.clone(),
                            // Sans `force`, git refuse de retirer un worktree
                            // qui a des modifications — c'est la protection
                            // qu'on veut ici, et le message le dira.
                            force: false,
                        });
                        cx.notify();
                    });
                    true
                })
        });
    }
}
