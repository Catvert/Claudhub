//! Les conflits, et le garde-fou d'une opération à mi-chemin.
//!
//! Un merge interrompu laisse le dépôt dans un état que rien ne nomme :
//! l'index porte des conflits, `HEAD` ne désigne pas ce qu'on croit, et la
//! liste des modifications se remplit de fichiers qu'on n'a pas touchés. Tant
//! qu'il dure, la barre d'état le dit et propose d'en sortir.
//!
//! **Une vue à trois volets n'est pas promise ici.** Les trois actions par
//! fichier — garder la nôtre, garder la leur, ouvrir dans l'éditeur — tiennent
//! en peu de code et couvrent ce qu'on fait dans la grande majorité des cas ;
//! une fusion à la main se fait dans l'éditeur, qui sait déjà le faire.

use std::path::PathBuf;

use gpui::{div, prelude::*, px, Context, SharedString, Window};
use gpui_component::{
    button::{Button, ButtonVariants},
    h_flex, v_flex, ActiveTheme, Sizable,
};

use crate::git::Pending;
use crate::runtime::Cmd;
use crate::tr;
use crate::ui::app::ClaudhubApp;
use crate::ui::icons::icon;

impl ClaudhubApp {
    /// L'opération en cours dans le worktree affiché, s'il y en a une.
    pub(super) fn pending_operation(&self) -> Option<Pending> {
        self.active_review()?.status.pending
    }

    /// Les fichiers que git déclare non fusionnés.
    pub(super) fn conflicted_files(&self) -> Vec<PathBuf> {
        self.active_review()
            .map(|state| {
                state
                    .status
                    .conflicted()
                    .map(|file| file.path.clone())
                    .collect()
            })
            .unwrap_or_default()
    }

    pub(super) fn resolve_conflict(&mut self, path: PathBuf, ours: bool, cx: &mut Context<Self>) {
        let Some(worktree) = self.active.clone() else {
            return;
        };
        self.git.send(Cmd::ResolveConflict {
            worktree,
            path,
            ours,
        });
        cx.notify();
    }

    /// Marque un fichier résolu : c'est une indexation, et rien d'autre.
    pub(super) fn mark_resolved(&mut self, path: PathBuf, cx: &mut Context<Self>) {
        let Some(worktree) = self.active.clone() else {
            return;
        };
        self.set_staged(worktree, vec![path], true, cx);
    }

    pub(super) fn abort_pending(&mut self, cx: &mut Context<Self>) {
        let Some(worktree) = self.active.clone() else {
            return;
        };
        self.git.send(Cmd::AbortPending { worktree });
        cx.notify();
    }

    pub(super) fn resume_pending(&mut self, cx: &mut Context<Self>) {
        let Some(worktree) = self.active.clone() else {
            return;
        };
        self.git.send(Cmd::ResumePending { worktree });
        cx.notify();
    }

    /// Le panneau des conflits.
    pub(super) fn render_conflicts(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let list_scroll = self.scroll_of("conflicts");
        let find = self.render_find(crate::ui::find::Pane::Conflicts, cx);
        let query = self.query(crate::ui::find::Pane::Conflicts, cx);
        let pending = self.pending_operation();
        let files: Vec<_> = self
            .conflicted_files()
            .into_iter()
            .filter(|path| crate::ui::find::matches(&query, &path.to_string_lossy()))
            .collect();
        let range = crate::git::DiffRange::Working;
        let selected = self
            .active_review()
            .and_then(|state| state.selected.clone());
        let (muted, warning) = (cx.theme().muted_foreground, cx.theme().warning);
        let mono = cx.theme().mono_font_family.clone();

        let bar = h_flex()
            .h(crate::ui::theme::bar_height(cx))
            .w_full()
            .px_2()
            .gap_2()
            .items_center()
            .border_b_1()
            .border_color(cx.theme().border)
            .child(icon("git-merge").xsmall())
            .child(
                div()
                    .flex_1()
                    .text_xs()
                    .text_color(if pending.is_some() { warning } else { muted })
                    .child(match pending {
                        Some(kind) => tr!(kind.key()),
                        None => tr!("conflict-count", { count: files.len() }),
                    }),
            )
            .children(pending.map(|_| self.render_pending_buttons("panel", cx)));

        if files.is_empty() {
            return v_flex()
                .size_full()
                .child(bar)
                .children(find)
                .child(
                    v_flex()
                        .size_full()
                        .items_center()
                        .justify_center()
                        .gap_2()
                        .text_color(muted)
                        .child(icon("git-merge"))
                        .child(div().text_sm().child(tr!("conflict-none"))),
                )
                .into_any_element();
        }

        // Peu de fichiers, presque toujours : une liste virtualisée n'aurait
        // rien à économiser, et chaque ligne porte trois boutons.
        let rows: Vec<_> = files
            .into_iter()
            .enumerate()
            .map(|(index, path)| {
                let is_open = selected.as_deref() == Some(path.as_path());
                self.render_conflict_row(index, path, range.clone(), is_open, mono.clone(), cx)
            })
            .collect();

        v_flex()
            .size_full()
            .child(bar)
            .children(find)
            .child(
                div().flex_1().min_h_0().child(
                    self.scrolled(
                        "conflict-bar",
                        &list_scroll,
                        crate::ui::motion::Axes::Vertical,
                        window,
                        v_flex()
                            .id("conflict-list")
                            .size_full()
                            .overflow_y_scroll()
                            .track_scroll(&list_scroll)
                            .children(rows),
                        cx,
                    ),
                ),
            )
            .into_any_element()
    }

