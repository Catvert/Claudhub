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

use std::cell::Cell;
use std::path::{Path, PathBuf};
use std::rc::Rc;

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
/// What a maximized sheet leaves of the window round it.
const SHEET_MARGIN: gpui_kit::Pixels = px(16.);

/// What a sheet shows beside the diff.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum SheetKind {
    /// The changes in progress and the commit box: « Commit ».
    Commit,
    /// What the branch has written since its base: « Review ».
    Review,
}

/// A sheet in its dialog: a child entity, for the reason of the settings
/// form — `open_dialog` keeps a `Fn` called back from the root's render,
/// where reading the application panics, and a child's render runs after.
///
/// **It draws its own head** — its title, what it is compared against, and
/// the buttons of a window: the dialog's title could say none of the rest,
/// and a sheet two columns of code wide is one that wants the whole screen
/// now and then. `maximized` is shared with the dialog's closure, which
/// cannot read the application to size itself.
pub(super) struct ReviewSheet {
    app: WeakEntity<ClaudhubApp>,
    kind: SheetKind,
    maximized: Rc<Cell<bool>>,
}

impl Render for ReviewSheet {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let Some(app) = self.app.upgrade() else {
            return div().into_any_element();
        };
        let (kind, maximized) = (self.kind, self.maximized.clone());
        app.update(cx, |app, cx| {
            app.render_review_sheet(kind, maximized, window, cx)
        })
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
            self.listed_status(path).map(|status| status.files.clone());
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

