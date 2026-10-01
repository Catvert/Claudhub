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
//!
//! **The review card** is the editor's branch review laid on the focus
//! board — its Review tab: the same list, the same base selector, the same
//! diff. For the reason above it is live only on the worktree on show, and
//! only while no sheet paints the same panels over it — two copies of one
//! virtual list in a frame would share its scroll.

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
const LIST_WIDTH: gpui_kit::Pixels = px(REVIEW_LIST_WIDTH);
/// The same, as a board lays it out.
pub(super) const REVIEW_LIST_WIDTH: f32 = 380.;
/// The list's height in the review card; the diff takes the rest.
const CARD_LIST_HEIGHT: gpui_kit::Pixels = px(200.);
/// What a maximized sheet leaves of the window round it.
const SHEET_MARGIN: gpui_kit::Pixels = px(16.);

/// A sheet in its dialog: a child entity, like the settings form — the `Fn`
/// `open_dialog` keeps is called back at every frame, and a child entity is
/// what reads the application from there, with state of its own that
/// outlives each call (see "Conventions gpui").
///
/// **It draws its own head** — its title and the buttons of a window: the
/// dialog's title could say none of the rest,
/// and a sheet two columns of code wide is one that wants the whole screen
/// now and then. `maximized` is shared with the dialog's closure, which
/// cannot read the application to size itself.
pub(super) struct ReviewSheet {
    app: WeakEntity<ClaudhubApp>,
    maximized: Rc<Cell<bool>>,
}

impl Render for ReviewSheet {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let Some(app) = self.app.upgrade() else {
            return div().into_any_element();
        };
        let maximized = self.maximized.clone();
        app.update(cx, |app, cx| app.render_review_sheet(maximized, window, cx))
    }
}

