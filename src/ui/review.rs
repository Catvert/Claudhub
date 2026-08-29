//! The review: the list of touched files, and the chosen file's diff.
//!
//! Four comparison ranges, chosen by the tabs at the head of the list: the
//! unstaged changes, the index, the whole checkout against HEAD, and the whole
//! branch since it diverged from its base. The last is the one used to review an
//! agent's work before pushing it.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::rc::Rc;

use gpui::{div, prelude::*, px, uniform_list, Context, Focusable, Window};
use gpui_component::{
    button::{Button, ButtonVariants},
    checkbox::Checkbox,
    h_flex,
    input::Textarea,
    select::Select,
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
    Group(GroupRow),
    /// A folder of the tree, collapsible.
    Dir(DirRow),
    File(FileRow),
}

/// A group heading, with what its box acts on.
///
/// The files and the tick are carried by the row rather than read back from the
/// list at paint time: that read walked the whole list twice per group and per
/// frame.
#[derive(Clone)]
struct GroupRow {
    group: Group,
    paths: Rc<[PathBuf]>,
    /// The whole group is already staged.
    checked: bool,
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
    path: Rc<PathBuf>,
    /// What is displayed: one segment, or the merged chain.
    label: String,
    depth: usize,
    collapsed: bool,
    /// Every file of the subtree, including those a collapse hides: ticking a
    /// closed folder must stage what it contains, and not what is visible of it.
    ///
    /// Behind an `Rc`: the two boxes of a visible folder capture it on every
    /// frame, and a subtree can hold hundreds of paths.
    paths: Rc<[PathBuf]>,
    /// True when the whole subtree is already staged.
    staged: bool,
    /// True when the whole subtree has already been reviewed — that is what
    /// makes a click on a folder a review of everything it contains.
    reviewed: bool,
}

#[derive(Clone)]
struct FileRow {
    /// The path, in a slice of one: the staging box and the review tick both act
    /// on a list of paths, and an `Rc` costs a visible row no allocation per
    /// frame. `path()` reads it back.
    paths: Rc<[PathBuf]>,
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
    fn path(&self) -> &Path {
        &self.paths[0]
    }

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

