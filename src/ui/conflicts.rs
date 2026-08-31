//! Conflicts, and the guard rail of a half-finished operation.
//!
//! An interrupted merge leaves the repository in a state nothing names: the
//! index carries conflicts, `HEAD` does not point where you think, and the
//! change list fills with files you never touched. While it lasts, the status
//! bar says so and offers a way out.
//!
//! **Clicking a file opens the three-pane view** (`ui::merge_view`); the two
//! buttons on its row settle the cases that need no reading at all — a file one
//! has already decided about as a whole — and stay the only way out of the
//! conflicts three columns cannot show: a binary file, and a file one side
//! deleted.

use std::path::PathBuf;

use gpui::{div, prelude::*, px, Context, SharedString, Window};
use gpui_component::{
    button::{Button, ButtonVariants},
    h_flex, v_flex, ActiveTheme, Sizable,
};

use crate::git::Pending;
use crate::runtime::{Action, Cmd};
use crate::tr;
use crate::ui::app::ClaudhubApp;
use crate::ui::icons::icon;

impl ClaudhubApp {
    /// The operation in progress in the displayed worktree, if there is one.
    pub(super) fn pending_operation(&self) -> Option<Pending> {
        self.active_review()?.status.pending
    }

    /// The files git reports as unmerged.
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

    /// Is there one at all, without building the list.
    ///
    /// Read on every notification of the application — the panel's tab appears
    /// and disappears with it — where the paths themselves are not wanted.
    pub(super) fn has_conflicts(&self) -> bool {
        self.active_review()
            .is_some_and(|state| state.status.conflicted().next().is_some())
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

    /// Marks a file resolved: that is a staging, and nothing else.
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
        let cmd = Cmd::AbortPending {
            worktree: worktree.clone(),
        };
        self.start(Some(worktree), Action::Abort, cmd, cx);
    }

    pub(super) fn resume_pending(&mut self, cx: &mut Context<Self>) {
        let Some(worktree) = self.active.clone() else {
            return;
        };
        let cmd = Cmd::ResumePending {
            worktree: worktree.clone(),
        };
        self.start(Some(worktree), Action::Resume, cmd, cx);
    }

    /// The conflicts panel.
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

        // Few files, almost always: a virtualised list would have nothing to
        // save, and each row carries three buttons.
        let rows: Vec<_> = files
            .into_iter()
            .enumerate()
            .map(|(index, path)| {
                let is_open = selected.as_deref() == Some(path.as_path());
                self.render_conflict_row(index, path, is_open, mono.clone(), cx)
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
                            // The bar is painted over the content: what a row
                            // carries on its right — here the two buttons that
                            // settle a file — would sit under it.
                            .pr(crate::ui::theme::scroll_gutter())
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
                    .on_click(cx.listener(move |this, _, window, cx| {
                        let Some(worktree) = this.active.clone() else {
                            return;
                        };
                        this.open_merge(worktree, for_open.clone(), window, cx);
                    }))
                    .child(label),
            )
            .child(
                h_flex()
                    .gap_1()
                    .child(
                        Button::new(("keep-ours", index))
                            .outline()
                            .small()
                            .label(tr!("conflict-keep-ours"))
                            .on_click(cx.listener(move |this, _, _window, cx| {
                                this.resolve_conflict(for_ours.clone(), true, cx);
                            })),
                    )
                    .child(
                        Button::new(("keep-theirs", index))
                            .outline()
                            .small()
                            .label(tr!("conflict-keep-theirs"))
                            .on_click(cx.listener(move |this, _, _window, cx| {
                                this.resolve_conflict(for_theirs.clone(), false, cx);
                            })),
                    )
                    .child(div().flex_1())
                    .child(
                        Button::new(("mark-resolved", index))
                            .ghost()
                            .small()
                            .icon(icon("check"))
                            .tooltip(tr!("conflict-resolved"))
                            .on_click(cx.listener(move |this, _, _window, cx| {
                                this.mark_resolved(for_done.clone(), cx);
                            })),
                    ),
            )
    }

    /// Continue / Abort. The same two buttons in the status bar and in the
    /// panel: it is the only thing to do with a half-finished operation, and
    /// looking for it in a single place would be a treasure hunt.
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
                    .small()
                    .label(tr!("pending-continue"))
                    .on_click(cx.listener(|this, _, _window, cx| this.resume_pending(cx))),
            )
            .child(
                Button::new(SharedString::from(format!("{scope}-abort")))
                    .ghost()
                    .small()
                    .label(tr!("pending-abort"))
                    .on_click(cx.listener(|this, _, _window, cx| this.abort_pending(cx))),
            )
    }

    /// What the status bar shows of an operation in progress.
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