    /// The changes as a section of the worktree's own card — the focus
    /// view's, where the card and its changes are one card: a rule, a line
    /// that names them with their count, the files, and the way into the
    /// review. `None` with nothing to commit.
    pub(super) fn render_changes_section(
        &self,
        path: &Path,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        if !self.has_changes(path) {
            return None;
        }
        let theme = cx.theme().clone();
        let muted = theme.muted_foreground;
        let summary = self.summaries.get(path).copied().unwrap_or_default();
        let files: Option<Vec<FileStatus>> =
            self.listed_status(path).map(|status| status.files.clone());
        let staged = files
            .as_ref()
            .map_or(0, |files| files.iter().filter(|f| f.is_staged()).count());
        let worktree = path.to_path_buf();
        let list = match files {
            None => div()
                .py_1()
                .text_xs()
                .text_color(muted)
                .child(tr!("overview-changes-reading"))
                .into_any_element(),
            Some(files) => v_flex()
                .id(SharedString::from(format!(
                    "focus-changes-list-{}",
                    path.display()
                )))
                .w_full()
                // A long list scrolls inside the card rather than making the
                // card the height of the list: the commits and the button
                // stay in view.
                .max_h(px(320.))
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
        Some(
            v_flex()
                .id(SharedString::from(format!(
                    "focus-changes-{}",
                    path.display()
                )))
                // Its gestures are its own: a press here neither selects the
                // card nor, twice, leaves the screen for the editor.
                .on_click(|_, _, cx| cx.stop_propagation())
                .w_full()
                .gap_1()
                .pt_2()
                .border_t_1()
                .border_color(theme.border)
                .child(
                    h_flex()
                        .gap_1p5()
                        .items_center()
                        .child(icon("file-diff").xsmall().text_color(theme.success))
                        .child(
                            div()
                                .text_xs()
                                .font_weight(gpui_kit::FontWeight::SEMIBOLD)
                                .child(tr!("overview-changes")),
                        )
                        .child(
                            div()
                                .text_xs()
                                .text_color(muted)
                                .child(tr!("home-files", { count: summary.files })),
                        )
                        .child(super::topbar::volume_on(summary, None, cx))
                        .child(div().flex_1())
                        .child(
                            div()
                                .text_xs()
                                .text_color(muted)
                                .child(tr!("commit-staged-count", { count: staged })),
                        )
                        .child(
                            Button::new(SharedString::from(format!(
                                "focus-changes-review-{}",
                                path.display()
                            )))
                            .primary()
                            .xsmall()
                            .icon(icon("git-commit-horizontal"))
                            .label(tr!("overview-changes-review"))
                            .on_click(cx.listener(
                                move |this, _, window, cx| {
                                    this.open_review_sheet(&open, None, window, cx);
                                },
                            )),
                        ),
                )
                // The rows run to the card's edges, as a list's do: the
                // body's padding is given back.
                .child(div().mx(px(-8.)).child(list))
                .into_any_element(),
        )
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

    /// The status the list shows: the last one read, **kept while the next
    /// one is on its way**. The watcher asks again at every write in the
    /// worktree, and a list that went back to « reading » for the length of
    /// each `git status` blinked, and the card with it, a line tall one
    /// moment and a list the next. Only a first reading shows the wait — or
    /// a reading after an empty list, where the count says there is more.
    fn listed_status(&self, worktree: &Path) -> Option<&crate::git::Status> {
        let status = self
            .review
            .get(worktree)
            .filter(|_| self.status_seen.contains(worktree))
            .map(|review| &review.status)?;
        (!status.files.is_empty() || !self.pending_status.contains(worktree)).then_some(status)
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
        self.forget_closed_sheets(window, cx);
        if self.commit_sheet {
            // Already open: the file pressed is now the one it shows.
            cx.notify();
            return;
        }
        self.show_sheet(SheetKind::Commit, window, cx);
        super::dialogs::focus_field(&self.commit_input, window, cx);
        cx.notify();
    }

    /// Opens the review of what a worktree's branch has written since its
    /// base, on its first file — the editor's branch review and its diff,
    /// in the sheet the commit has.
    pub(super) fn open_branch_sheet(
        &mut self,
        worktree: &Path,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.active.as_deref() != Some(worktree) {
            self.select_worktree(worktree.to_path_buf(), window, cx);
        }
        self.branch_sheet_pick = true;
        self.forget_closed_sheets(window, cx);
        if !self.branch_sheet {
            self.show_sheet(SheetKind::Review, window, cx);
        }
        cx.notify();
    }

    /// A sheet closed from its own head. **`close_dialog` does not call the
    /// dialog's `on_close`** — it only pops it — so what that would have
    /// reset is reset here, or the sheet stays « open » and its button
    /// never opens it again.
    fn close_sheet(&mut self, kind: SheetKind, window: &mut Window, cx: &mut Context<Self>) {
        match kind {
            SheetKind::Commit => {
                self.commit_sheet = false;
                self.review_sheet_pick = false;
            }
            SheetKind::Review => {
                self.branch_sheet = false;
                self.branch_sheet_pick = false;
            }
        }
        window.close_dialog(cx);
        cx.notify();
    }

    /// A sheet is open only while a dialog is: whichever way one went —
    /// and a path that closes dialogs without their `on_close` is easy to
    /// add — a flag left up must not keep a sheet from opening again.
    fn forget_closed_sheets(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !window.has_active_dialog(cx) {
            self.commit_sheet = false;
            self.branch_sheet = false;
        }
    }

    /// The sheet's dialog: its size read off the window at every frame —
    /// two columns of which one is code, and code in six hundred pixels is
    /// read a word per line — the whole window when maximized. No title and
    /// no cross of the dialog's: the sheet draws its own head.
    fn show_sheet(&mut self, kind: SheetKind, window: &mut Window, cx: &mut Context<Self>) {
        let app = cx.entity().downgrade();
        let maximized = Rc::new(Cell::new(self.sheet_maximized));
        let sheet = cx.new(|_| ReviewSheet {
            app: app.clone(),
            kind,
            maximized: maximized.clone(),
        });
        match kind {
            SheetKind::Commit => self.commit_sheet = true,
            SheetKind::Review => self.branch_sheet = true,
        }
        window.open_dialog(cx, move |dialog, window, _| {
            let viewport = window.viewport_size();
            let (width, height, top) = if maximized.get() {
                (
                    viewport.width - SHEET_MARGIN * 2.,
                    viewport.height - SHEET_MARGIN * 2.,
                    SHEET_MARGIN,
                )
            } else {
                let width = viewport.width.min(MAX_WIDTH) - px(64.);
                let height = viewport.height.min(MAX_HEIGHT) - px(80.);
                (
                    width,
                    height,
                    ((viewport.height - height) / 2.).max(SHEET_MARGIN),
                )
            };
            let app = app.clone();
            dialog
                .w(width)
                .max_w(width)
                .margin_top(top)
                .p_0()
                .close_button(false)
                .overlay_closable(true)
                // A definite box: the two panels are `size_full`, which
                // resolves against nothing in a box sized by its content.
                .child(div().w_full().h(height).child(sheet.clone()))
                // Entrée belongs to the message field and the list, never to
                // the dialog: it would shut the sheet on the key meant to
                // write in it.
                .on_ok(|_, _, _| false)
                .on_close(move |_, _, cx| {
                    if let Some(app) = app.upgrade() {
                        app.update(cx, |this, _| match kind {
                            SheetKind::Commit => {
                                this.commit_sheet = false;
                                this.review_sheet_pick = false;
                            }
                            SheetKind::Review => {
                                this.branch_sheet = false;
                                this.branch_sheet_pick = false;
                            }
                        });
                    }
                })
        });
    }

    /// A sheet: its head, then its two columns — the list, and the diff.
    fn render_review_sheet(
        &mut self,
        kind: SheetKind,
        maximized: Rc<Cell<bool>>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        match kind {
            SheetKind::Commit => self.pick_review_file(cx),
            SheetKind::Review => self.pick_branch_file(cx),
        }
        let head = self.render_sheet_head(kind, maximized, cx);
        let list = match kind {
            SheetKind::Commit => self.render_changes(window, cx).into_any_element(),
            SheetKind::Review => self.render_branch_review(window, cx).into_any_element(),
        };
        let border = cx.theme().border;
        v_flex()
            .size_full()
            .child(head)
            .child(
                h_flex()
                    .flex_1()
                    .min_h_0()
                    .w_full()
                    .p_3()
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
                            .child(list),
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
                    ),
            )
            .into_any_element()
    }

    /// A sheet's head: what it is and whose — the review saying what it
    /// compares against — and the two buttons of a window: maximize, which
    /// a double click on the head does too, and close.
    fn render_sheet_head(
        &mut self,
        kind: SheetKind,
        maximized: Rc<Cell<bool>>,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let theme = cx.theme().clone();
        let name = match self.active.as_deref() {
            Some(worktree) => {
                let (repo, label) = self.project_label(worktree);
                match repo {
                    Some(repo) => format!("{repo} · {label}"),
                    None => label.to_string(),
                }
            }
            None => String::new(),
        };
        let (glyph, title) = match kind {
            SheetKind::Commit => (
                "git-commit-horizontal",
                tr!("overview-commit-title", { name: name }),
            ),
            SheetKind::Review => ("file-diff", tr!("sheet-review-title", { name: name })),
        };
        let against = (kind == SheetKind::Review)
            .then(|| self.active_review())
            .flatten()
            .and_then(
                |state| match (state.since_review, state.review_point.as_ref()) {
                    (true, Some(_)) => Some(tr!("sheet-review-since")),
                    _ => state
                        .base
                        .clone()
                        .map(|base| tr!("sheet-review-against", { base: base })),
                },
            );
        let big = maximized.get();
        let toggle = {
            let maximized = maximized.clone();
            move |this: &mut Self, window: &mut Window, cx: &mut Context<Self>| {
                let next = !maximized.get();
                maximized.set(next);
                this.sheet_maximized = next;
                window.refresh();
                cx.notify();
            }
        };
        let on_double = toggle.clone();
        h_flex()
            .id("sheet-head")
            .flex_none()
            .w_full()
            .pl_4()
            .pr_2()
            .py_2()
            .gap_2()
            .items_center()
            .border_b_1()
            .border_color(theme.border)
            .on_click(
                cx.listener(move |this, event: &gpui_kit::ClickEvent, window, cx| {
                    if event.click_count() >= 2 {
                        on_double(this, window, cx);
                    }
                }),
            )
            .child(icon(glyph).text_color(theme.muted_foreground))
            .child(
                div()
                    .flex_none()
                    .max_w(gpui_kit::relative(0.6))
                    .truncate()
                    .font_weight(gpui_kit::FontWeight::SEMIBOLD)
                    .child(title),
            )
            .children(against.map(|against| {
                div()
                    .min_w_0()
                    .truncate()
                    .text_sm()
                    .text_color(theme.muted_foreground)
                    .child(against)
            }))
            .child(div().flex_1())
            .child(
                Button::new("sheet-maximize")
                    .ghost()
                    .small()
                    .icon(icon(if big { "minimize" } else { "maximize" }))
                    .tooltip(if big {
                        tr!("sheet-restore")
                    } else {
                        tr!("sheet-maximize")
                    })
                    .on_click(cx.listener(move |this, _, window, cx| toggle(this, window, cx))),
            )
            .child(
                Button::new("sheet-close")
                    .ghost()
                    .small()
                    .icon(icon("x"))
                    .tooltip(tr!("sheet-close"))
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.close_sheet(kind, window, cx);
                    })),
            )
            .into_any_element()
    }

    /// The branch review's first file, once its list is there, when the
    /// sheet was opened — the diff brought to the branch's range, whatever
    /// the editor had left it on.
    fn pick_branch_file(&mut self, cx: &mut Context<Self>) {
        if !self.branch_sheet_pick {
            return;
        }
        let Some(worktree) = self.active.clone() else {
            return;
        };
        let Some(state) = self.review.get(&worktree) else {
            return;
        };
        let Some(range) = super::review::branch_panel_range(
            state.base.as_deref(),
            state.review_point.as_ref(),
            state.since_review,
        ) else {
            return;
        };
        let Some(files) = state.files.get(&range) else {
            return;
        };
        self.branch_sheet_pick = false;
        let kept = state.range == range
            && state
                .selected
                .as_deref()
                .is_some_and(|selected| files.iter().any(|file| file.path == selected));
        let first = files.first().map(|file| file.path.clone());
        if let (false, Some(first)) = (kept, first) {
            self.open_file(worktree, first, range, cx);
        }
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