        let find = self.render_find(Self::find_pane(&range), cx);
        // Built here, with the two other things that need `&mut self`, and not
        // in a `when` down the tree: the rest of this function holds the review
        // state borrowed.
        let bar = if matches!(range, DiffRange::Working) {
            self.render_changes_bar(cx).into_any_element()
        } else {
            self.render_base_bar(cx).into_any_element()
        };
        // It is the panel that asks for its list: it alone knows what it shows,
        // and loading both ranges in advance would cost a command for a tab
        // nobody will open.
        self.ensure_files(range.clone(), cx);
        // Taken before any borrow of the state: it is the view that holds it,
        // and the list only hangs off it.
        let scroll = self.file_scroll(&range);
        let Some(view) = self.rows_view(&range, cx) else {
            return div().into_any_element();
        };
        let tree = crate::ui::settings::Settings::global(cx).review_tree;
        let Some(state) = self.review.get(&worktree) else {
            return div().into_any_element();
        };
        let selected = state.selected.clone();
        let staged_count = view.staged;
        let rows = view.shown;
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
            .child(bar)
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
                        // Behind `Rc`: every gesture of every visible row
                        // captures them, and that was four copies of the
                        // worktree's path per row and per frame.
                        let row_range = Rc::new(range.clone());
                        let worktree = Rc::new(worktree);
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
                                                    ix,
                                                    &worktree,
                                                    &row_range,
                                                    selected.as_deref(),
                                                    &colors,
                                                    checkable,
                                                    tree,
                                                    &entity,
                                                    cx,
                                                )
                                            })
                                            .collect::<Vec<_>>()
                                    },
                                )
                                .size_full()
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

    /// The pane whose search bar filters a range's list.
    ///
    /// Two panels show this list at the same time: each has its own search,
    /// otherwise filtering the changes would also filter the branch review.
    fn find_pane(range: &DiffRange) -> crate::ui::find::Pane {
        if matches!(range, DiffRange::Working) {
            crate::ui::find::Pane::Changes
        } else {
            crate::ui::find::Pane::Branch
        }
    }

    /// A range's rows, from the cache, rebuilt only when it is stale.
    ///
    /// The status is the source for the changes in progress — it alone tells the
    /// index from the working tree — and `--numstat` for the ranges that are
    /// about commits and have no notion of an index.
    fn rows_view(&mut self, range: &DiffRange, cx: &gpui::App) -> Option<RowsView> {
        let query = self.query(Self::find_pane(range), cx);
        let tree = crate::ui::settings::Settings::global(cx).review_tree;
        let worktree = self.active.clone()?;
        let state = self.review.get_mut(&worktree)?;
        let epoch = state.rows_epoch;
        let fresh = state.row_cache.get(range).is_some_and(|cache| {
            cache.epoch == epoch && cache.tree == tree && cache.query == query
        });
        if !fresh {
            let files = state.files.get(range).map(Vec::as_slice).unwrap_or(&[]);
            let flat = Rc::new(rows_for(
                range,
                &state.status,
                files,
                &state.reviewed,
                &query,
            ));
            let staged = flat
                .iter()
                .filter(|row| matches!(row, Row::File(file) if file.staged))
                .count();
            let shown = shown_rows(&flat, &query, tree, &state.collapsed);
            state.row_cache.insert(
                range.clone(),
                RowCache {
                    epoch,
                    query,
                    tree,
                    shown,
                    staged,
                },
            );
        }
        let cache = state.row_cache.get(range)?;
        Some(RowsView {
            shown: cache.shown.clone(),
            staged: cache.staged,
        })
    }

    /// The list's files, in the order they are displayed.
    ///
    /// The displayed order and not the raw one: a collapsed folder hides its
    /// files, and the arrows must not open a file the list does not show — the
    /// next one would then be impossible to find by eye. The search counts the
    /// same way: it is the same list.
    fn visible_files(&mut self, range: &DiffRange, cx: &gpui::App) -> Vec<PathBuf> {
        let Some(view) = self.rows_view(range, cx) else {
            return Vec::new();
        };
        view.shown
            .iter()
            .filter_map(|row| match row {
                Row::File(file) => Some(file.path().to_path_buf()),
                _ => None,
            })
            .collect()
    }

    /// Brings the list onto a file.
    ///
    /// The index is that of the **displayed** list — folders included, and
    /// without what the collapses hide: it is that list the view virtualises,
    /// and an index taken elsewhere would name another row.
    pub(super) fn reveal_file(&mut self, range: &DiffRange, path: &Path, cx: &mut Context<Self>) {
        let Some(view) = self.rows_view(range, cx) else {
            return;
        };
        let Some(index) = view
            .shown
            .iter()
            .position(|row| matches!(row, Row::File(file) if file.path() == path))
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

    /// Where the reviewed file stands: its one-based rank in the displayed
    /// list, and the list's length.
    ///
    /// The same list the arrows walk — folders collapsed, hidden files out —
    /// so the count matches what the two buttons around it will actually do.
    /// `None` when the list does not show the file.
    pub(super) fn diff_file_position(
        &mut self,
        path: &Path,
        cx: &gpui::App,
    ) -> Option<(usize, usize)> {
        let worktree = self.active.clone()?;
        let range = self.review.get(&worktree)?.range.clone();
        let files = self.visible_files(&range, cx);
        let rank = files.iter().position(|file| file == path)? + 1;
        Some((rank, files.len()))
    }

    /// The changes panel's bar: what one does to the repository, then the
    /// display toggle.
    ///
    /// `fetch`, `pull` and `push` live here and not in the window's toolbar:
    /// they are that panel's gestures — you look at what changed, you tick, you
    /// commit, you push — and keeping them at the other end of the screen made
    /// you cross the window to finish a sentence started at the bottom.
    fn render_changes_bar(&mut self, cx: &mut Context<Self>) -> impl IntoElement {
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
            // The tree toggle sits on the left, alone, and keeps its icon: it
            // does not act on the repository, it changes how this list is
            // shown. The gap that follows is what says so — the buttons on the
            // right are the gestures, and this one is a way of looking.
            .child(self.tree_toggle(cx))
            .child(div().flex_1())
            .child(
                // The one gesture that stays wordless. What it does — read what
                // the remote has — is what its two neighbours then act on, and
                // it is the arrow one presses without reading, several times an
                // hour. A word beside it would push the two that decide
                // something out to the edge.
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
            // Throwing everything away sits beside putting it aside: the two
            // gestures that act on *this list* as a whole, before the three
            // that talk to the remote. Disabled on a clean tree — the button
            // then says there is nothing to lose, which is the answer.
            .child({
                let clean = self
                    .active_review()
                    .map(|state| state.status.is_clean())
                    .unwrap_or(true);
                Button::new("rollback-all")
                    .ghost()
                    .xsmall()
                    .icon(icon("undo-2"))
                    .tooltip(tr!("action-rollback-all"))
                    .loading(self.active_running(Action::Discard))
                    .disabled(!has_active || clean)
                    .on_click(
                        cx.listener(|this, _, window, cx| this.confirm_rollback_all(window, cx)),
                    )
            })
            // Right after the fetch, and before the two that talk to the
            // remote: putting the changes aside is what one does *to this
            // list*, and it is the gesture that comes before pulling onto a
            // tree that is not clean. The stack itself is read in the
            // "Stashes" tab.
            .child(
                Button::new("stash")
                    .ghost()
                    .xsmall()
                    .icon(icon("archive"))
                    .label(tr!("action-stash-short"))
                    .tooltip(tr!("stash-new"))
                    .loading(self.active_running(Action::Stash))
                    .disabled(!has_active)
                    .on_click(cx.listener(|this, _, window, cx| this.prompt_stash(window, cx))),
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
                    // The word, and the count beside it when there is one. The
                    // count used to *be* the label: a lone "3" said how much
                    // there was to do without saying what the button did, and
                    // the two arrows differed by their direction alone.
                    .label(with_count(tr!("action-pull-short"), behind))
                    .when(behind > 0, |el| el.primary())
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
                    .label(with_count(tr!("action-push-short"), ahead))
                    .when(ahead > 0, |el| el.primary())
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
            .child(self.find_button(crate::ui::find::Pane::Changes, cx))
    }

    /// The branch review's bar: the toggle, and the choice of base.
    ///
    /// The integration branch git guesses is a starting point, not a fate — one
    /// compares just as well against `dev`, against another working branch or
    /// against a remote.
    fn render_base_bar(&mut self, cx: &mut Context<Self>) -> impl IntoElement {
        self.bar(cx)
            .child(self.tree_toggle(cx))
            // The select takes the room between the two buttons rather than
            // asking for its own: it is a whole `size_full` element, and left to
            // itself in a bar that pushes its children right it filled the bar
            // edge to edge — a bordered field where everything around it is a
            // ghost button, and the widest thing in the panel for the shortest
            // text in it. `appearance(false)` finishes the job: what is read
            // here is the name of the base, and the chevron says it can be
            // changed.
            .child(
                div().flex_1().min_w_0().child(
                    Select::new(&self.base_select)
                        .xsmall()
                        .appearance(false)
                        .title_prefix(tr!("range-base-prefix"))
                        .placeholder(tr!("range-base-placeholder"))
                        .menu_width(crate::ui::base_select::MENU_WIDTH),
                ),
            )
            .child(self.find_button(crate::ui::find::Pane::Branch, cx))
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
                        let pushing = self.active_running(Action::CommitPush);
                        Button::new("commit")
                            .primary()
                            .xsmall()
                            .icon(icon("git-commit-horizontal"))
                            .label(tr!("action-commit"))
                            .loading(committing)
                            .disabled(!can_commit || committing || pushing)
                            .on_click(cx.listener(|this, _, _, cx| this.commit(false, false, cx)))
                    })
                    .child({
                        // **A second button, not a menu on the first.** The two
                        // are one gesture apart and both are made a dozen times
                        // a day; hiding one behind a chevron would cost a click
                        // to the one that ends the work — the commit that stays
                        // local is the exception, on a branch nobody else
                        // reads. It is the ghost of the two: what it adds is a
                        // round trip, and the primary fill belongs to the
                        // gesture that always applies.
                        let committing = self.active_running(Action::Commit);
                        let pushing = self.active_running(Action::CommitPush);
                        Button::new("commit-push")
                            .outline()
                            .xsmall()
                            .icon(icon("arrow-up-from-line"))
                            .label(tr!("action-commit-push"))
                            .loading(pushing)
                            .disabled(!can_commit || committing || pushing)
                            .on_click(cx.listener(|this, _, _, cx| this.commit(false, true, cx)))
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
        state.rows_changed();
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

    /// Commits what is in the index. `amend` reuses the previous commit, and
    /// `push` sends the branch off in the same command — see `Cmd::Commit`.
    pub(super) fn commit(&mut self, amend: bool, push: bool, cx: &mut Context<Self>) {
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
            push,
        };
        let action = if push {
            Action::CommitPush
        } else {
            Action::Commit
        };
        self.start(Some(worktree), action, cmd, cx);
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

    /// Asks for confirmation before rolling the whole worktree back to HEAD.
    ///
    /// The same dialog as `confirm_removal`, for the same reason, only wider:
    /// this one takes every file in the list at once, untracked ones included.
    fn confirm_rollback_all(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(worktree) = self.active.clone() else {
            return;
        };
        let entity = cx.entity();
        let (title, warning) = (tr!("rollback-all-title"), tr!("rollback-all-warning"));
        window.open_dialog(cx, move |dialog, _window, _cx| {
            let (worktree, entity) = (worktree.clone(), entity.clone());
            dialog
                .title(title.clone())
                .child(div().text_xs().child(warning.clone()))
                .overlay_closable(false)
                .close_button(false)
                .footer(super::dialogs::confirm())
                .on_ok(move |_, _window, cx| {
                    entity.update(cx, |this, cx| {
                        let cmd = Cmd::RollbackAll {
                            worktree: worktree.clone(),
                        };
                        this.start(Some(worktree.clone()), Action::Discard, cmd, cx);
                    });
                    true
                })
        });
    }

    /// Stages one hunk of the displayed diff.
    ///
    /// The patch is rebuilt here and not kept beside the diff: recomposing the
    /// whole file as patches cost a copy of it on every diff that arrives, for
    /// a click that happens once in a while — and the click has the state to
    /// hand, which the render's closure did not.
    pub(super) fn stage_hunk(&mut self, hunk: usize, cx: &mut Context<Self>) {
        let Some(worktree) = self.active.clone() else {
            return;
        };
        let Some(diff) = self.active_review().and_then(|state| state.diff.clone()) else {
            return;
        };
        let Some(hunk) = diff.file.hunks.get(hunk) else {
            return;
        };
        let patch = crate::git::diff::hunk_patch(&diff.path, None, hunk, false);
        self.git.send(Cmd::ApplyHunk {
            worktree,
            patch,
            reverse: false,
        });
        cx.notify();
    }

    /// Stages one hunk of the remainder panel — the part of a partially
    /// staged file that is not in the index yet.
    ///
    /// The patch is built from the **unstaged** diff (index → working tree),
    /// which is exactly the base `git apply --cached` applies against; the
    /// displayed diff would not do, its hunks being measured from HEAD.
    pub(super) fn stage_remainder_hunk(&mut self, hunk: usize, cx: &mut Context<Self>) {
        let Some(worktree) = self.active.clone() else {
            return;
        };
        let Some((path, diff)) = self
            .active_review()
            .and_then(|state| state.unstaged.clone())
        else {
            return;
        };
        let Some(hunk) = diff.hunks.get(hunk) else {
            return;
        };
        let patch = crate::git::diff::hunk_patch(&path, None, hunk, false);
        self.git.send(Cmd::ApplyHunk {
            worktree,
            patch,
            reverse: false,
        });
        cx.notify();
    }

    /// The "other part" of a partially staged file: the hunks the next commit
    /// would leave behind, each with the gesture that adds it to the index.
    ///
    /// Under the diff and not in it: the displayed diff shows the whole
    /// change, and marking which of its lines are staged would mean stitching
    /// two diffs together line by line. This panel shows the remainder as git
    /// tells it, and disappears when there is none.
    pub(super) fn render_unstaged_panel(
        &mut self,
        cx: &mut Context<Self>,
    ) -> Option<gpui::AnyElement> {
        let state = self.active_review()?;
        if !matches!(state.range, DiffRange::Working) {
            return None;
        }
        let path = state.selected.clone()?;
        let (kept, diff) = state.unstaged.clone()?;
        if kept != path || diff.hunks.is_empty() {
            return None;
        }
        let theme = cx.theme().clone();
        let colors = crate::ui::theme::DiffColors::of(cx);
        let mono = theme.mono_font_family.clone();
        let font_size = gpui::px(crate::ui::settings::Settings::global(cx).diff_font_size);
        let count = diff.hunks.len();

        let hunks = diff.hunks.iter().enumerate().map(|(ix, hunk)| {
            v_flex()
                .child(
                    h_flex()
                        .px_2()
                        .gap_2()
                        .items_center()
                        .bg(colors.hunk_bg)
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .truncate()
                                .text_xs()
                                .text_color(theme.muted_foreground)
                                .child(hunk.header.clone()),
                        )
                        .child(
                            Button::new(("stage-remainder", ix))
                                .ghost()
                                .xsmall()
                                .icon(icon("plus"))
                                .label(tr!("diff-unstaged-add"))
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    this.stage_remainder_hunk(ix, cx);
                                })),
                        ),
                )
                .children(hunk.lines.iter().map(|line| {
                    use crate::git::DiffLineKind;
                    let (bg, fg, sign) = match line.kind {
                        DiffLineKind::Added => (Some(colors.added_bg), Some(colors.added_fg), "+"),
                        DiffLineKind::Removed => {
                            (Some(colors.removed_bg), Some(colors.removed_fg), "-")
                        }
                        DiffLineKind::Context => (None, None, " "),
                        DiffLineKind::NoNewline => (None, None, "\\"),
                    };
                    div()
                        .px_2()
                        .whitespace_nowrap()
                        .overflow_hidden()
                        .when_some(bg, |el, bg| el.bg(bg))
                        .when_some(fg, |el, fg| el.text_color(fg))
                        .child(format!("{sign}{}", line.text))
                }))
        });

        Some(
            v_flex()
                .flex_none()
                .border_t_1()
                .border_color(theme.border)
                .child(
                    h_flex()
                        .h(crate::ui::theme::bar_height(cx))
                        .px_2()
                        .gap_2()
                        .items_center()
                        .border_b_1()
                        .border_color(theme.border)
                        .child(
                            div()
                                .text_xs()
                                .text_color(theme.muted_foreground)
                                .child(tr!("diff-unstaged-title", { count: count })),
                        )
                        .child(div().flex_1())
                        .child(
                            Button::new("stage-remainder-all")
                                .ghost()
                                .xsmall()
                                .label(tr!("diff-unstaged-add-all"))
                                .on_click(cx.listener({
                                    let worktree = self.active.clone();
                                    let path = path.clone();
                                    move |this, _, _, cx| {
                                        if let Some(worktree) = worktree.clone() {
                                            this.set_staged(worktree, vec![path.clone()], true, cx);
                                        }
                                    }
                                })),
                        ),
                )
                .child(
                    div()
                        .id("unstaged-remainder")
                        .max_h(gpui::px(240.))
                        .overflow_y_scroll()
                        .font_family(mono)
                        .text_size(font_size)
                        .child(v_flex().children(hunks)),
                )
                .into_any_element(),
        )
    }
}

