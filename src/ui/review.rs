//! The review: the list of touched files, and the chosen file's diff.
//!
//! Four comparison ranges, chosen by the tabs at the head of the list: the
//! unstaged changes, the index, the whole checkout against HEAD, and the whole
//! branch since it diverged from its base. The last is the one used to review an
//! agent's work before pushing it.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use gpui::{div, prelude::*, px, uniform_list, Context, Focusable, Window};
use gpui_component::{
    button::{Button, ButtonVariants},
    checkbox::Checkbox,
    h_flex,
    input::Textarea,
    select::Select,
    separator::Separator as Divider,
    v_flex, ActiveTheme, Disableable, Sizable, WindowExt,
};

use crate::git::{DiffFile, DiffRange, Status, StatusCode};
use crate::runtime::{Action, Cmd};
use crate::tr;
use crate::ui::app::ClaudhubApp;
use crate::ui::icons::icon;
use crate::ui::theme::{status_color, DiffColors};

/// One row of the changes list.
///
/// The files are grouped as in the clients that hide the index: what is tracked
/// on one side, what is not yet on the other. The group carries its own box,
/// which stages or unstages everything at once.
#[derive(Clone)]
enum Row {
    Group(Group),
    /// A folder of the tree, collapsible.
    Dir(DirRow),
    File(FileRow),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Group {
    /// Files git already tracks.
    Tracked,
    /// Files never added. Ticking them is what starts tracking them.
    Untracked,
}

/// A folder in the changes list.
///
/// Empty intermediate folders are merged with their only child:
/// `app/Http/Livewire/Forms` fits on one line instead of four, and that is what
/// makes the tree readable on a Laravel or Symfony project, where one goes down
/// six levels before finding a file.
#[derive(Clone)]
struct DirRow {
    /// Full path, and collapse key. It is the deepest folder of the merged
    /// chain: collapsing `app/Http` and collapsing `app/Http/Livewire` are two
    /// different gestures, but a merged chain only offers one.
    path: PathBuf,
    /// What is displayed: one segment, or the merged chain.
    label: String,
    depth: usize,
    collapsed: bool,
    /// Every file of the subtree, including those a collapse hides: ticking a
    /// closed folder must stage what it contains, and not what is visible of it.
    paths: Vec<PathBuf>,
    /// True when the whole subtree is already staged.
    staged: bool,
    /// True when the whole subtree has already been reviewed — that is what
    /// makes a click on a folder a review of everything it contains.
    reviewed: bool,
    added: usize,
    removed: usize,
}

#[derive(Clone)]
struct FileRow {
    path: PathBuf,
    /// Depth in the tree. Zero in the flat list.
    depth: usize,
    name: String,
    directory: String,
    /// git's two codes, the index's then the working tree's: it is the exact
    /// information, and it fits in two characters where a single checkbox would
    /// have to lie about partially staged files.
    index: StatusCode,
    worktree: StatusCode,
    added: usize,
    removed: usize,
    /// This file will go into the next commit, at least in part.
    staged: bool,
    untracked: bool,
    /// It has been marked reviewed, and has not changed since.
    reviewed: bool,
}

impl FileRow {
    /// Only part of the file is staged: what git writes `MM`.
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

impl ClaudhubApp {
    /// The list of changes in progress, and what is needed to commit them.
    pub(super) fn render_changes(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        self.render_file_list(DiffRange::Working, window, cx)
    }

    /// The branch review: what the branch has written since its base.
    ///
    /// While the base is unknown — a repository with no integration branch, or
    /// the branch checked out here which would have nothing to compare itself
    /// against — the panel says so rather than show an empty list one would take
    /// for wrong.
    pub(super) fn render_branch_review(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        match self.active_review().and_then(|state| state.base.clone()) {
            Some(base) => self
                .render_file_list(DiffRange::Branch { base }, window, cx)
                .into_any_element(),
            None => v_flex()
                .size_full()
                .child(self.render_base_bar(cx))
                .child(
                    div()
                        .p_3()
                        .text_xs()
                        .text_color(cx.theme().muted_foreground)
                        .child(tr!("range-branch-none")),
                )
                .into_any_element(),
        }
    }

