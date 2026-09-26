//! The home screen's changes node, and the review it opens.
//!
//! **The node** hangs under a worktree's card while the worktree has anything
//! to commit, and lists it: the files, what git says of each, and whether it
//! is in the index — the tick is the Changes panel's, and works from here on
//! a worktree nobody has opened. A file pressed, or the button at its foot,
//! opens the review.
//!
//! **The review** is the editor's two panels in a dialog: the Changes list
//! with its commit box on the left, the diff on the right — the same
//! functions, the same state, the same draft. Both speak of the worktree on
//! show, so opening the review selects the card first, as the branch picker
//! does: one surface, and what is started here is finished in the editor and
//! back. A second copy of either panel would be a second set of bugs.

use std::path::{Path, PathBuf};

use gpui_kit::component::{
    button::{Button, ButtonVariants as _},
    checkbox::Checkbox,
    h_flex, v_flex, ActiveTheme, Disableable as _, Sizable as _, WindowExt as _,
};
use gpui_kit::{
    div, prelude::*, px, AnyElement, Context, MouseButton, SharedString, WeakEntity, Window,
};

use crate::git::{DiffRange, FileStatus, StatusCode};
use crate::tr;
use crate::ui::app::ClaudhubApp;
use crate::ui::icons::icon;
use crate::ui::overview::{self, Node};

/// The review at its widest and tallest: two columns, one of them code.
const MAX_WIDTH: gpui_kit::Pixels = px(1440.);
const MAX_HEIGHT: gpui_kit::Pixels = px(900.);
/// The list's column in the review; the diff takes the rest.
const LIST_WIDTH: gpui_kit::Pixels = px(380.);

/// The review in its dialog: a child entity, for the reason of the settings
/// form — `open_dialog` keeps a `Fn` called back from the root's render,
/// where reading the application panics, and a child's render runs after.
pub(super) struct ReviewSheet {
    app: WeakEntity<ClaudhubApp>,
}

impl Render for ReviewSheet {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let Some(app) = self.app.upgrade() else {
            return div().into_any_element();
        };
        app.update(cx, |app, cx| app.render_review_sheet(window, cx))
    }
}

/// The two letters git gives a file, folded into what the list shows: `?`
/// for a file git does not know, otherwise the index's and the worktree's —
/// the Changes panel's reading.
fn codes(file: &FileStatus) -> (String, StatusCode) {
    if file.is_untracked() {
        return ("?".into(), StatusCode::Untracked);
    }
    let (index, worktree) = (file.index.letter(), file.worktree.letter());
    let tint = if file.is_unstaged() {
        file.worktree
    } else {
        file.index
    };
    let text = match (index.trim().is_empty(), worktree.trim().is_empty()) {
        (true, _) => worktree.to_string(),
        (_, true) => index.to_string(),
        _ => format!("{index}{worktree}"),
    };
    (text, tint)
}

impl ClaudhubApp {
    /// A worktree's changes node: its head, its files, and the way into the
    /// review.
    pub(super) fn render_changes_node(
        &self,
        path: &Path,
        zoom: f32,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let theme = cx.theme().clone();
        let muted = theme.muted_foreground;
        let detail = zoom >= super::overview_view::DETAIL;
        let node = Node::Changes(path.to_path_buf());
        let summary = self.summaries.get(path).copied().unwrap_or_default();
        // The list is the status's, which a worktree nobody opened has only
        // once the home screen asked for it — `ensure_changes_read`.
        let files: Option<Vec<FileStatus>> =
            self.fresh_status(path).map(|status| status.files.clone());
        let staged = files
            .as_ref()
            .map_or(0, |files| files.iter().filter(|f| f.is_staged()).count());
        let tint = theme.success;
        let app = cx.entity().downgrade();

        let head = h_flex()
            .flex_none()
            .h(px(overview::HEAD * zoom))
            .px_2()
            .gap_1p5()
            .items_center()
            .bg(tint.opacity(0.14))
            .cursor_grab()
            .on_mouse_down(
                MouseButton::Left,
                super::overview_view::grab(app.clone(), node.clone()),
            )
            .child(icon("file-diff").text_color(tint))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .font_weight(gpui_kit::FontWeight::SEMIBOLD)
                    .child(tr!("overview-changes")),
            )
            .child(
                div()
                    .flex_none()
                    .text_xs()
                    .text_color(muted)
                    .child(tr!("home-files", { count: summary.files })),
            )
            .child(super::topbar::volume_on(summary, None, cx))
            .when(detail, |el| {
                el.child(self.window_controls(node.clone(), cx))
            });

        let worktree = path.to_path_buf();
        let body = match files {
            None => div()
                .flex_1()
                .p_2()
                .text_xs()
                .text_color(muted)
                .child(tr!("overview-changes-reading"))
                .into_any_element(),
            Some(files) => v_flex()
                .id(SharedString::from(format!(
                    "overview-changes-list-{}",
                    path.display()
                )))
                .flex_1()
                .min_h_0()
                .py_1()
                .overflow_y_scroll()
                .children(
                    files
                        .into_iter()
                        .enumerate()
                        .map(|(index, file)| self.render_changes_row(&worktree, index, file, cx)),
                )
                .into_any_element(),
        };