/// Renders one row of the list: a group heading or a file.
///
/// A free function because a virtualised list's closure does not receive the
/// view: it captures the entity and goes back through `update` to act, as the
/// dialog handlers do.
#[allow(clippy::too_many_arguments)]
fn render_row(
    rows: &Rc<Vec<Row>>,
    index: usize,
    worktree: &Rc<PathBuf>,
    range: &Rc<DiffRange>,
    selected: Option<&Path>,
    colors: &DiffColors,
    checkable: bool,
    // Is the list in tree form? A file then reserves the place of the chevron
    // it does not have; flat, that place would be a column of nothing.
    tree: bool,
    entity: &gpui::Entity<ClaudhubApp>,
    cx: &mut gpui::App,
) -> gpui::AnyElement {
    match rows.get(index) {
        Some(Row::Group(group)) => render_group(group, index, worktree, entity, cx),
        Some(Row::Dir(dir)) => render_dir(dir, index, worktree, range, checkable, entity, cx),
        Some(Row::File(file)) => render_file(
            file, index, worktree, range, selected, colors, checkable, tree, entity, cx,
        ),
        None => div().into_any_element(),
    }
}

/// A button's word, with the count of what is waiting when there is any.
///
/// Kept out of the catalogues: "Pull 3" is the same in both languages, and a
/// key per shape would be two entries saying nothing a `format!` does not.
fn with_count(label: gpui::SharedString, count: usize) -> gpui::SharedString {
    match count {
        0 => label,
        _ => gpui::SharedString::from(format!("{label} {count}")),
    }
}