    pub(super) fn render_file_list(
        &mut self,
        range: DiffRange,
        window: &mut Window,
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

        // Two panels show this list at the same time: each has its own search,
        // otherwise filtering the changes would also filter the branch review.
        let pane = if matches!(range, DiffRange::Working) {
            crate::ui::find::Pane::Changes
        } else {
            crate::ui::find::Pane::Branch
        };
        let find = self.render_find(pane, cx);
        let query = self.query(pane, cx);
        // It is the panel that asks for its list: it alone knows what it shows,
        // and loading both ranges in advance would cost a command for a tab
        // nobody will open.
        self.ensure_files(range.clone(), cx);
        // Taken before any borrow of the state: it is the view that holds it,
        // and the list only hangs off it.
        let scroll = self.file_scroll(&range);
        let Some(state) = self.review.get(&worktree) else {
            return div().into_any_element();
        };
        let selected = state.selected.clone();
        let collapsed = state.collapsed.clone();
        // The flat list stays the reference: it is what counts what is staged
        // and what gives a group's box the files to act on, including those a
        // collapsed folder hides.
        // The filter applies to the flat list, before the tree: it is the
        // reference, and a folder with nothing left in it has to disappear with
        // its files.
        let flat: Vec<Row> = self
            .rows(&range, cx)
            .into_iter()
            .filter(|row| match row {
                Row::File(file) => crate::ui::find::matches(&query, &file.path.to_string_lossy()),
                _ => true,
            })
            .collect();
        let staged_count = flat
            .iter()
            .filter(|row| matches!(row, Row::File(file) if file.staged))
            .count();
        // The two lists are behind an `Rc`: without the tree, the displayed list
        // **is** the flat one, and copying several hundred rows on every frame
        // to say so would be the cost of the whole panel.
        let flat = std::rc::Rc::new(flat);
        // During a search, collapses are ignored: a file found in a closed
        // folder would not be visible, and the search would look as if it had
        // found nothing.
        let rows = if crate::ui::settings::Settings::global(cx).review_tree {
            let collapsed = if query.trim().is_empty() {
                collapsed
            } else {
                HashSet::new()
            };
            std::rc::Rc::new(tree_rows(&flat, &collapsed))
        } else {
            flat.clone()
        };
        let can_commit = staged_count > 0;
        let commits = matches!(range, DiffRange::Working);
        // Two lists live side by side: they cannot carry the same id, otherwise
        // they would share their scrolling.
        let list_id = match &range {
            DiffRange::Working => "working".to_string(),
            DiffRange::Branch { base } => format!("branch-{base}"),
            DiffRange::Commit { id, .. } => format!("commit-{id}"),
        };

        // No right-hand rule: it was the seam with the neighbouring diff, from
        // the days when the panels touched — the gutter separates them now.
        v_flex()
            .size_full()
            .when(!matches!(range, DiffRange::Working), |el| {
                el.child(self.render_base_bar(cx))
            })
            .when(matches!(range, DiffRange::Working), |el| {
                el.child(self.render_changes_bar(cx))
            })
            .children(find)
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
                    // Virtualised list: a branch review routinely touches
                    // several hundred files, and rebuilding that many rows —
                    // each with its two buttons — on every frame is enough to
                    // bring the interface down to a few frames per second.
                    .when(!rows.is_empty(), |el| {
                        let entity = cx.entity();
                        let colors = DiffColors::of(cx);
                        let count = rows.len();
                        // Only the changes in progress can be ticked: on a
                        // commit already written, there is nothing to stage.
                        let checkable = matches!(range, DiffRange::Working);
                        let row_range = range.clone();
                        el.child(
                            self.scrolled(
                                gpui::SharedString::from(format!("file-bar-{}", list_id)),
                                &scroll,
                                crate::ui::motion::Axes::Vertical,
                                window,
                                uniform_list(
                                    gpui::SharedString::from(format!("file-list-{}", list_id)),
                                    count,
                                    move |visible, _window, cx| {
                                        visible
                                            .map(|ix| {
                                                render_row(
                                                    &rows,
                                                    &flat,
                                                    ix,
                                                    &worktree,
                                                    &row_range,
                                                    selected.as_deref(),
                                                    &colors,
                                                    checkable,
                                                    &entity,
                                                    cx,
                                                )
                                            })
                                            .collect::<Vec<_>>()
                                    },
                                )
                                .size_full()
                                // The rows' inset is here and not on them:
                                // `uniform_list` lays its entries out at the
                                // size it computes, and a margin on an entry is
                                // ignored. It is that inset that lets the
                                // rounded backgrounds breathe instead of
                                // crossing the panel from edge to edge.
                                .px_1()
                                .track_scroll(&scroll.clone()),
                                cx,
                            ),
                        )
                    }),
            )
            .when(commits, |el| {
                el.child(self.render_commit_box(can_commit, staged_count, cx))
            })
            .into_any_element()
    }

    /// The rows of a range's list.
    ///
    /// The status is the source for the changes in progress — it alone tells the
    /// index from the working tree — and `--numstat` for the ranges that are
    /// about commits and have no notion of an index.
    fn rows(&self, range: &DiffRange, _cx: &Context<Self>) -> Vec<Row> {
        let Some(state) = self.active_review() else {
            return Vec::new();
        };
        let files = state.files.get(range).map(Vec::as_slice).unwrap_or(&[]);
        rows_for(range, &state.status, files, &state.reviewed)
    }

    /// The list's files, in the order they are displayed.
    ///
    /// The displayed order and not the raw one: a collapsed folder hides its
    /// files, and the arrows must not open a file the list does not show — the
    /// next one would then be impossible to find by eye.
    fn visible_files(&self, range: &DiffRange, cx: &Context<Self>) -> Vec<PathBuf> {
        self.visible_rows(range, cx)
            .into_iter()
            .filter_map(|row| match row {
                Row::File(file) => Some(file.path),
                _ => None,
            })
            .collect()
    }

    /// The entries as the list shows them, folders included.
    fn visible_rows(&self, range: &DiffRange, cx: &Context<Self>) -> Vec<Row> {
        let Some(state) = self.active_review() else {
            return Vec::new();
        };
        let flat = self.rows(range, cx);
        if crate::ui::settings::Settings::global(cx).review_tree {
            tree_rows(&flat, &state.collapsed)
        } else {
            flat
        }
    }

    /// Brings the list onto a file.
    ///
    /// The index is that of the **displayed** list — folders included, and
    /// without what the collapses hide: it is that list the view virtualises,
    /// and an index taken elsewhere would name another row.
    pub(super) fn reveal_file(&mut self, range: &DiffRange, path: &Path, cx: &mut Context<Self>) {
        let Some(index) = self
            .visible_rows(range, cx)
            .iter()
            .position(|row| matches!(row, Row::File(file) if file.path == path))
        else {
            return;
        };
        self.file_scroll(range)
            .scroll_to_item(index, gpui::ScrollStrategy::Center);
    }

    /// Opens the previous or next file in the list.
    ///
    /// At the ends, nothing happens: wrapping would restart a review just
    /// finished with nothing to say so.
    pub(super) fn step_file(&mut self, delta: isize, cx: &mut Context<Self>) {
        let Some(worktree) = self.active.clone() else {
            return;
        };
        let Some(range) = self.review.get(&worktree).map(|state| state.range.clone()) else {
            return;
        };
        let files = self.visible_files(&range, cx);
        let Some(state) = self.review.get(&worktree) else {
            return;
        };
        let current = state
            .selected
            .as_ref()
            .and_then(|path| files.iter().position(|file| file == path));
        let Some(index) = step_index(current, delta, files.len()) else {
            return;
        };
        let Some(path) = files.get(index).cloned() else {
            return;
        };
        self.open_file(worktree, path, range, cx);
    }