/// The changes node's list: the status's files behind an `Rc`, and how
/// many are in the index — built once per status (`ReviewState::rows_changed`
/// drops it), and borrowed by the virtual list at every frame.
pub(crate) struct ChangesList {
    files: Rc<Vec<FileStatus>>,
    staged: usize,
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

/// One file of the changes node: its tick, its code, its name and folder.
/// The tick stages or unstages it where it is; the rest opens the review on
/// it.
fn changes_row(
    app: &gpui_kit::Entity<ClaudhubApp>,
    worktree: &Path,
    index: usize,
    file: &FileStatus,
    cx: &mut gpui_kit::App,
) -> AnyElement {
    let theme = cx.theme().clone();
    // Ticked when all of it is in the index; a press puts the rest in, or
    // takes all of it out — the Changes panel's box.
    let ticked = file.is_staged() && !file.is_unstaged();
    let conflicted = file.is_conflicted();
    let (code, tint) = codes(file);
    let color = super::theme::status_color(tint, cx);
    let name = file.file_name();
    let folder = file.directory();
    let (tick_app, tick_worktree, tick_path) =
        (app.clone(), worktree.to_path_buf(), file.path.clone());
    let (open_app, open_worktree, open_path) =
        (app.clone(), worktree.to_path_buf(), file.path.clone());
    h_flex()
        .id(("overview-changes-row", index))
        .w_full()
        .px_2()
        .h(super::theme::row_height(cx))
        .gap_1p5()
        .items_center()
        .cursor_pointer()
        .hover(|style| style.bg(theme.list_hover))
        .on_click(move |_, window, cx| {
            open_app.update(cx, |this, cx| {
                this.open_review_sheet(&open_worktree, Some(open_path.clone()), window, cx);
            });
        })
        .child(
            div()
                // The tick is its own gesture, not the row's.
                .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                .child(
                    Checkbox::new(("overview-changes-stage", index))
                        .checked(ticked)
                        .disabled(conflicted || !(ticked || file.is_stageable()))
                        .on_click(move |_, _, cx| {
                            cx.stop_propagation();
                            tick_app.update(cx, |this, cx| {
                                this.set_staged(
                                    tick_worktree.clone(),
                                    vec![tick_path.clone()],
                                    !ticked,
                                    cx,
                                );
                            });
                        }),
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

impl ClaudhubApp {
    /// A worktree's changes node: its head, its files, and the way into the
    /// review.
    pub(super) fn render_changes_node(
        &mut self,
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
        let files = self.changes_list(path);
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

        let (body, foot) = self.changes_parts(path, files, cx);

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

    /// The node's list, from its cache — see `ChangesList`.
    fn changes_list(&mut self, path: &Path) -> Option<(Rc<Vec<FileStatus>>, usize)> {
        self.listed_status(path)?;
        let state = self.review.get_mut(path)?;
        let list = state.home_changes.get_or_insert_with(|| ChangesList {
            staged: state.status.files.iter().filter(|f| f.is_staged()).count(),
            files: Rc::new(state.status.files.clone()),
        });
        Some((list.files.clone(), list.staged))
    }

    /// The node's list and its foot — how much is staged, and the way into
    /// the review. **Virtual**: a worktree can have thousands of files in
    /// progress, and the node is painted at every frame an agent works.
    fn changes_parts(
        &self,
        path: &Path,
        files: Option<(Rc<Vec<FileStatus>>, usize)>,
        cx: &mut Context<Self>,
    ) -> (AnyElement, AnyElement) {
        let theme = cx.theme().clone();
        let muted = theme.muted_foreground;
        let staged = files.as_ref().map_or(0, |(_, staged)| *staged);
        let body = match files {
            None => div()
                .flex_1()
                .p_2()
                .text_xs()
                .text_color(muted)
                .child(tr!("overview-changes-reading"))
                .into_any_element(),
            Some((files, _)) => {
                let (entity, worktree) = (cx.entity(), path.to_path_buf());
                v_flex()
                    .flex_1()
                    .min_h_0()
                    .py_1()
                    .child(
                        gpui_kit::uniform_list(
                            SharedString::from(format!("overview-changes-list-{}", path.display())),
                            files.len(),
                            move |range, _, cx| {
                                range
                                    .map(|index| {
                                        changes_row(&entity, &worktree, index, &files[index], cx)
                                    })
                                    .collect()
                            },
                        )
                        .size_full(),
                    )
                    .into_any_element()
            }
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
        (body, foot.into_any_element())
    }

    /// The worktree's status, when it is the latest word on it: asked since
    /// the sweep's count last moved, and answered.
    fn fresh_status(&self, worktree: &Path) -> Option<&crate::git::Status> {
        (self.changes_read.contains(worktree) && !self.status_reads.is_pending(worktree))
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
        (!status.files.is_empty() || !self.status_reads.is_pending(worktree)).then_some(status)
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
        self.show_sheet(window, cx);
        super::dialogs::focus_field(&self.commit_input, window, cx);
        cx.notify();
    }

    /// A worktree's branch review as a thing of its board: a head that
    /// names it and what it compares against — unless its column's title
    /// says it, `head` false — then the list above the diff, side by side
    /// when it fills the middle, `wide`.
    pub(super) fn render_review_card(
        &mut self,
        path: &Path,
        wide: bool,
        head: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let theme = cx.theme().clone();
        let muted = theme.muted_foreground;
        let node = Node::Review(path.to_path_buf());
        let app = cx.entity().downgrade();
        let on_show = self.active.as_deref() == Some(path);
        let sheet_open = self.commit_sheet;
        let against = self
            .review
            .get(path)
            .and_then(|state| state.review_against());

        let head = head.then(|| {
            h_flex()
                .flex_none()
                .h(px(overview::HEAD))
                .px_2()
                .gap_1p5()
                .items_center()
                .bg(theme.info.opacity(0.14))
                .cursor_grab()
                .on_mouse_down(
                    MouseButton::Left,
                    super::overview_view::grab(app.clone(), node.clone()),
                )
                .child(icon("file-diff").text_color(theme.info))
                .child(
                    div()
                        .flex_none()
                        .truncate()
                        .font_weight(gpui_kit::FontWeight::SEMIBOLD)
                        .child(tr!("focus-review")),
                )
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .truncate()
                        .text_xs()
                        .text_color(muted)
                        .children(against),
                )
                .child(self.window_controls(node.clone(), cx))
        });

        let message =
            |text: SharedString| div().text_xs().text_color(muted).text_center().child(text);
        let folded = self.overview_hand.collapsed.contains(&node);
        let body = if folded {
            div().into_any_element()
        } else if on_show && !sheet_open {
            let ready = self.pick_card_file(path, cx);
            let list = self.render_branch_review(window, cx).into_any_element();
            let diff = if ready {
                self.render_diff(window, cx).into_any_element()
            } else {
                v_flex()
                    .size_full()
                    .items_center()
                    .justify_center()
                    .child(message(tr!("review-pick-a-file")))
                    .into_any_element()
            };
            let border = theme.border;
            if wide {
                // The divider between them is the board's to drag.
                let start = v_flex()
                    .size_full()
                    .overflow_hidden()
                    .border_r_1()
                    .border_color(border)
                    .child(list)
                    .into_any_element();
                let end = div().size_full().child(diff).into_any_element();
                div()
                    .flex_1()
                    .min_h_0()
                    .w_full()
                    .child(self.two_sides(path, "review", REVIEW_LIST_WIDTH, start, end, cx))
                    .into_any_element()
            } else {
                v_flex()
                    .flex_1()
                    .min_h_0()
                    .w_full()
                    .child(
                        v_flex()
                            .flex_none()
                            .h(CARD_LIST_HEIGHT)
                            .w_full()
                            .overflow_hidden()
                            .border_b_1()
                            .border_color(border)
                            .child(list),
                    )
                    .child(div().flex_1().min_h_0().w_full().child(diff))
                    .into_any_element()
            }
        } else {
            let worktree = path.to_path_buf();
            v_flex()
                .flex_1()
                .min_h_0()
                .p_3()
                .gap_2()
                .items_center()
                .justify_center()
                .child(message(if sheet_open {
                    tr!("focus-review-sheet")
                } else {
                    tr!("focus-review-idle")
                }))
                .when(!sheet_open, |el| {
                    el.child(
                        Button::new(SharedString::from(format!(
                            "focus-review-show-{}",
                            path.display()
                        )))
                        .small()
                        .icon(icon("eye"))
                        .label(tr!("focus-review-show"))
                        .on_click(cx.listener(
                            move |this, _, window, cx| {
                                this.select_worktree(worktree.clone(), window, cx);
                                cx.notify();
                            },
                        )),
                    )
                })
                .into_any_element()
        };

        v_flex()
            .size_full()
            .overflow_hidden()
            .rounded(theme.radius_lg)
            .border_1()
            .border_color(theme.border)
            .bg(theme.background)
            .text_sm()
            // Its rows are pressed, not the card dragged by them; and a
            // press here neither selects the board nor, twice, leaves the
            // screen for the editor.
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .children(head)
            .child(body)
            .into_any_element()
    }

    /// Brings the diff to the branch's range for the review card — the
    /// file shown kept when it is in the list, else the first one — and
    /// says whether the diff is on that range. The commit sheet leaves it on
    /// the changes in progress, the editor on whatever it was reading.
    fn pick_card_file(&mut self, worktree: &Path, cx: &mut Context<Self>) -> bool {
        let Some(state) = self.review.get(worktree) else {
            return false;
        };
        let Some(range) = state.branch_panel_range() else {
            return false;
        };
        if state.range == range && state.selected.is_some() {
            return true;
        }
        // Its list may still be on its way: the next frame asks again.
        let Some(first) = state
            .files
            .get(&range)
            .and_then(|files| files.first())
            .map(|file| file.path.clone())
        else {
            return false;
        };
        self.open_file(worktree.to_path_buf(), first, range, cx);
        true
    }

    /// A sheet closed from its own head. **`close_dialog` does not call the
    /// dialog's `on_close`** — it only pops it — so what that would have
    /// reset is reset here, or the sheet stays « open » and its button
    /// never opens it again.
    fn close_sheet(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.commit_sheet = false;
        self.review_sheet_pick = false;
        window.close_dialog(cx);
        cx.notify();
    }

    /// A sheet is open only while a dialog is: whichever way one went —
    /// and a path that closes dialogs without their `on_close` is easy to
    /// add — a flag left up must not keep a sheet from opening again.
    fn forget_closed_sheets(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !window.has_active_dialog(cx) {
            self.commit_sheet = false;
        }
    }

    /// The sheet's dialog: its size read off the window at every frame —
    /// two columns of which one is code, and code in six hundred pixels is
    /// read a word per line — the whole window when maximized. No title and
    /// no cross of the dialog's: the sheet draws its own head.
    fn show_sheet(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let app = cx.entity().downgrade();
        let maximized = Rc::new(Cell::new(self.sheet_maximized));
        let sheet = cx.new(|_| ReviewSheet {
            app: app.clone(),
            maximized: maximized.clone(),
        });
        self.commit_sheet = true;
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
                        app.update(cx, |this, _| {
                            this.commit_sheet = false;
                            this.review_sheet_pick = false;
                        });
                    }
                })
        });
    }

    /// A sheet: its head, then its two columns — the list, and the diff.
    fn render_review_sheet(
        &mut self,
        maximized: Rc<Cell<bool>>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        self.pick_review_file(cx);
        let head = self.render_sheet_head(maximized, cx);
        let list = self.render_changes(window, cx).into_any_element();
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

    /// A sheet's head: what it is and whose, and the two buttons of a
    /// window: maximize, which a double click on the head does too, and
    /// close.
    fn render_sheet_head(
        &mut self,
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
        let title = tr!("overview-commit-title", { name: name });
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
            .child(icon("git-commit-horizontal").text_color(theme.muted_foreground))
            .child(
                div()
                    .flex_none()
                    .max_w(gpui_kit::relative(0.6))
                    .truncate()
                    .font_weight(gpui_kit::FontWeight::SEMIBOLD)
                    .child(title),
            )
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
                        this.close_sheet(window, cx);
                    })),
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