    fn render_conflict_row(
        &mut self,
        index: usize,
        path: PathBuf,
        range: crate::git::DiffRange,
        is_open: bool,
        mono: SharedString,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let label = path.display().to_string();
        let (for_open, for_ours, for_theirs, for_done) =
            (path.clone(), path.clone(), path.clone(), path.clone());
        v_flex()
            .id(("conflict", index))
            .px_2()
            .py_1()
            .gap_1()
            .border_b_1()
            .border_color(cx.theme().border)
            .when(is_open, |el| el.bg(cx.theme().accent))
            .child(
                div()
                    .id(("conflict-path", index))
                    .text_sm()
                    .font_family(mono)
                    .truncate()
                    .cursor_pointer()
                    .on_click(cx.listener(move |this, _, _window, cx| {
                        let Some(worktree) = this.active.clone() else {
                            return;
                        };
                        this.open_file(worktree, for_open.clone(), range.clone(), cx);
                    }))
                    .child(label),
            )
            .child(
                h_flex()
                    .gap_1()
                    .child(
                        Button::new(("keep-ours", index))
                            .outline()
                            .xsmall()
                            .label(tr!("conflict-keep-ours"))
                            .on_click(cx.listener(move |this, _, _window, cx| {
                                this.resolve_conflict(for_ours.clone(), true, cx);
                            })),
                    )
                    .child(
                        Button::new(("keep-theirs", index))
                            .outline()
                            .xsmall()
                            .label(tr!("conflict-keep-theirs"))
                            .on_click(cx.listener(move |this, _, _window, cx| {
                                this.resolve_conflict(for_theirs.clone(), false, cx);
                            })),
                    )
                    .child(div().flex_1())
                    .child(
                        Button::new(("mark-resolved", index))
                            .ghost()
                            .xsmall()
                            .icon(icon("check"))
                            .tooltip(tr!("conflict-resolved"))
                            .on_click(cx.listener(move |this, _, _window, cx| {
                                this.mark_resolved(for_done.clone(), cx);
                            })),
                    ),
            )
    }

    /// Continuer / Abandonner. Les mêmes deux boutons dans la barre d'état et
    /// dans le panneau : c'est la seule chose à faire d'une opération à
    /// mi-chemin, et la chercher à un seul endroit serait une chasse au trésor.
    pub(super) fn render_pending_buttons(
        &self,
        scope: &'static str,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        h_flex()
            .gap_1()
            .child(
                Button::new(SharedString::from(format!("{scope}-continue")))
                    .outline()
                    .xsmall()
                    .label(tr!("pending-continue"))
                    .on_click(cx.listener(|this, _, _window, cx| this.resume_pending(cx))),
            )
            .child(
                Button::new(SharedString::from(format!("{scope}-abort")))
                    .ghost()
                    .xsmall()
                    .label(tr!("pending-abort"))
                    .on_click(cx.listener(|this, _, _window, cx| this.abort_pending(cx))),
            )
    }

    /// Ce que la barre d'état affiche d'une opération en cours.
    pub(super) fn render_pending_bar(&self, cx: &mut Context<Self>) -> Option<impl IntoElement> {
        let kind = self.pending_operation()?;
        let warning = cx.theme().warning;
        Some(
            h_flex()
                .gap_1()
                .items_center()
                .child(div().text_color(warning).child(icon("git-merge").xsmall()))
                .child(div().text_color(warning).child(tr!(kind.key())))
                .child(self.render_pending_buttons("bar", cx))
                .child(gpui_component::separator::Separator::vertical().h(px(12.))),
        )
    }
}