    /// The changes panel's bar: what one does to the repository, then the
    /// display toggle.
    ///
    /// `fetch`, `pull` and `push` live here and not in the window's toolbar:
    /// they are that panel's gestures — you look at what changed, you tick, you
    /// commit, you push — and keeping them at the other end of the screen made
    /// you cross the window to finish a sentence started at the bottom.
    fn render_changes_bar(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let has_active = self.active.is_some();
        // A fetch, a pull and a push talk to a remote: seconds, sometimes tens
        // of them, during which nothing on screen moved. The button turns while
        // its own operation runs — its own, and on **this** worktree: an agent
        // pushing next door is not this button's business.
        let fetching = self.active_running(Action::Fetch);
        let pulling = self.active_running(Action::Pull);
        let pushing = self.active_running(Action::Push);
        // The ahead and behind counts on the upstream, as the status reports
        // them. They are **on the buttons** and not only in the status bar: they
        // are what says which of the two gestures there is to make, and a
        // disabled button says there is nothing to do — which is half the
        // information one is after on arriving at a worktree.
        let (ahead, behind) = self
            .active_review()
            .map(|state| (state.status.ahead, state.status.behind))
            .unwrap_or((0, 0));
        self.bar(cx)
            .child(
                Button::new("fetch")
                    .ghost()
                    .xsmall()
                    .icon(icon("refresh-cw"))
                    .tooltip(tr!("action-fetch"))
                    .loading(fetching)
                    .disabled(!has_active || fetching)
                    .on_click(cx.listener(|this, _, _, cx| {
                        if let Some(worktree) = this.active.clone() {
                            let cmd = Cmd::Fetch {
                                worktree: worktree.clone(),
                            };
                            this.start(Some(worktree), Action::Fetch, cmd, cx);
                        }
                    })),
            )
            .child(
                Button::new("pull")
                    .ghost()
                    .xsmall()
                    .icon(icon("arrow-down-to-line"))
                    .tooltip(if behind > 0 {
                        tr!("action-pull-behind", { count: behind })
                    } else {
                        tr!("action-pull")
                    })
                    .when(behind > 0, |el| el.primary().label(behind.to_string()))
                    .loading(pulling)
                    .disabled(!has_active || pulling)
                    .on_click(cx.listener(|this, _, _, cx| {
                        if let Some(worktree) = this.active.clone() {
                            let cmd = Cmd::Pull {
                                worktree: worktree.clone(),
                            };
                            this.start(Some(worktree), Action::Pull, cmd, cx);
                        }
                    })),
            )
            .child(
                Button::new("push")
                    .ghost()
                    .xsmall()
                    .icon(icon("arrow-up-from-line"))
                    .tooltip(if ahead > 0 {
                        tr!("action-push-ahead", { count: ahead })
                    } else {
                        tr!("action-push")
                    })
                    .when(ahead > 0, |el| el.primary().label(ahead.to_string()))
                    .loading(pushing)
                    .disabled(!has_active || pushing)
                    .on_click(cx.listener(|this, _, _, cx| {
                        if let Some(worktree) = this.active.clone() {
                            let cmd = Cmd::Push {
                                worktree: worktree.clone(),
                                force_with_lease: false,
                            };
                            this.start(Some(worktree), Action::Push, cmd, cx);
                        }
                    })),
            )
            .child(Divider::vertical().h(px(12.)))
            .child(self.tree_toggle(cx))
    }

    /// The branch review's bar: the toggle, and the choice of base.
    ///
    /// The integration branch git guesses is a starting point, not a fate — one
    /// compares just as well against `dev`, against another working branch or
    /// against a remote.
    fn render_base_bar(&self, cx: &mut Context<Self>) -> impl IntoElement {
        self.bar(cx).child(self.tree_toggle(cx)).child(
            Select::new(&self.base_select)
                .xsmall()
                .title_prefix(tr!("range-base-prefix"))
                .placeholder(tr!("range-base-placeholder"))
                .menu_width(crate::ui::base_select::MENU_WIDTH),
        )
    }

    fn bar(&self, cx: &mut Context<Self>) -> gpui::Div {
        h_flex()
            .h(crate::ui::theme::bar_height(cx))
            .w_full()
            .px_1()
            .gap_1()
            .items_center()
            .justify_end()
            .border_b_1()
            .border_color(cx.theme().border)
    }

    fn tree_toggle(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let tree = crate::ui::settings::Settings::global(cx).review_tree;
        Button::new("review-tree")
            .ghost()
            .xsmall()
            .icon(icon(if tree { "list-tree" } else { "list" }))
            .tooltip(if tree {
                tr!("review-as-list")
            } else {
                tr!("review-as-tree")
            })
            .on_click(cx.listener(|this, _, _, cx| this.toggle_review_tree(cx)))
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
            .child(Textarea::new(&self.commit_input).h(px(64.)))
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
                    .children(self.suggest_button(can_commit, cx))
                    .child({
                        // A commit runs the repository's hooks — a linter, a
                        // test suite — and those take as long as they take.
                        let committing = self.active_running(Action::Commit);
                        Button::new("commit")
                            .primary()
                            .xsmall()
                            .icon(icon("git-commit-horizontal"))
                            .label(tr!("action-commit"))
                            .loading(committing)
                            .disabled(!can_commit || committing)
                            .on_click(cx.listener(|this, _, _, cx| this.commit(false, cx)))
                    }),
            )
    }

    /// The button that asks the agent for a message.
    ///
    /// It does not exist when the setting is empty: offering a gesture that will
    /// fail for want of a command is worth less than offering nothing. It spins
    /// while waiting — the agent takes ten to thirty seconds, and a button that
    /// says nothing for that long gets clicked three times.
    fn suggest_button(&self, can_commit: bool, cx: &mut Context<Self>) -> Option<impl IntoElement> {
        let command = crate::ui::settings::Settings::global(cx)
            .commit_message_command
            .clone();
        if command.trim().is_empty() {
            return None;
        }
        let waiting = self.suggesting_message.is_some();
        Some(
            Button::new("commit-suggest")
                .ghost()
                .xsmall()
                .icon(icon(if waiting { "loader-circle" } else { "sparkles" }))
                .tooltip(tr!("commit-suggest"))
                .disabled(!can_commit || waiting)
                .on_click(cx.listener(|this, _, _, cx| this.suggest_commit_message(cx))),
        )
    }