        let open = path.to_path_buf();
        let foot = h_flex()
            .flex_none()
            .px_2()
            .py_1p5()
            .gap_2()
            .items_center()
            .border_t_1()
            .border_color(theme.border)
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .text_xs()
                    .text_color(muted)
                    .child(tr!("commit-staged-count", { count: staged })),
            )
            .child(
                Button::new(SharedString::from(format!(
                    "overview-changes-review-{}",
                    path.display()
                )))
                .primary()
                .xsmall()
                .icon(icon("git-commit-horizontal"))
                .label(tr!("overview-changes-review"))
                .on_click(cx.listener(move |this, _, window, cx| {
                    this.open_review_sheet(&open, None, window, cx);
                })),
            );

        v_flex()
            .size_full()
            .overflow_hidden()
            .rounded(theme.radius_lg)
            .border_1()
            .border_color(tint.opacity(0.45))
            .bg(theme.background)
            .text_sm()
            // Its rows are pressed, not the node dragged by them.
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .child(head)
            .child(body)
            .when(detail, |el| el.child(foot))
            .into_any_element()
    }

    /// One file of the node: its tick, its code, its name and folder. The
    /// tick stages or unstages it where it is; the rest opens the review on
    /// it.
    fn render_changes_row(
        &self,
        worktree: &Path,
        index: usize,
        file: FileStatus,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let theme = cx.theme().clone();
        // Ticked when all of it is in the index; a press puts the rest in, or
        // takes all of it out — the Changes panel's box.
        let ticked = file.is_staged() && !file.is_unstaged();
        let conflicted = file.is_conflicted();
        let (code, tint) = codes(&file);
        let color = super::theme::status_color(tint, cx);
        let name = file.file_name();
        let folder = file.directory();
        let (tick_worktree, tick_path) = (worktree.to_path_buf(), file.path.clone());
        let (open_worktree, open_path) = (worktree.to_path_buf(), file.path.clone());
        h_flex()
            .id(("overview-changes-row", index))
            .w_full()
            .px_2()
            .h(super::theme::row_height(cx))
            .gap_1p5()
            .items_center()
            .cursor_pointer()
            .hover(|style| style.bg(theme.list_hover))
            .on_click(cx.listener(move |this, _, window, cx| {
                this.open_review_sheet(&open_worktree, Some(open_path.clone()), window, cx);
            }))
            .child(
                div()
                    // The tick is its own gesture, not the row's.
                    .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                    .child(
                        Checkbox::new(("overview-changes-stage", index))
                            .checked(ticked)
                            .disabled(conflicted || !(ticked || file.is_stageable()))
                            .on_click(cx.listener(move |this, _, _, cx| {
                                cx.stop_propagation();
                                this.set_staged(
                                    tick_worktree.clone(),
                                    vec![tick_path.clone()],
                                    !ticked,
                                    cx,
                                );
                            })),
                    ),
            )
            .child(
                div()
                    .flex_none()
                    .w(px(18.))
                    .text_xs()
                    .font_family(theme.mono_font_family.clone())
                    .text_color(color)
                    .child(SharedString::from(code)),
            )
            .child(crate::ui::file_icons::file_icon(&file.path, cx))
            .child(
                h_flex()
                    .flex_1()
                    .min_w_0()
                    .gap_1()
                    .overflow_hidden()
                    .child(
                        div()
                            .flex_none()
                            .max_w_full()
                            .truncate()
                            .when(conflicted, |el| el.text_color(theme.danger))
                            .child(SharedString::from(name)),
                    )
                    .when(!folder.is_empty(), |el| {
                        el.child(
                            div()
                                .min_w_0()
                                .truncate()
                                .text_xs()
                                .text_color(theme.muted_foreground)
                                .child(SharedString::from(folder)),
                        )
                    }),
            )
            .into_any_element()
    }

    /// The worktree's status, when it is the latest word on it: asked since
    /// the sweep's count last moved, and answered.
    fn fresh_status(&self, worktree: &Path) -> Option<&crate::git::Status> {
        (self.changes_read.contains(worktree) && !self.pending_status.contains(worktree))
            .then(|| self.review.get(worktree).map(|review| &review.status))
            .flatten()
    }

    /// Whether a worktree has anything to commit, for its changes node.
    ///
    /// The fresh status decides when there is one, the sweep's count
    /// otherwise: the count is every worktree's but lags a commit by a whole
    /// sweep, and the node would stay behind, empty, over what was just
    /// committed.
    pub(super) fn has_changes(&self, worktree: &Path) -> bool {
        match self.fresh_status(worktree) {
            Some(status) => !status.files.is_empty(),
            None => self
                .summaries
                .get(worktree)
                .is_some_and(|summary| !summary.is_empty()),
        }
    }

    /// Asks the status of every worktree on the home screen that has
    /// something to commit and whose list is not in hand: the node shows the
    /// files, and the background sweep only counts them. Once per worktree
    /// until its count moves — `summaries_arrived` forgets the ones that did.
    pub(super) fn ensure_changes_read(&mut self, worktrees: &[PathBuf], cx: &mut Context<Self>) {
        for worktree in worktrees {
            let dirty = self
                .summaries
                .get(worktree)
                .is_some_and(|summary| !summary.is_empty());
            if !dirty || self.changes_read.contains(worktree) {
                continue;
            }
            self.changes_read.insert(worktree.clone());
            self.ensure_review(worktree, cx);
            self.request_status(worktree.clone());
        }
    }

    /// The sweep's counts: a worktree whose count moved has its list read
    /// again the next time the home screen paints it.
    pub(super) fn summaries_arrived(&mut self, summaries: Vec<(PathBuf, crate::git::Summary)>) {
        for (worktree, summary) in &summaries {
            if self.summaries.get(worktree) != Some(summary) {
                self.changes_read.remove(worktree);
            }
        }
        self.summaries.extend(summaries);
    }

    /// Opens the review of a worktree's changes, on `file` if one was
    /// pressed, else on the first one there is.
    pub(super) fn open_review_sheet(
        &mut self,
        worktree: &Path,
        file: Option<PathBuf>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.active.as_deref() != Some(worktree) {
            self.select_worktree(worktree.to_path_buf(), window, cx);
        }
        match file {
            Some(file) => {
                self.review_sheet_pick = false;
                self.open_file(worktree.to_path_buf(), file, DiffRange::Working, cx);
            }
            // Picked once the list is known — it may still be on its way.
            None => self.review_sheet_pick = true,
        }
        if self.commit_sheet {
            // Already open: the file pressed is now the one it shows.
            cx.notify();
            return;
        }
        let (repo, label) = self.project_label(worktree);
        let name = match repo {
            Some(repo) => format!("{repo} · {label}"),
            None => label.to_string(),
        };
        let title = tr!("overview-commit-title", { name: name });
        let app = cx.entity().downgrade();
        let sheet = cx.new(|_| ReviewSheet { app: app.clone() });
        self.commit_sheet = true;
        window.open_dialog(cx, move |dialog, window, _| {
            // Read off the window, as the quick palette is: two columns of
            // which one is code, and code in six hundred pixels is read a
            // word per line. Rebuilt on every frame, so it follows a resize.
            let viewport = window.viewport_size();
            let width = viewport.width.min(MAX_WIDTH) - px(64.);
            let height = viewport.height.min(MAX_HEIGHT) - px(120.);
            let app = app.clone();
            dialog
                .title(title.clone())
                .w(width)
                .max_w(width)
                .overlay_closable(true)
                .close_button(true)
                // A definite box: the two panels are `size_full`, which
                // resolves against nothing in a box sized by its content.
                .child(div().w_full().h(height).child(sheet.clone()))
                // Entrée belongs to the message field and the list, never to
                // the dialog: it would shut the review on the key meant to
                // write in it.
                .on_ok(|_, _, _| false)
                .on_close(move |_, _, cx| {
                    if let Some(app) = app.upgrade() {
                        app.update(cx, |this, _| {
                            this.commit_sheet = false;
                            this.review_sheet_pick = false;
                        });
                    }
                })
        });
        super::dialogs::focus_field(&self.commit_input, window, cx);
        cx.notify();
    }

    /// The review's two columns: the Changes panel, and the diff.
    fn render_review_sheet(&mut self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        self.pick_review_file(cx);
        let border = cx.theme().border;
        h_flex()
            .size_full()
            .gap_3()
            .child(
                v_flex()
                    .flex_none()
                    .w(LIST_WIDTH)
                    .h_full()
                    .overflow_hidden()
                    .rounded(cx.theme().radius)
                    .border_1()
                    .border_color(border)
                    .child(self.render_changes(window, cx)),
            )
            .child(
                v_flex()
                    .flex_1()
                    .min_w_0()
                    .h_full()
                    .overflow_hidden()
                    .rounded(cx.theme().radius)
                    .border_1()
                    .border_color(border)
                    .child(self.render_diff(window, cx)),
            )
            .into_any_element()
    }

    /// The first file, once the list is there, when the review was opened
    /// without one — and the diff brought back to the changes in progress,
    /// whatever range the editor had left it on.
    fn pick_review_file(&mut self, cx: &mut Context<Self>) {
        if !self.review_sheet_pick {
            return;
        }
        let Some(worktree) = self.active.clone() else {
            return;
        };
        let Some(state) = self.review.get(&worktree) else {
            return;
        };
        let Some(first) = state.status.files.first().map(|file| file.path.clone()) else {
            return;
        };
        self.review_sheet_pick = false;
        let kept = state.range == DiffRange::Working
            && state
                .selected
                .as_deref()
                .is_some_and(|selected| state.status.file(selected).is_some());
        if !kept {
            self.open_file(worktree, first, DiffRange::Working, cx);
        }
    }
}