/// A folder: the chevron, the box that stages everything it contains, and how
/// many files it holds.
#[allow(clippy::too_many_arguments)]
fn render_dir(
    row: &DirRow,
    index: usize,
    worktree: &Rc<PathBuf>,
    range: &Rc<DiffRange>,
    checkable: bool,
    entity: &gpui::Entity<ClaudhubApp>,
    cx: &mut gpui::App,
) -> gpui::AnyElement {
    let staged = row.staged;
    let count = row.paths.len();

    h_flex()
        .id(("dir", index))
        .h(crate::ui::theme::row_height(cx))
        // A band across the panel, like the project explorer's: see
        // "L'explorateur de projet" in CLAUDE.md.
        .w_full()
        .pl_1()
        .pr(crate::ui::theme::scroll_gutter())
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
                entity.update(cx, |this, cx| this.toggle_directory((*path).clone(), cx));
            }
        })
        // The rules of the levels above, as in the project explorer: the tree
        // is the same gesture on both sides of the window, and an indentation
        // without them reads as names that happen to start further right.
        .children(crate::ui::theme::indent_guides(
            row.depth,
            crate::ui::theme::indent_guide(cx),
        ))
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
            let (entity, worktree, paths) = (entity.clone(), worktree.clone(), row.paths.clone());
            el.child(
                Checkbox::new(("stage-dir", index))
                    .checked(staged)
                    .on_click(move |_, _window, cx| {
                        cx.stop_propagation();
                        entity.update(cx, |this, cx| {
                            this.set_staged((*worktree).clone(), paths.to_vec(), !staged, cx)
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
        // The count of files, and **not** the volume of lines. A folder summing
        // `+142 −4` sits in the same column as the `+65 −2` of the file under
        // it, in the same colours, and reads as a fourth file rather than as
        // the total of the three: what a folder answers is how many there are
        // to go through, and the lines are read file by file.
        .child(render_reviewed(
            ("reviewed-dir", index),
            row.reviewed,
            worktree,
            range,
            &row.paths,
            entity,
            cx,
        ))
        .into_any_element()
}

fn render_group(
    row: &GroupRow,
    index: usize,
    worktree: &Rc<PathBuf>,
    entity: &gpui::Entity<ClaudhubApp>,
    cx: &mut gpui::App,
) -> gpui::AnyElement {
    let checked = row.checked;
    let paths = row.paths.clone();
    let count = paths.len();
    let label = match row.group {
        Group::Tracked => tr!("group-tracked"),
        Group::Untracked => tr!("group-untracked"),
    };
    let (entity, worktree) = (entity.clone(), worktree.clone());

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
                        this.set_staged((*worktree).clone(), paths.to_vec(), !checked, cx)
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
    worktree: &Rc<PathBuf>,
    range: &Rc<DiffRange>,
    selected: Option<&Path>,
    colors: &DiffColors,
    checkable: bool,
    tree: bool,
    entity: &gpui::Entity<ClaudhubApp>,
    cx: &mut gpui::App,
) -> gpui::AnyElement {
    let is_selected = selected == Some(row.path());
    let staged = row.staged;

    h_flex()
        .id(("file", index))
        .h(crate::ui::theme::row_height(cx))
        .w_full()
        .pl_1()
        .pr(crate::ui::theme::scroll_gutter())
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
                worktree.clone(),
                row.paths.clone(),
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
                    this.open_file((*worktree).clone(), path[0].clone(), (*range).clone(), cx)
                });
            }
        })
        .children(crate::ui::theme::indent_guides(
            row.depth,
            crate::ui::theme::indent_guide(cx),
        ))
        // The place of the chevron a file does not have: without it a file's
        // box sits to the **left** of the box of the folder holding it, and the
        // whole indentation is read backwards. Only in tree form — flat, that
        // place is a column of nothing.
        .when(tree, |el| el.child(crate::ui::theme::chevron_space()))
        // Ticking is staging. The ranges that are about commits already written
        // have nothing to tick: a box there would be a button that lies.
        .when(checkable, |el| {
            let (entity, worktree, paths) = (entity.clone(), worktree.clone(), row.paths.clone());
            el.child(Checkbox::new(("stage", index)).checked(staged).on_click(
                move |_, _window, cx| {
                    cx.stop_propagation();
                    entity.update(cx, |this, cx| {
                        this.set_staged((*worktree).clone(), paths.to_vec(), !staged, cx)
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
        .child(crate::ui::file_icons::file_icon(row.path(), cx))
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
            let (entity, worktree, paths) = (entity.clone(), worktree.clone(), row.paths.clone());
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
                        cx.stop_propagation();
                        entity.update(cx, |this, cx| {
                            this.confirm_removal(
                                (*worktree).clone(),
                                paths[0].clone(),
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
            &row.paths,
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
    worktree: &Rc<PathBuf>,
    range: &Rc<DiffRange>,
    paths: &Rc<[PathBuf]>,
    entity: &gpui::Entity<ClaudhubApp>,
    cx: &gpui::App,
) -> Button {
    let (entity, worktree, range, paths) = (
        entity.clone(),
        worktree.clone(),
        range.clone(),
        paths.clone(),
    );
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
            cx.stop_propagation();
            entity.update(cx, |this, cx| {
                this.set_reviewed(
                    (*worktree).clone(),
                    (*range).clone(),
                    paths.to_vec(),
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
    query: &str,
) -> Vec<Row> {
    let volumes: std::collections::HashMap<&PathBuf, (usize, usize)> = files
        .iter()
        .map(|f| (&f.path, (f.added, f.removed)))
        .collect();
    let volume = |path: &PathBuf| volumes.get(path).copied().unwrap_or((0, 0));
    // Reviewed **and** unchanged since: the recorded volume is what expires the
    // tick, otherwise it would say "reviewed" of a file an agent has just
    // rewritten.
    //
    // A set and not a scan: a branch review holds as many reviews as files, and
    // the scan made the list quadratic.
    let ticks: HashSet<(&Path, usize, usize)> = reviewed
        .iter()
        .filter(|item| item.range == *range)
        .map(|item| (item.path.as_path(), item.added, item.removed))
        .collect();
    let is_reviewed =
        |path: &Path, added: usize, removed: usize| ticks.contains(&(path, added, removed));
    // The query filters the files before they are grouped: a group whose files
    // have all gone must go with them, and a group's box must only carry what
    // is still shown.
    let keep = |path: &Path| crate::ui::find::matches(query, &path.to_string_lossy());

    match range {
        DiffRange::Working => {
            let mut tracked = Vec::new();
            let mut untracked = Vec::new();
            for file in &status.files {
                if matches!(file.index, StatusCode::Ignored) {
                    continue;
                }
                if !keep(&file.path) {
                    continue;
                }
                let (added, removed) = volume(&file.path);
                let row = FileRow {
                    paths: Rc::from(vec![file.path.clone()]),
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
            for (group, files) in [(Group::Tracked, tracked), (Group::Untracked, untracked)] {
                if files.is_empty() {
                    continue;
                }
                rows.push(Row::Group(GroupRow {
                    group,
                    paths: files.iter().map(|file| file.path().to_path_buf()).collect(),
                    checked: files.iter().all(|file| file.staged),
                }));
                rows.extend(files.into_iter().map(Row::File));
            }
            rows
        }
        DiffRange::Branch { .. } | DiffRange::Commit { .. } => files
            .iter()
            .filter(|f| keep(&f.path))
            .map(|f| {
                Row::File(FileRow {
                    paths: Rc::from(vec![f.path.clone()]),
                    depth: 0,
                    name: crate::git::status::file_name(&f.path),
                    directory: crate::git::status::directory(&f.path),
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
                out.push(Row::Group(group.clone()));
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
    let paths: Vec<PathBuf> = files.iter().map(|file| file.path().to_path_buf()).collect();
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
                // part included: it is what the checkbox stages and what the
                // tick marks reviewed.
                let inside: Vec<&FileRow> = leaves.iter().map(|index| &files[*index]).collect();
                out.push(Row::Dir(DirRow {
                    path: Rc::new(path),
                    label,
                    depth,
                    collapsed,
                    paths: inside
                        .iter()
                        .map(|file| file.path().to_path_buf())
                        .collect(),
                    staged: inside.iter().all(|file| file.staged),
                    reviewed: inside.iter().all(|file| file.reviewed),
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

/// What the list paints, from the flat rows it is derived from.
///
/// One function for both the render and the arrows: they read the same list, and
/// while they each computed their own, the arrows opened files a filtered list
/// did not show.
fn shown_rows(
    flat: &Rc<Vec<Row>>,
    query: &str,
    tree: bool,
    collapsed: &HashSet<PathBuf>,
) -> Rc<Vec<Row>> {
    if !tree {
        // Without the tree, the displayed list **is** the flat one, and copying
        // several hundred rows to say so would be the cost of the whole panel.
        return flat.clone();
    }
    // During a search, collapses are ignored: a file found in a closed folder
    // would not be visible, and the search would look as if it had found
    // nothing.
    if query.trim().is_empty() {
        Rc::new(tree_rows(flat, collapsed))
    } else {
        Rc::new(tree_rows(flat, &HashSet::new()))
    }
}

/// The file list of one range, kept between frames.
///
/// Held by `ReviewState`, one per range. Thrown away by
/// `ReviewState::rows_changed` — status, files, reviews, collapses — and rebuilt
/// when the query or the tree setting no longer matches the one it was built
/// with.
pub struct RowCache {
    epoch: u64,
    query: String,
    tree: bool,
    shown: Rc<Vec<Row>>,
    /// How many files are ticked, over the **whole** list: a collapsed folder
    /// hides its files from `shown`, and it still stages them.
    staged: usize,
}

/// A range's rows as the view reads them back: one `Rc` clone and a count.
struct RowsView {
    shown: Rc<Vec<Row>>,
    staged: usize,
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
                Row::Group(group) => Some(group.group),
                Row::File(_) | Row::Dir(_) => None,
            })
            .collect()
    }

    fn group_of(rows: &[Row], group: Group) -> &GroupRow {
        rows.iter()
            .find_map(|row| match row {
                Row::Group(row) if row.group == group => Some(row),
                _ => None,
            })
            .expect("le groupe")
    }

    fn tree(paths: &[&str], collapsed: &[&str]) -> Vec<Row> {
        let flat: Vec<Row> = paths
            .iter()
            .map(|p| {
                Row::File(FileRow {
                    paths: Rc::from(vec![PathBuf::from(p)]),
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
            "",
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
        let rows = rows_for(&DiffRange::Working, &status, &[], &[], "");
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
        let rows = rows_for(&DiffRange::Working, &status, &[], &[], "");
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
        let rows = rows_for(&DiffRange::Working, &status, &[], &[], "");
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
            "",
        );
        assert!(files_of(&rows)[0].reviewed);

        let rows = rows_for(
            &DiffRange::Working,
            &status,
            &[diff_file("a.rs", 13, 3)],
            &[reviewed("a.rs", 12, 3)],
            "",
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
            "",
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
            "",
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
            "",
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
        let rows = rows_for(&DiffRange::Working, &status, &[], &[], "");
        assert_eq!(groups_of(&rows), vec![Group::Tracked]);
    }

    #[test]
    fn a_group_is_checked_only_when_all_of_it_is() {
        let mixed = status(vec![
            file("un.rs", StatusCode::Modified, StatusCode::Unmodified),
            file("deux.rs", StatusCode::Unmodified, StatusCode::Modified),
        ]);
        let rows = rows_for(&DiffRange::Working, &mixed, &[], &[], "");
        assert!(!group_of(&rows, Group::Tracked).checked);
        assert_eq!(group_of(&rows, Group::Tracked).paths.len(), 2);

        let everything = status(vec![
            file("un.rs", StatusCode::Modified, StatusCode::Unmodified),
            file("deux.rs", StatusCode::Added, StatusCode::Unmodified),
        ]);
        let rows = rows_for(&DiffRange::Working, &everything, &[], &[], "");
        assert!(group_of(&rows, Group::Tracked).checked);
    }

    #[test]
    fn a_group_checkbox_only_covers_its_own_files() {
        let status = status(vec![
            file("suivi.rs", StatusCode::Modified, StatusCode::Unmodified),
            file("neuf.rs", StatusCode::Untracked, StatusCode::Untracked),
        ]);
        let rows = rows_for(&DiffRange::Working, &status, &[], &[], "");
        assert_eq!(
            group_of(&rows, Group::Untracked).paths.to_vec(),
            vec![PathBuf::from("neuf.rs")]
        );
        assert_eq!(
            group_of(&rows, Group::Tracked).paths.to_vec(),
            vec![PathBuf::from("suivi.rs")]
        );
    }

    /// The list the arrows walk is the list on screen: while the render ignored
    /// the collapses during a search and the arrows did not, the arrows opened
    /// files the filtered list did not show.
    #[test]
    fn a_search_opens_what_a_collapse_hides() {
        let status = status(vec![
            file(
                "src/ui/app.rs",
                StatusCode::Modified,
                StatusCode::Unmodified,
            ),
            file("Cargo.toml", StatusCode::Modified, StatusCode::Unmodified),
        ]);
        let collapsed: HashSet<PathBuf> = [PathBuf::from("src/ui")].into_iter().collect();

        let flat = Rc::new(rows_for(&DiffRange::Working, &status, &[], &[], ""));
        let shown = shown_rows(&flat, "", true, &collapsed);
        assert_eq!(shape(&shown), vec!["groupe", "[1] src/ui", "Cargo.toml"]);

        // The query has kept one file, and it is inside the closed folder: the
        // list has to show it anyway.
        let flat = Rc::new(rows_for(&DiffRange::Working, &status, &[], &[], "app"));
        let shown = shown_rows(&flat, "app", true, &collapsed);
        assert_eq!(shape(&shown), vec!["groupe", "[1] src/ui", " app.rs"]);
    }

    /// A group whose files the search has all removed goes with them: an empty
    /// heading reads as a group with nothing left to do.
    #[test]
    fn a_search_that_empties_a_group_removes_it() {
        let status = status(vec![
            file("suivi.rs", StatusCode::Modified, StatusCode::Unmodified),
            file("neuf.rs", StatusCode::Untracked, StatusCode::Untracked),
        ]);
        let rows = rows_for(&DiffRange::Working, &status, &[], &[], "neuf");
        assert_eq!(groups_of(&rows), vec![Group::Untracked]);
        assert_eq!(files_of(&rows).len(), 1);
        // And the group's box only carries what is left.
        assert_eq!(group_of(&rows, Group::Untracked).paths.len(), 1);
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
        let rows = rows_for(&DiffRange::Working, &status, &files, &[], "");
        let row = files_of(&rows)[0];
        assert_eq!((row.added, row.removed), (12, 3));

        // With `--numstat` not yet arrived, the row still shows.
        let rows = rows_for(&DiffRange::Working, &status, &[], &[], "");
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
            "",
        );
        assert!(groups_of(&rows).is_empty(), "pas de groupes sur un commit");
        let row = files_of(&rows)[0];
        assert_eq!(row.name, "ajoute.rs");
        assert_eq!(row.directory, "dossier");
        assert_eq!(row.index, StatusCode::Added);
        assert!(!row.partial());
    }
}