    /// Asks the agent for a message for what is staged.
    ///
    /// The command goes into a worker like everything else: `claude -p` takes
    /// ten to thirty seconds, and waiting for it from a click handler would
    /// freeze the window for the whole time.
    pub(super) fn suggest_commit_message(&mut self, cx: &mut Context<Self>) {
        let Some(worktree) = self.active.clone() else {
            return;
        };
        if self.suggesting_message.is_some() {
            return;
        }
        let command = crate::ui::settings::Settings::global(cx)
            .commit_message_command
            .clone();
        self.suggesting_message = Some(worktree.clone());
        self.git.send(Cmd::SuggestMessage { worktree, command });
        self.announce(tr!("commit-suggest-running"), cx);
        cx.notify();
    }

    /// Switches between the tree and the flat list.
    ///
    /// The choice is global and persistent: it is a reading habit, not a
    /// decision taken again per worktree.
    pub(super) fn toggle_review_tree(&mut self, cx: &mut Context<Self>) {
        crate::ui::settings::Settings::update_global(cx, |s| s.review_tree = !s.review_tree);
        cx.notify();
    }

    /// Collapses or unfolds a folder of the list.
    pub(super) fn toggle_directory(&mut self, path: PathBuf, cx: &mut Context<Self>) {
        let Some(state) = self.active_review_mut() else {
            return;
        };
        if !state.collapsed.remove(&path) {
            state.collapsed.insert(path);
        }
        if let Some(worktree) = self.active_path() {
            self.persist_review(&worktree, cx);
        }
        cx.notify();
    }

    /// Ticks or unticks files, that is, stages them or takes them out of the
    /// index. It is the only staging gesture the interface offers: the box
    /// replaces the two lists git distinguishes.
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

    /// Commits what is in the index. `amend` reuses the previous commit.
    pub(super) fn commit(&mut self, amend: bool, cx: &mut Context<Self>) {
        let Some(worktree) = self.active.clone() else {
            return;
        };
        let message = self.commit_input.read(cx).value().to_string();
        if message.trim().is_empty() && !amend {
            return;
        }
        let cmd = Cmd::Commit {
            worktree: worktree.clone(),
            message,
            amend,
            all: false,
        };
        self.start(Some(worktree), Action::Commit, cmd, cx);
    }
}

