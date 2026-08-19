//! Panneau des branches.
//!
//! Il sert à deux choses : basculer le worktree courant sur une autre branche,
//! et créer un worktree à partir d'une branche existante — le geste de départ
//! d'une relecture, quand le travail d'un agent est arrivé sur une branche
//! qu'on n'a pas encore déployée.

use gpui::{div, prelude::*, px, Context};
use gpui_component::{
    button::{Button, ButtonVariants},
    h_flex, v_flex, ActiveTheme, Disableable, Sizable, StyledExt,
};

use crate::git::BranchKind;
use crate::runtime::Cmd;
use crate::tr;
use crate::ui::app::PerchApp;
use crate::ui::icons::icon;

impl PerchApp {
    pub(super) fn render_branches(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let Some(worktree) = self.active.clone() else {
            return div().into_any_element();
        };
        let Some(repo) = self.repo_of(&worktree) else {
            return div().into_any_element();
        };
        let main = repo.main.clone();
        let branches: Vec<_> = repo
            .branches
            .iter()
            .map(|b| {
                (
                    b.name.clone(),
                    b.kind,
                    b.is_head,
                    b.date.clone(),
                    b.subject.clone(),
                    b.checked_out_at.clone(),
                )
            })
            .collect();

        v_flex()
            .h(px(260.))
            .w_full()
            .border_t_1()
            .border_color(cx.theme().border)
            .child(
                h_flex()
                    .h(crate::ui::theme::bar_height(cx))
                    .px_2()
                    .items_center()
                    .justify_between()
                    .child(
                        div()
                            .text_xs()
                            .font_semibold()
                            .text_color(cx.theme().muted_foreground)
                            .child(tr!("panel-branches")),
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
                    ),
            )
            .child(
                div()
                    .id("branch-list")
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .children(branches.into_iter().enumerate().map(
                        |(ix, (name, kind, is_head, date, subject, used_by))| {
                            let for_checkout = name.clone();
                            let for_worktree = name.clone();
                            let main_for_worktree = main.clone();
                            let worktree = worktree.clone();
                            // Git refuse deux checkouts de la même branche :
                            // le bouton est désactivé plutôt que de promettre
                            // une erreur.
                            let taken = used_by.is_some() && !is_head;
                            h_flex()
                                .id(("branch", ix))
                                .h(crate::ui::theme::row_height(cx))
                                .px_2()
                                .gap_2()
                                .items_center()
                                .when(is_head, |el| el.bg(cx.theme().accent.opacity(0.5)))
                                .hover(|s| s.bg(cx.theme().accent.opacity(0.3)))
                                .child(
                                    icon(match kind {
                                        BranchKind::Local => "git-branch",
                                        BranchKind::Remote => "download",
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
                                                .when(is_head, |el| el.font_semibold())
                                                .child(name),
                                        )
                                        .child(
                                            div()
                                                .truncate()
                                                .text_xs()
                                                .text_color(cx.theme().muted_foreground)
                                                .child(format!("{date} · {subject}")),
                                        ),
                                )
                                .when(!is_head, |el| {
                                    el.child(
                                        Button::new(("checkout", ix))
                                            .ghost()
                                            .xsmall()
                                            .icon(icon("check"))
                                            .tooltip(tr!("branch-checkout"))
                                            .disabled(taken)
                                            .on_click(cx.listener(move |this, _, _, cx| {
                                                this.git.send(Cmd::Checkout {
                                                    worktree: worktree.clone(),
                                                    branch: for_checkout.clone(),
                                                });
                                                cx.notify();
                                            })),
                                    )
                                    .child(
                                        Button::new(("worktree-from", ix))
                                            .ghost()
                                            .xsmall()
                                            .icon(icon("folder-open"))
                                            .tooltip(tr!("branch-new-worktree"))
                                            .disabled(taken)
                                            .on_click(cx.listener(move |this, _, _, cx| {
                                                this.worktree_from_branch(
                                                    main_for_worktree.clone(),
                                                    for_worktree.clone(),
                                                    cx,
                                                );
                                            })),
                                    )
                                })
                        },
                    )),
            )
            .into_any_element()
    }

    fn prompt_new_branch(&mut self, window: &mut gpui::Window, cx: &mut Context<Self>) {
        self.open_text_dialog(
            tr!("branch-new"),
            tr!("branch-new-placeholder"),
            window,
            cx,
            |this, name, cx| {
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
    fn worktree_from_branch(
        &mut self,
        main: std::path::PathBuf,
        branch: String,
        cx: &mut Context<Self>,
    ) {
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