impl ClaudhubApp {
    /// Asks for confirmation before discarding changes.
    ///
    /// The only Claudhub action that destroys work without git keeping a copy:
    /// neither `reflog` nor `stash` catches a `restore --worktree`. Hence the
    /// dialog, even though all the rest of the interface acts on a click.
    fn confirm_removal(
        &mut self,
        worktree: PathBuf,
        path: PathBuf,
        untracked: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let label = path.display().to_string();
        let entity = cx.entity();
        let (title, warning) = if untracked {
            (tr!("delete-title"), tr!("delete-warning"))
        } else {
            (tr!("discard-title"), tr!("discard-warning"))
        };
        window.open_dialog(cx, move |dialog, _window, _cx| {
            let (worktree, path, entity) = (worktree.clone(), path.clone(), entity.clone());
            dialog
                .title(title.clone())
                .child(
                    v_flex()
                        .gap_1()
                        .child(div().text_sm().child(label.clone()))
                        .child(div().text_xs().child(warning.clone())),
                )
                .overlay_closable(false)
                .close_button(false)
                .footer(super::dialogs::confirm())
                .on_ok(move |_, _window, cx| {
                    entity.update(cx, |this, cx| {
                        let paths = vec![path.clone()];
                        let worktree = worktree.clone();
                        this.git.send(if untracked {
                            Cmd::Delete { worktree, paths }
                        } else {
                            Cmd::Discard { worktree, paths }
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

/// Renders one row of the list: a group heading or a file.
///
/// A free function because a virtualised list's closure does not receive the
/// view: it captures the entity and goes back through `update` to act, as the
/// dialog handlers do.
#[allow(clippy::too_many_arguments)]
fn render_row(
    rows: &std::rc::Rc<Vec<Row>>,
    flat: &std::rc::Rc<Vec<Row>>,
    index: usize,
    worktree: &Path,
    range: &DiffRange,
    selected: Option<&Path>,
    colors: &DiffColors,
    checkable: bool,
    entity: &gpui::Entity<ClaudhubApp>,
    cx: &mut gpui::App,
) -> gpui::AnyElement {
    match rows.get(index) {
        Some(Row::Group(group)) => render_group(flat, index, *group, worktree, entity, cx),
        Some(Row::Dir(dir)) => {
            render_dir(dir, index, worktree, range, colors, checkable, entity, cx)
        }
        Some(Row::File(file)) => render_file(
            file, index, worktree, range, selected, colors, checkable, entity, cx,
        ),
        None => div().into_any_element(),
    }
}

/// One tree level's offset.
///
/// Proportional to the text, like the heights: a fixed indentation disappears
/// when the font grows, and the tree becomes a flat list again.
fn indent(depth: usize, cx: &gpui::App) -> gpui::Pixels {
    px(8.) + crate::ui::theme::row_height(cx) * 0.5 * depth as f32
}

/// A folder: the chevron, the box that stages everything it contains, and the
/// total of its lines.
#[allow(clippy::too_many_arguments)]
fn render_dir(
    row: &DirRow,
    index: usize,
    worktree: &Path,
    range: &DiffRange,
    colors: &DiffColors,
    checkable: bool,
    entity: &gpui::Entity<ClaudhubApp>,
    cx: &mut gpui::App,
) -> gpui::AnyElement {
    let staged = row.staged;
    let count = row.paths.len();

    h_flex()
        .id(("dir", index))
        .h(crate::ui::theme::row_height(cx))
        .rounded(cx.theme().radius)
        .pl(indent(row.depth, cx))
        .pr_2()
        .gap_1()
        .items_center()
        .cursor_pointer()
        .whitespace_nowrap()
        .overflow_hidden()
        // A green folder is a fully reviewed folder: that is what its tick
        // promises in one click.
        .when(row.reviewed, |el| el.bg(cx.theme().success.opacity(0.12)))
        .hover(|s| s.bg(cx.theme().accent.opacity(0.4)))
        .on_click({
            let (entity, path) = (entity.clone(), row.path.clone());
            move |_, _window, cx| {
                entity.update(cx, |this, cx| this.toggle_directory(path.clone(), cx));
            }
        })
        .child(
            icon(if row.collapsed {
                "chevron-right"
            } else {
                "chevron-down"
            })
            .xsmall(),
        )
        // A folder's box acts on its whole subtree, collapsed part included:
        // ticking a closed folder must stage what it contains, and not what is
        // visible of it.
        .when(checkable, |el| {
            let (entity, worktree, paths) =
                (entity.clone(), worktree.to_path_buf(), row.paths.clone());
            el.child(
                Checkbox::new(("stage-dir", index))
                    .checked(staged)
                    .on_click(move |_, _window, cx| {
                        entity.update(cx, |this, cx| {
                            this.set_staged(worktree.clone(), paths.clone(), !staged, cx)
                        });
                    }),
            )
        })
        .child(
            icon(if row.collapsed {
                "folder-closed"
            } else {
                "folder-open"
            })
            .xsmall()
            .text_color(cx.theme().muted_foreground),
        )
        .child(
            div()
                .flex_1()
                .min_w_0()
                .truncate()
                .text_sm()
                .child(row.label.clone()),
        )
        .child(
            div()
                .flex_none()
                .text_xs()
                .text_color(cx.theme().muted_foreground)
                .child(count.to_string()),
        )
        .children(crate::ui::theme::volume(row.added, row.removed, colors))
        .child(render_reviewed(
            ("reviewed-dir", index),
            row.reviewed,
            worktree,
            range,
            row.paths.clone(),
            entity,
            cx,
        ))
        .into_any_element()
}

fn render_group(
    rows: &std::rc::Rc<Vec<Row>>,
    index: usize,
    group: Group,
    worktree: &Path,
    entity: &gpui::Entity<ClaudhubApp>,
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
        .h(crate::ui::theme::row_height(cx))
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
    range: &DiffRange,
    selected: Option<&Path>,
    colors: &DiffColors,
    checkable: bool,
    entity: &gpui::Entity<ClaudhubApp>,
    cx: &mut gpui::App,
) -> gpui::AnyElement {
    let is_selected = selected == Some(row.path.as_path());
    let staged = row.staged;

    h_flex()
        .id(("file", index))
        .h(crate::ui::theme::row_height(cx))
        .rounded(cx.theme().radius)
        .pl(indent(row.depth, cx))
        .pr_2()
        .gap_2()
        .items_center()
        .cursor_pointer()
        .whitespace_nowrap()
        .overflow_hidden()
        // Reviewed: a green background, which shows at a glance where the tick
        // alone on the right would mean scanning a column. The selection comes
        // in front — it is where you are, and losing sight of that is worse than
        // forgetting an already-read row.
        .when(row.reviewed && !is_selected, |el| {
            el.bg(cx.theme().success.opacity(0.12))
        })
        .when(is_selected, |el| el.bg(cx.theme().accent))
        .hover(|s| s.bg(cx.theme().accent.opacity(0.5)))
        .on_click({
            let (entity, worktree, path, range) = (
                entity.clone(),
                worktree.to_path_buf(),
                row.path.clone(),
                range.clone(),
            );
            move |_, window, cx| {
                // The focus goes back to the view: without that, the arrows
                // would go on browsing the explorer's tree if that is where one
                // came from, and reviewing the file just opened with the
                // keyboard would be inert.
                let handle = entity.read(cx).focus_handle(cx);
                window.focus(&handle, cx);
                entity.update(cx, |this, cx| {
                    this.open_file(worktree.clone(), path.clone(), range.clone(), cx)
                });
            }
        })
        // Ticking is staging. The ranges that are about commits already written
        // have nothing to tick: a box there would be a button that lies.
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
                .font_family(cx.theme().mono_font_family.clone())
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
        // The icon says the family by its shape and the language by its tint:
        // that is what makes a list of two hundred files scannable, where git's
        // codes only say what changed.
        .child(crate::ui::file_icons::file_icon(&row.path, cx))
        .child(
            h_flex()
                .flex_1()
                .min_w_0()
                .gap_1()
                .items_baseline()
                // A reviewed file dims: that is what makes the list say at a
                // glance what is left to read, where the tick alone on the right
                // would mean scanning a column.
                .child(
                    div()
                        .truncate()
                        .text_sm()
                        .when(row.reviewed, |el| {
                            el.text_color(cx.theme().muted_foreground)
                        })
                        .child(row.name.clone()),
                )
                .child(
                    div()
                        .truncate()
                        .text_xs()
                        .text_color(cx.theme().muted_foreground)
                        .child(row.directory.clone()),
                ),
        )
        // A file only part of which is staged: the ticked box is not enough to
        // say so, and it is precisely the case where one thinks a whole file is
        // being committed when only half of it is.
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
        .children(crate::ui::theme::volume(row.added, row.removed, colors))
        // A tracked file goes back to its original state; a new file has none —
        // it is deleted, which is not the same gesture and therefore carries
        // neither the same icon nor the same warning.
        .when(checkable, |el| {
            let (entity, worktree, path) =
                (entity.clone(), worktree.to_path_buf(), row.path.clone());
            let untracked = row.untracked;
            el.child(
                Button::new(("discard", index))
                    .ghost()
                    .xsmall()
                    .icon(icon(if untracked { "trash-2" } else { "undo-2" }))
                    .tooltip(if untracked {
                        tr!("action-delete")
                    } else {
                        tr!("action-discard")
                    })
                    .on_click(move |_, window, cx| {
                        entity.update(cx, |this, cx| {
                            this.confirm_removal(
                                worktree.clone(),
                                path.clone(),
                                untracked,
                                window,
                                cx,
                            )
                        });
                    }),
            )
        })
        // A file is marked reviewed on its own, as a folder is marked whole: it
        // is the basic gesture, the folder being only its shortcut.
        .child(render_reviewed(
            ("reviewed", index),
            row.reviewed,
            worktree,
            range,
            vec![row.path.clone()],
            entity,
            cx,
        ))
        .into_any_element()
}

/// The button that marks a file — or a whole folder — reviewed.
///
/// A tick and not a box: this list's checkbox already means "stage", and two
/// boxes side by side for two unrelated gestures would be confused at first
/// glance. It lives on the right, after the volume, where nothing competes with
/// reading the name.
///
/// On a folder it carries the whole subtree, collapsed part included — like the
/// staging box, and for the same reason: it is the gesture worth the detour, a
/// branch review having more folders reviewed in one go than files reviewed one
/// by one.
fn render_reviewed(
    id: (&'static str, usize),
    reviewed: bool,
    worktree: &Path,
    range: &DiffRange,
    paths: Vec<PathBuf>,
    entity: &gpui::Entity<ClaudhubApp>,
    cx: &gpui::App,
) -> Button {
    let (entity, worktree, range) = (entity.clone(), worktree.to_path_buf(), range.clone());
    Button::new(id)
        .ghost()
        .xsmall()
        .icon(
            icon(if reviewed { "check-check" } else { "check" }).text_color(if reviewed {
                cx.theme().success
            } else {
                cx.theme().muted_foreground.opacity(0.5)
            }),
        )
        .tooltip(if reviewed {
            tr!("action-unreview")
        } else {
            tr!("action-review")
        })
        .on_click(move |_, _window, cx| {
            entity.update(cx, |this, cx| {
                this.set_reviewed(
                    worktree.clone(),
                    range.clone(),
                    paths.clone(),
                    !reviewed,
                    cx,
                )
            });
        })
}

/// The list's rows for a given review range.
///
/// The neighbouring file, or nothing at the ends.
///
/// With no file open, the first arrow takes the end it points at.
fn step_index(current: Option<usize>, delta: isize, len: usize) -> Option<usize> {
    if len == 0 {
        return None;
    }
    let next = match current {
        Some(index) => index as isize + delta,
        None if delta > 0 => 0,
        None => len as isize - 1,
    };
    (next >= 0 && next < len as isize).then_some(next as usize)
}

/// A free function because it is this view's only real decision — which file
/// appears, in which group, ticked or not — and because it can be tested without
/// a window.
///
/// The status is the source for the changes in progress: it alone tells what is
/// staged from what is not, a distinction the checkbox renders. The other ranges
/// are about commits, which have no notion of an index, and come from
/// `--numstat`.
fn rows_for(
    range: &DiffRange,
    status: &Status,
    files: &[DiffFile],
    reviewed: &[crate::ui::vault::Reviewed],
) -> Vec<Row> {
    let volumes: std::collections::HashMap<&PathBuf, (usize, usize)> = files
        .iter()
        .map(|f| (&f.path, (f.added, f.removed)))
        .collect();
    let volume = |path: &PathBuf| volumes.get(path).copied().unwrap_or((0, 0));
    // Reviewed **and** unchanged since: the recorded volume is what expires the
    // tick, otherwise it would say "reviewed" of a file an agent has just
    // rewritten.
    let is_reviewed = |path: &PathBuf, added: usize, removed: usize| {
        reviewed.iter().any(|item| {
            item.range == *range
                && item.path == *path
                && item.added == added
                && item.removed == removed
        })
    };

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
                    depth: 0,
                    name: file.file_name(),
                    directory: file.directory(),
                    index: file.index,
                    worktree: file.worktree,
                    added,
                    removed,
                    staged: file.is_staged(),
                    untracked: file.is_untracked(),
                    reviewed: is_reviewed(&file.path, added, removed),
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
                    depth: 0,
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
                    // A commit is already written: nothing to tick.
                    staged: true,
                    untracked: false,
                    reviewed: is_reviewed(&f.path, f.added, f.removed),
                })
            })
            .collect(),
    }
}

/// A group's files, for the boxes that act on the whole batch.
fn group_paths(rows: &[Row], group: Group) -> Vec<PathBuf> {
    let mut inside = false;
    let mut paths = Vec::new();
    for row in rows {
        match row {
            Row::Group(g) => inside = *g == group,
            Row::File(file) if inside => paths.push(file.path.clone()),
            Row::File(_) | Row::Dir(_) => {}
        }
    }
    paths
}

/// True if the whole group is already staged.
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
            Row::File(_) | Row::Dir(_) => {}
        }
    }
    any
}

/// Turns the flat list into a tree.
///
/// The groups are kept as they are; each block of files between two groups
/// becomes a tree. The construction is delegated to `ui::tree`, which knows only
/// paths — what a row shows is decided here, and that is what lets the project
/// explorer use the same tree with other leaves.
fn tree_rows(flat: &[Row], collapsed: &HashSet<PathBuf>) -> Vec<Row> {
    let mut out = Vec::new();
    let mut block: Vec<FileRow> = Vec::new();
    for row in flat {
        match row {
            Row::Group(group) => {
                flush(&mut block, collapsed, &mut out);
                out.push(Row::Group(*group));
            }
            Row::File(file) => block.push(file.clone()),
            // A list already in tree form is not turned into a tree again.
            Row::Dir(_) => {}
        }
    }
    flush(&mut block, collapsed, &mut out);
    out
}

fn flush(block: &mut Vec<FileRow>, collapsed: &HashSet<PathBuf>, out: &mut Vec<Row>) {
    if block.is_empty() {
        return;
    }
    let files: Vec<FileRow> = std::mem::take(block);
    let paths: Vec<PathBuf> = files.iter().map(|file| file.path.clone()).collect();
    // Open unless named: a review is a few dozen files, and is read wide open.
    for entry in crate::ui::tree::build(&paths, crate::ui::tree::Folds::OpenBut(collapsed)) {
        match entry {
            crate::ui::tree::Entry::Dir {
                path,
                label,
                depth,
                collapsed,
                leaves,
            } => {
                // A folder's aggregates cover its **whole** subtree, collapsed
                // part included: it is what the checkbox stages, and what the
                // volume announces.
                let inside: Vec<&FileRow> = leaves.iter().map(|index| &files[*index]).collect();
                out.push(Row::Dir(DirRow {
                    path,
                    label,
                    depth,
                    collapsed,
                    paths: inside.iter().map(|file| file.path.clone()).collect(),
                    staged: inside.iter().all(|file| file.staged),
                    reviewed: inside.iter().all(|file| file.reviewed),
                    added: inside.iter().map(|file| file.added).sum(),
                    removed: inside.iter().map(|file| file.removed).sum(),
                }));
            }
            crate::ui::tree::Entry::Leaf { index, depth } => {
                let mut file = files[index].clone();
                file.depth = depth;
                // The folder is carried by the row above: repeating it on every
                // file is exactly the noise the tree removes.
                file.directory.clear();
                out.push(Row::File(file));
            }
        }
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

    fn files_of(rows: &[Row]) -> Vec<&FileRow> {
        rows.iter()
            .filter_map(|row| match row {
                Row::File(file) => Some(file),
                Row::Group(_) | Row::Dir(_) => None,
            })
            .collect()
    }

    fn groups_of(rows: &[Row]) -> Vec<Group> {
        rows.iter()
            .filter_map(|row| match row {
                Row::Group(group) => Some(*group),
                Row::File(_) | Row::Dir(_) => None,
            })
            .collect()
    }

    fn tree(paths: &[&str], collapsed: &[&str]) -> Vec<Row> {
        let flat: Vec<Row> = paths
            .iter()
            .map(|p| {
                Row::File(FileRow {
                    path: PathBuf::from(p),
                    depth: 0,
                    name: Path::new(p)
                        .file_name()
                        .unwrap()
                        .to_string_lossy()
                        .into_owned(),
                    directory: String::new(),
                    index: StatusCode::Modified,
                    worktree: StatusCode::Unmodified,
                    added: 1,
                    removed: 0,
                    staged: true,
                    untracked: false,
                    reviewed: false,
                })
            })
            .collect();
        let collapsed: HashSet<PathBuf> = collapsed.iter().map(PathBuf::from).collect();
        tree_rows(&flat, &collapsed)
    }

    fn shape(rows: &[Row]) -> Vec<String> {
        rows.iter()
            .map(|row| match row {
                Row::Group(_) => "groupe".to_string(),
                Row::Dir(dir) => format!(
                    "{}[{}] {}",
                    " ".repeat(dir.depth),
                    dir.paths.len(),
                    dir.label
                ),
                Row::File(file) => format!("{}{}", " ".repeat(file.depth), file.name),
            })
            .collect()
    }

    #[test]
    fn lonely_directories_are_merged_into_one_line() {
        // The point of the tree on a Laravel project: without merging,
        // `app/Http/Livewire/Forms` costs four rows and four levels of
        // indentation for one file.
        let rows = tree(&["app/Http/Livewire/Forms/BillForm.php"], &[]);
        assert_eq!(
            shape(&rows),
            vec!["[1] app/Http/Livewire/Forms", " BillForm.php"]
        );
    }

    #[test]
    fn a_directory_splits_where_its_contents_split() {
        let rows = tree(
            &["src/ui/app.rs", "src/ui/review.rs", "src/git/diff.rs"],
            &[],
        );
        assert_eq!(
            shape(&rows),
            vec![
                "[3] src",
                " [1] git",
                "  diff.rs",
                " [2] ui",
                "  app.rs",
                "  review.rs",
            ]
        );
    }

    #[test]
    fn a_collapsed_directory_hides_its_files_but_still_counts_them() {
        // This count is not cosmetic: it is the list the folder's box acts on,
        // and ticking a closed folder must stage what it contains, not what is
        // visible of it.
        let rows = tree(&["src/ui/app.rs", "src/ui/review.rs"], &["src/ui"]);
        assert_eq!(shape(&rows), vec!["[2] src/ui"]);
        let Row::Dir(dir) = &rows[0] else {
            panic!("un dossier");
        };
        assert!(dir.collapsed);
        assert_eq!(dir.paths.len(), 2);
        assert_eq!(dir.added, 2);
    }

    #[test]
    fn files_at_the_root_come_after_the_directories() {
        let rows = tree(&["Cargo.toml", "src/main.rs"], &[]);
        assert_eq!(shape(&rows), vec!["[1] src", " main.rs", "Cargo.toml"]);
    }

    #[test]
    fn groups_survive_the_tree() {
        let flat = rows_for(
            &DiffRange::Working,
            &status(vec![
                file("src/a.rs", StatusCode::Modified, StatusCode::Unmodified),
                file("src/b.rs", StatusCode::Untracked, StatusCode::Untracked),
            ]),
            &[],
            &[],
        );
        let rows = tree_rows(&flat, &HashSet::new());
        // One tree per group, and not a single tree mixing tracked and untracked
        // under the same folder.
        assert_eq!(
            shape(&rows),
            vec!["groupe", "[1] src", " a.rs", "groupe", "[1] src", " b.rs"]
        );
    }

    #[test]
    fn staged_and_unstaged_files_share_one_list() {
        // The point of the merge: no more two ranges to stitch together
        // mentally, one list where the box says what will go into the next commit.
        let status = status(vec![
            file("indexe.rs", StatusCode::Modified, StatusCode::Unmodified),
            file("modifie.rs", StatusCode::Unmodified, StatusCode::Modified),
            file("nouveau.rs", StatusCode::Untracked, StatusCode::Untracked),
        ]);
        let rows = rows_for(&DiffRange::Working, &status, &[], &[]);
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

        // Files never added form their own group: ticking them does not mean the
        // same thing as for a file already tracked.
        assert_eq!(groups_of(&rows), vec![Group::Tracked, Group::Untracked]);
    }

    #[test]
    fn a_partially_staged_file_says_so() {
        // `MM`: a ticked box would suggest the whole file is going out.
        let status = status(vec![file(
            "moitie.rs",
            StatusCode::Modified,
            StatusCode::Modified,
        )]);
        let rows = rows_for(&DiffRange::Working, &status, &[], &[]);
        let file = files_of(&rows)[0];
        assert!(file.staged);
        assert!(file.partial(), "partial staging must be reported");
        assert_eq!(file.codes(), "MM");
    }

    #[test]
    fn the_codes_show_what_git_says() {
        let status = status(vec![
            file("ajoute.rs", StatusCode::Added, StatusCode::Unmodified),
            file("efface.rs", StatusCode::Unmodified, StatusCode::Deleted),
            file("neuf.rs", StatusCode::Untracked, StatusCode::Untracked),
        ]);
        let rows = rows_for(&DiffRange::Working, &status, &[], &[]);
        let files = files_of(&rows);
        assert_eq!(files[0].codes(), "A");
        assert_eq!(files[1].codes(), "D");
        assert_eq!(files[2].codes(), "?");
    }

    fn diff_file(path: &str, added: usize, removed: usize) -> DiffFile {
        DiffFile {
            path: PathBuf::from(path),
            original: None,
            added,
            removed,
            binary: false,
        }
    }

    fn reviewed(path: &str, added: usize, removed: usize) -> crate::ui::vault::Reviewed {
        crate::ui::vault::Reviewed {
            range: DiffRange::Working,
            path: PathBuf::from(path),
            added,
            removed,
        }
    }

    /// The recorded volume is what expires the tick: an agent that rewrites a
    /// file cancels its review, otherwise the list would say "reviewed" of
    /// content nobody has read.
    #[test]
    fn a_file_that_changed_since_is_no_longer_reviewed() {
        let status = status(vec![file(
            "a.rs",
            StatusCode::Modified,
            StatusCode::Unmodified,
        )]);
        let rows = rows_for(
            &DiffRange::Working,
            &status,
            &[diff_file("a.rs", 12, 3)],
            &[reviewed("a.rs", 12, 3)],
        );
        assert!(files_of(&rows)[0].reviewed);

        let rows = rows_for(
            &DiffRange::Working,
            &status,
            &[diff_file("a.rs", 13, 3)],
            &[reviewed("a.rs", 12, 3)],
        );
        assert!(
            !files_of(&rows)[0].reviewed,
            "the file has changed again since"
        );
    }

    /// A review taken in one range does not hold in the other: it is not the
    /// same diff that was read.
    #[test]
    fn a_review_belongs_to_its_range() {
        let status = status(vec![file(
            "a.rs",
            StatusCode::Modified,
            StatusCode::Unmodified,
        )]);
        let rows = rows_for(
            &DiffRange::Branch {
                base: "master".into(),
            },
            &status,
            &[diff_file("a.rs", 1, 0)],
            &[reviewed("a.rs", 1, 0)],
        );
        assert!(!files_of(&rows)[0].reviewed);
    }

    /// A folder's tick only lights up when its whole subtree has been read,
    /// collapsed part included — that is what it promises in one click.
    #[test]
    fn a_directory_is_reviewed_only_when_all_of_it_is() {
        let status = status(vec![
            file("src/un.rs", StatusCode::Modified, StatusCode::Unmodified),
            file("src/deux.rs", StatusCode::Modified, StatusCode::Unmodified),
        ]);
        let files = [diff_file("src/un.rs", 1, 0), diff_file("src/deux.rs", 2, 0)];
        let flat = rows_for(
            &DiffRange::Working,
            &status,
            &files,
            &[reviewed("src/un.rs", 1, 0)],
        );
        let dirs = tree_rows(&flat, &HashSet::new());
        let dir = dirs
            .iter()
            .find_map(|row| match row {
                Row::Dir(dir) => Some(dir),
                _ => None,
            })
            .expect("un dossier");
        assert!(!dir.reviewed);
        assert_eq!(dir.paths.len(), 2, "la coche porte tout le sous-arbre");

        let flat = rows_for(
            &DiffRange::Working,
            &status,
            &files,
            &[reviewed("src/un.rs", 1, 0), reviewed("src/deux.rs", 2, 0)],
        );
        let dirs = tree_rows(&flat, &HashSet::new());
        assert!(dirs
            .iter()
            .any(|row| matches!(row, Row::Dir(dir) if dir.reviewed)));
    }

    #[test]
    fn an_empty_group_is_not_shown() {
        let status = status(vec![file(
            "suivi.rs",
            StatusCode::Modified,
            StatusCode::Unmodified,
        )]);
        let rows = rows_for(&DiffRange::Working, &status, &[], &[]);
        assert_eq!(groups_of(&rows), vec![Group::Tracked]);
    }

    #[test]
    fn a_group_is_checked_only_when_all_of_it_is() {
        let mixed = status(vec![
            file("un.rs", StatusCode::Modified, StatusCode::Unmodified),
            file("deux.rs", StatusCode::Unmodified, StatusCode::Modified),
        ]);
        let rows = rows_for(&DiffRange::Working, &mixed, &[], &[]);
        assert!(!group_checked(&rows, Group::Tracked));
        assert_eq!(group_paths(&rows, Group::Tracked).len(), 2);

        let everything = status(vec![
            file("un.rs", StatusCode::Modified, StatusCode::Unmodified),
            file("deux.rs", StatusCode::Added, StatusCode::Unmodified),
        ]);
        let rows = rows_for(&DiffRange::Working, &everything, &[], &[]);
        assert!(group_checked(&rows, Group::Tracked));
    }

    #[test]
    fn a_group_checkbox_only_covers_its_own_files() {
        let status = status(vec![
            file("suivi.rs", StatusCode::Modified, StatusCode::Unmodified),
            file("neuf.rs", StatusCode::Untracked, StatusCode::Untracked),
        ]);
        let rows = rows_for(&DiffRange::Working, &status, &[], &[]);
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
    fn arrows_walk_the_file_list_without_wrapping() {
        assert_eq!(step_index(Some(1), 1, 4), Some(2));
        assert_eq!(
            step_index(Some(0), -1, 4),
            None,
            "before the first, nothing"
        );
        assert_eq!(
            step_index(Some(3), 1, 4),
            None,
            "after the last, nothing either"
        );
        assert_eq!(step_index(None, 1, 4), Some(0));
        assert_eq!(step_index(None, -1, 4), Some(3));
        assert_eq!(step_index(None, 1, 0), None);
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
        let rows = rows_for(&DiffRange::Working, &status, &files, &[]);
        let row = files_of(&rows)[0];
        assert_eq!((row.added, row.removed), (12, 3));

        // With `--numstat` not yet arrived, the row still shows.
        let rows = rows_for(&DiffRange::Working, &status, &[], &[]);
        assert_eq!(
            (files_of(&rows)[0].added, files_of(&rows)[0].removed),
            (0, 0)
        );
    }

    #[test]
    fn commit_ranges_come_from_the_file_list_alone() {
        // No status at all: a commit review only talks about what git has
        // already written, and nothing in it is to be ticked.
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
            &[],
        );
        assert!(groups_of(&rows).is_empty(), "pas de groupes sur un commit");
        let row = files_of(&rows)[0];
        assert_eq!(row.name, "ajoute.rs");
        assert_eq!(row.directory, "dossier");
        assert_eq!(row.index, StatusCode::Added);
        assert!(!row.partial());
    }
}
