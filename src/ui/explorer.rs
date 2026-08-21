//! The project explorer, and touching up a file.
//!
//! **The tree comes from a single git call** (`ls-files --cached --others
//! --exclude-standard`), not from a disk walk: a Laravel project has forty
//! thousand directories, and opening them one by one would cost one system call
//! each to reach the seven hundred that carry code.
//!
//! **The tree is built once**, when the list arrives and on every collapse, and
//! filed behind an `Rc`. Unlike the review list — which counts hundreds of
//! entries — this one counts tens of thousands: rebuilding it on every frame
//! would bring the interface down.
//!
//! **It is browsed with the keyboard**, like PhpStorm's: up and down from one
//! row of the *displayed* list to the next, right to unfold, left to collapse or
//! to go up to the parent folder, Enter to open. Hence a key context of its own
//! (`ClaudhubExplorer`), the bare arrows otherwise belonging to diff review.
//!
//! **The cursor is a path, not an index.** The tree is rebuilt on every
//! collapse, on every search keystroke and on every re-read of the list: an
//! index there would name a different row from one time to the next.
//!
//! **Open and under the cursor are two things**, and they look different: one
//! browses the tree with the keyboard without leaving the file being reviewed.
//!
//! **Editing stays light.** A short touch-up here, the real work in the external
//! editor of your choice: Claudhub does not become an IDE, and `external_editor`
//! is what makes that division workable.

use std::path::{Path, PathBuf};
use std::rc::Rc;

use gpui::{div, prelude::*, px, uniform_list, Context, Entity, Pixels, SharedString, Window};
use gpui_component::{
    button::{Button, ButtonVariants},
    h_flex,
    input::{Editor, EditorState},
    menu::{ContextMenuExt, DropdownMenu, PopupMenuItem},
    v_flex, ActiveTheme, Sizable, WindowExt,
};

use crate::files;
use crate::runtime::Cmd;
use crate::tr;
use crate::ui::app::ClaudhubApp;
use crate::ui::icons::icon;
use crate::ui::settings::Settings;
use crate::ui::theme::status_color;
use crate::ui::tree;

/// A worktree's files, and the tree drawn from them.
pub struct Explorer {
    /// The flat list, as git returns it: it is the reference, and the tree is
    /// only a display of it.
    pub files: Vec<PathBuf>,
    /// The displayed tree, rebuilt on every collapse and never in a render.
    pub rows: Rc<Vec<tree::Entry>>,
    pub collapsed: std::collections::HashSet<PathBuf>,
    /// A request has gone out and not come back: without this guard, every frame
    /// of the panel would restart `ls-files`.
    pub pending: bool,
    /// Were the ignored files asked for, so we know when to re-read.
    pub ignored: bool,
    /// The search `rows` was built for. Compared at render time: that is the
    /// price of having nobody to notify when it changes.
    pub query: String,
    /// The row the keyboard works on — a file or a folder.
    ///
    /// A **path** and not an index: the tree is rebuilt on every collapse, on
    /// every search keystroke and on every re-read of the list, and an index
    /// there would name a different row from one time to the next.
    pub cursor: Option<PathBuf>,
}

impl Default for Explorer {
    fn default() -> Self {
        Self {
            files: Vec::new(),
            rows: Rc::new(Vec::new()),
            collapsed: std::collections::HashSet::new(),
            pending: false,
            ignored: false,
            query: String::new(),
            cursor: None,
        }
    }
}

impl Explorer {
    fn rebuild(&mut self) {
        // During a search, collapses are ignored and the tree is reduced to what
        // matches: a file found in a closed folder would not be visible, and the
        // search would look as if it had found nothing.
        let keep: Option<Vec<usize>> = (!self.query.trim().is_empty()).then(|| {
            self.files
                .iter()
                .enumerate()
                .filter(|(_, path)| crate::ui::find::matches(&self.query, &path.to_string_lossy()))
                .map(|(index, _)| index)
                .collect()
        });
        let open = std::collections::HashSet::new();
        let collapsed = if keep.is_some() {
            &open
        } else {
            &self.collapsed
        };
        let rows = tree::build_subset(&self.files, keep.as_deref(), collapsed);
        self.rows = Rc::new(rows);
    }

    /// A displayed entry's path, folder or file.
    fn path_at(&self, index: usize) -> Option<PathBuf> {
        match self.rows.get(index)? {
            tree::Entry::Dir { path, .. } => Some(path.clone()),
            tree::Entry::Leaf { index, .. } => self.files.get(*index).cloned(),
        }
    }

    /// Where a path sits in the displayed list, if it is still there.
    fn row_of(&self, wanted: &Path) -> Option<usize> {
        (0..self.rows.len()).find(|index| self.path_at(*index).as_deref() == Some(wanted))
    }

    fn is_dir(&self, index: usize) -> bool {
        matches!(self.rows.get(index), Some(tree::Entry::Dir { .. }))
    }

    /// Opens every folder leading to a path.
    ///
    /// Removing each ancestor is enough, merged chains included:
    /// `app/Http/Livewire/Forms` fits on one line but is still an ancestor of
    /// the file it contains.
    fn reveal(&mut self, path: &Path) {
        let mut changed = false;
        for ancestor in path.ancestors().skip(1) {
            changed |= self.collapsed.remove(ancestor);
        }
        if changed {
            self.rebuild();
        }
    }

    /// Collapses everything open at the first level and below.
    fn collapse_all(&mut self) {
        // Every folder, and not only the visible ones: what a closed folder
        // hides has to be closed too when it is reopened.
        for path in &self.files {
            for ancestor in path.ancestors().skip(1) {
                if !ancestor.as_os_str().is_empty() {
                    self.collapsed.insert(ancestor.to_path_buf());
                }
            }
        }
        self.rebuild();
    }

    /// Unfolds a whole subtree.
    fn expand_under(&mut self, root: &Path) {
        self.collapsed
            .retain(|path| !path.starts_with(root) && path != root);
        self.rebuild();
    }

    /// Collapses a whole subtree, its root included.
    fn collapse_under(&mut self, root: &Path) {
        for path in &self.files {
            if !path.starts_with(root) {
                continue;
            }
            for ancestor in path.ancestors().skip(1) {
                if ancestor.starts_with(root) {
                    self.collapsed.insert(ancestor.to_path_buf());
                }
            }
        }
        self.collapsed.insert(root.to_path_buf());
        self.rebuild();
    }
}

/// A file open in the built-in editor.
pub struct Editing {
    pub worktree: PathBuf,
    pub path: PathBuf,
    /// The input entity, created **once** when the file is opened: recreated in
    /// a render, it would lose the cursor and the selection on the first
    /// keystroke.
    pub input: Entity<EditorState>,
    /// Digest of the content read, which makes it possible to refuse to
    /// overwrite an agent's work.
    pub hash: u64,
    /// What is on screen differs from what is on disk.
    pub dirty: bool,
}

impl ClaudhubApp {
    // — The tree ————————————————————————————————————————————————

    fn explorer(&mut self) -> Option<&mut Explorer> {
        let worktree = self.active.clone()?;
        Some(self.explorers.entry(worktree).or_default())
    }

    /// Asks for the file list, if it is missing or if the setting has changed.
    ///
    /// Called when the panel renders: it is what knows what it shows, and
    /// loading the list in advance would cost a command for a tab nobody will
    /// open.
    pub(super) fn ensure_project_files(&mut self, cx: &mut Context<Self>) {
        let Some(worktree) = self.active.clone() else {
            return;
        };
        let ignored = Settings::global(cx).show_ignored_files;
        let explorer = self.explorers.entry(worktree.clone()).or_default();
        if explorer.pending || (!explorer.files.is_empty() && explorer.ignored == ignored) {
            return;
        }
        explorer.pending = true;
        explorer.ignored = ignored;
        self.git.send(Cmd::ListFiles { worktree, ignored });
    }

    pub(super) fn project_files_arrived(&mut self, worktree: PathBuf, files: Vec<PathBuf>) {
        let explorer = self.explorers.entry(worktree).or_default();
        explorer.pending = false;
        explorer.files = files;
        explorer.rebuild();
    }

    pub(super) fn toggle_project_dir(&mut self, path: PathBuf, cx: &mut Context<Self>) {
        let Some(explorer) = self.explorer() else {
            return;
        };
        if !explorer.collapsed.remove(&path) {
            explorer.collapsed.insert(path);
        }
        explorer.rebuild();
        cx.notify();
    }

    /// Brings the cursor's row into view, without making the list jump when it
    /// is already there.
    fn reveal_cursor(&mut self) {
        let Some(explorer) = self.explorer() else {
            return;
        };
        let Some(index) = explorer
            .cursor
            .clone()
            .and_then(|path| explorer.row_of(&path))
        else {
            return;
        };
        self.files_scroll
            .scroll_to_item(index, gpui::ScrollStrategy::Top);
    }

    /// Moves up or down one row in the displayed tree.
    ///
    /// The displayed list, collapses included: it is the one the eye follows,
    /// and descending into a closed folder would lead to invisible rows.
    pub(super) fn step_project_cursor(&mut self, delta: isize, cx: &mut Context<Self>) {
        let Some(explorer) = self.explorer() else {
            return;
        };
        let count = explorer.rows.len();
        if count == 0 {
            return;
        }
        let current = explorer
            .cursor
            .clone()
            .and_then(|path| explorer.row_of(&path))
            .map(|index| index as isize);
        // With no cursor, the first arrow enters from the end it points at, like
        // diff review.
        let next = match current {
            Some(index) => (index + delta).clamp(0, count as isize - 1),
            None if delta > 0 => 0,
            None => count as isize - 1,
        } as usize;
        explorer.cursor = explorer.path_at(next);
        self.reveal_cursor();
        cx.notify();
    }

    /// Takes the cursor to the first or the last of the displayed list.
    pub(super) fn jump_project_cursor(&mut self, last: bool, cx: &mut Context<Self>) {
        let Some(explorer) = self.explorer() else {
            return;
        };
        let count = explorer.rows.len();
        if count == 0 {
            return;
        }
        explorer.cursor = explorer.path_at(if last { count - 1 } else { 0 });
        self.reveal_cursor();
        cx.notify();
    }

    /// Unfolds or collapses at the cursor.
    ///
    /// On a file, the left arrow goes up to the parent folder and the right one
    /// goes down a row: that is what every explorer does, and an inert key reads
    /// as a broken key.
    pub(super) fn fold_project_cursor(&mut self, open: bool, cx: &mut Context<Self>) {
        let Some(explorer) = self.explorer() else {
            return;
        };
        let Some(path) = explorer.cursor.clone() else {
            return self.step_project_cursor(if open { 1 } else { -1 }, cx);
        };
        let Some(index) = explorer.row_of(&path) else {
            return;
        };
        if explorer.is_dir(index) {
            let is_collapsed = explorer.collapsed.contains(&path);
            if open == is_collapsed {
                if open {
                    explorer.collapsed.remove(&path);
                } else {
                    explorer.collapsed.insert(path);
                }
                explorer.rebuild();
                cx.notify();
                return;
            }
        }
        if open {
            self.step_project_cursor(1, cx);
            return;
        }
        // Going up to the folder containing the row: the first ancestor that is
        // itself displayed, merged chains skipping levels.
        let parent = path
            .ancestors()
            .skip(1)
            .find(|ancestor| explorer.row_of(ancestor).is_some())
            .map(Path::to_path_buf);
        if parent.is_some() {
            explorer.cursor = parent;
            self.reveal_cursor();
            cx.notify();
        }
    }

    /// Enter: opens the file, or collapses the folder.
    pub(super) fn activate_project_cursor(&mut self, cx: &mut Context<Self>) {
        let Some(explorer) = self.explorer() else {
            return;
        };
        let Some(path) = explorer.cursor.clone() else {
            return;
        };
        let Some(index) = explorer.row_of(&path) else {
            return;
        };
        if explorer.is_dir(index) {
            self.toggle_project_dir(path, cx);
        } else {
            self.open_in_editor(path, cx);
        }
    }

    /// Shows in the tree the file currently being looked at.
    ///
    /// PhpStorm's "scroll from source" gesture: you are reading a diff and want
    /// to see where the file lives. It unfolds what is needed to reach it, and
    /// is **not** automatic — a list that jumps by itself on every click in the
    /// review is one movement too many.
    pub(super) fn reveal_open_file(&mut self, cx: &mut Context<Self>) {
        let path = self
            .editing
            .as_ref()
            .map(|editing| editing.path.clone())
            .or_else(|| {
                self.active_review()
                    .and_then(|state| state.selected.clone())
            });
        let Some(path) = path else {
            return;
        };
        let Some(explorer) = self.explorer() else {
            return;
        };
        explorer.reveal(&path);
        explorer.cursor = Some(path);
        self.reveal_cursor();
        cx.notify();
    }

    /// Gives the tree focus and puts the cursor in it.
    ///
    /// Without the focus, the arrow following the click would go to the diff:
    /// the bindings are settled on the focused node's context, and the tree is
    /// not focused merely because it was clicked in.
    pub(super) fn focus_project_tree(
        &mut self,
        path: PathBuf,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.explorer_focus.focus(window, cx);
        if let Some(explorer) = self.explorer() {
            explorer.cursor = Some(path);
        }
        cx.notify();
    }

    pub(super) fn expand_project_dir(&mut self, path: PathBuf, cx: &mut Context<Self>) {
        if let Some(explorer) = self.explorer() {
            explorer.expand_under(&path);
        }
        cx.notify();
    }

    pub(super) fn collapse_project_dir(&mut self, path: PathBuf, cx: &mut Context<Self>) {
        if let Some(explorer) = self.explorer() {
            explorer.collapse_under(&path);
        }
        cx.notify();
    }

    /// Copies an entry's path, relative to the worktree or absolute.
    ///
    /// Both are used, and not for the same things: the relative one is pasted
    /// into an agent's prompt, which works from the worktree; the absolute one
    /// into a terminal opened elsewhere.
    pub(super) fn copy_project_path(
        &mut self,
        path: &Path,
        absolute: bool,
        cx: &mut Context<Self>,
    ) {
        let text = match (absolute, self.active.as_ref()) {
            (true, Some(worktree)) => worktree.join(path).display().to_string(),
            _ => path.display().to_string(),
        };
        cx.write_to_clipboard(gpui::ClipboardItem::new_string(text));
        self.announce(tr!("copy-path-done"), cx);
    }

    pub(super) fn collapse_project_tree(&mut self, cx: &mut Context<Self>) {
        if let Some(explorer) = self.explorer() {
            explorer.collapse_all();
        }
        cx.notify();
    }

    pub(super) fn toggle_ignored_files(&mut self, cx: &mut Context<Self>) {
        Settings::update_global(cx, |s| s.show_ignored_files = !s.show_ignored_files);
        // The list changes order of magnitude: we ask for it again rather than
        // filter the one we have, which has never seen the ignored files.
        if let Some(explorer) = self.explorer() {
            explorer.files.clear();
            explorer.rebuild();
        }
        cx.notify();
    }

    // — Reading and writing ———————————————————————————————————

    /// Opens a file in the built-in editor.
    pub(super) fn open_in_editor(&mut self, path: PathBuf, cx: &mut Context<Self>) {
        let Some(worktree) = self.active.clone() else {
            return;
        };
        self.git.send(Cmd::ReadFile { worktree, path });
        cx.notify();
    }

    /// Receives a content and installs the editor.
    pub(super) fn file_content_arrived(
        &mut self,
        worktree: PathBuf,
        path: PathBuf,
        content: files::Content,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        // The language follows from the extension, as for a diff's highlighting:
        // it is the same table, PHP included.
        let language = crate::ui::highlight::language_for_path(&path).unwrap_or("text");
        let input = cx.new(|cx| {
            // `EditorState` and not `InputState`: the input rework split the
            // three modes into three types — one line, multi-line text, code.
            // The code features (language, line numbers, LSP) only exist on the
            // third.
            EditorState::new(window, cx)
                .language(language)
                .line_number(true)
                .default_value(content.text)
        });
        // The subscription is set up here, once per opened file: it is what
        // lights the unsaved-change indicator.
        cx.subscribe(&input, |this, _, event, cx| {
            if !matches!(event, gpui_component::input::InputEvent::Change) {
                return;
            }
            if let Some(editing) = this.editing.as_mut() {
                editing.dirty = true;
            }
            cx.notify();
        })
        .detach();
        self.editing = Some(Editing {
            worktree,
            path,
            input,
            hash: content.hash,
            dirty: false,
        });
        // A file that opens calls up the screen it is edited on. The gesture
        // comes from the explorer — so from that screen most of the time — but
        // also from a diff line, and answering it silently on the screen next
        // door would be an opened file nobody sees.
        self.enter_workspace(crate::ui::workspace::Workspace::Files, window, cx);
        self.set_panel_visible(crate::ui::panels::EditorPanel::NAME, true, cx);
        cx.notify();
    }

    pub(super) fn save_file(&mut self, cx: &mut Context<Self>) {
        let Some(editing) = self.editing.as_ref() else {
            return;
        };
        let content = editing.input.read(cx).value().to_string();
        self.git.send(Cmd::WriteFile {
            worktree: editing.worktree.clone(),
            path: editing.path.clone(),
            content: content.clone(),
            // The digest of what we had read: an agent that wrote in the
            // meantime makes the save be refused rather than be overwritten.
            expect: Some(editing.hash),
        });
        // The digest follows what has just been sent: without that, two saves in
        // a row would make the second be refused, the file having changed — by
        // us.
        if let Some(editing) = self.editing.as_mut() {
            editing.hash = files::digest(&content);
            editing.dirty = false;
        }
        cx.notify();
    }

    /// Closes the editor, asking for confirmation if the file has changed.
    pub(super) fn close_editor(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(editing) = self.editing.as_ref() else {
            return;
        };
        if !editing.dirty {
            self.editing = None;
            cx.notify();
            return;
        }
        let label = SharedString::from(editing.path.display().to_string());
        let entity = cx.entity();
        window.open_dialog(cx, move |dialog, _window, _cx| {
            let (entity, label) = (entity.clone(), label.clone());
            dialog
                .title(tr!("editor-discard-title"))
                .child(
                    v_flex()
                        .gap_1()
                        .child(div().text_sm().child(label.clone()))
                        .child(div().text_xs().child(tr!("editor-discard-help"))),
                )
                .overlay_closable(false)
                .close_button(false)
                .on_ok(move |_, _window, cx| {
                    entity.update(cx, |this, cx| {
                        this.editing = None;
                        cx.notify();
                    });
                    true
                })
        });
    }

    /// Opens a file in the external editor, at a given line.
    ///
    /// The gesture exists **from a diff line** as much as from the explorer:
    /// that is the real use case — you are reviewing, something is off, you open
    /// it where it is.
    pub(super) fn open_externally(&mut self, path: PathBuf, line: usize, cx: &mut Context<Self>) {
        let Some(worktree) = self.active.clone() else {
            return;
        };
        let editor = Settings::global(cx).external_editor.clone();
        if editor.trim().is_empty() {
            self.announce(tr!("editor-none-configured"), cx);
            return;
        }
        self.git.send(Cmd::OpenExternal {
            worktree,
            path,
            line,
            editor,
        });
        cx.notify();
    }

    /// Opens the diff's file in the external editor, at the selected line.
    pub(super) fn open_diff_externally(&mut self, cx: &mut Context<Self>) {
        let split = Settings::global(cx).diff_split;
        let Some(state) = self.active_review() else {
            return;
        };
        let Some(path) = state.selected.clone() else {
            return;
        };
        let line = state
            .diff
            .as_ref()
            .zip(state.diff_selection)
            .and_then(|(diff, (anchor, head))| {
                let row = if split {
                    diff.unified_span(anchor, head)?.0
                } else {
                    anchor.min(head)
                };
                let crate::ui::diff_view::Row::Line { hunk, line } = diff.rows.get(row).copied()?
                else {
                    return None;
                };
                let source = diff.file.hunks.get(hunk)?.lines.get(line)?;
                source.new_no.or(source.old_no)
            })
            .unwrap_or(1);
        self.open_externally(path, line, cx);
    }

    fn file_op(&mut self, op: files::Op, cx: &mut Context<Self>) {
        let Some(worktree) = self.active.clone() else {
            return;
        };
        self.git.send(Cmd::FileOp { worktree, op });
        // The list is re-read: only `ls-files` knows what git now tracks.
        if let Some(explorer) = self.explorer() {
            explorer.files.clear();
        }
        cx.notify();
    }

    // — The panel  ——————————————————————————————————————————————

    pub(super) fn render_files(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let Some(worktree) = self.active.clone() else {
            return v_flex()
                .size_full()
                .items_center()
                .justify_center()
                .text_color(cx.theme().muted_foreground)
                .child(tr!("no-worktree"))
                .into_any_element();
        };
        self.ensure_project_files(cx);
        let ignored = Settings::global(cx).show_ignored_files;
        let vim = Settings::global(cx).vim_mode;
        let scroll = self.files_scroll.clone();
        let focus = self.explorer_focus.clone();
        let find = self.render_find(crate::ui::find::Pane::Files, cx);
        let query = self.query(crate::ui::find::Pane::Files, cx);
        let bar = self.render_files_bar(&worktree, ignored, cx);
        let Some(explorer) = self.explorers.get_mut(&worktree) else {
            return div().into_any_element();
        };
        if explorer.query != query {
            explorer.query = query;
            explorer.rebuild();
        }
        let explorer = &*explorer;
        let pending = explorer.pending;
        let rows = explorer.rows.clone();
        let files = Rc::new(explorer.files.clone());
        let cursor = explorer.cursor.clone();
        let count = rows.len();
        // The git status is already here: showing it costs only one lookup per
        // visible row, and it is what makes the difference between a file list
        // and a project explorer.
        let status: Rc<std::collections::HashMap<PathBuf, crate::git::StatusCode>> = Rc::new(
            self.review
                .get(&worktree)
                .map(|state| {
                    state
                        .status
                        .files
                        .iter()
                        .map(|file| {
                            let code = if file.is_untracked() {
                                crate::git::StatusCode::Untracked
                            } else if !matches!(file.worktree, crate::git::StatusCode::Unmodified) {
                                file.worktree
                            } else {
                                file.index
                            };
                            (file.path.clone(), code)
                        })
                        .collect()
                })
                .unwrap_or_default(),
        );
        let open = self.editing.as_ref().map(|editing| editing.path.clone());
        let entity = cx.entity();
        let look = Look::of(cx);

        // Nothing to show, and nothing under way: it is an empty project or a
        // search with no result. During the first `ls-files`, the list stays
        // blank — announcing "no file" and then showing them reads as a display
        // glitch.
        if count == 0 && !pending {
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
                        .text_color(look.muted)
                        .child(icon("folder"))
                        .child(div().text_sm().child(tr!("files-empty"))),
                )
                .into_any_element();
        }

        v_flex()
            .size_full()
            .child(bar)
            .children(find)
            .child(
                div()
                    .id("project-tree")
                    // The arrows belong to the tree while it has focus: that is
                    // the context their predicate reads.
                    .key_context(crate::ui::shortcuts::explorer_context(vim))
                    .track_focus(&focus)
                    .flex_1()
                    .min_h_0()
                    .child(
                        self.scrolled(
                            "project-files-bar",
                            &scroll,
                            crate::ui::motion::Axes::Vertical,
                            window,
                            uniform_list("project-files", count, move |visible, _window, cx| {
                                visible
                                    .map(|ix| {
                                        render_row(
                                            &rows,
                                            &files,
                                            ix,
                                            &status,
                                            open.as_deref(),
                                            cursor.as_deref(),
                                            &look,
                                            &entity,
                                            cx,
                                        )
                                    })
                                    .collect::<Vec<_>>()
                            })
                            .size_full()
                            // See `review.rs`: the inset belongs to the list, a
                            // margin on a `uniform_list` entry being ignored.
                            .px_1()
                            .track_scroll(&scroll.clone()),
                            cx,
                        ),
                    ),
            )
            .into_any_element()
    }

    /// The header: the project, what it weighs, and the tree's gestures.
    ///
    /// Three buttons and a menu rather than six buttons: the panel is narrow by
    /// nature — it is a column of file names — and what serves once in a while
    /// has no business taking the room there of what serves on every review.
    fn render_files_bar(
        &mut self,
        worktree: &Path,
        ignored: bool,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let name = worktree
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_else(|| worktree.display().to_string());
        let count = self
            .explorers
            .get(worktree)
            .map(|explorer| explorer.files.len())
            .unwrap_or(0);
        let muted = cx.theme().muted_foreground;
        let entity = cx.entity();

        h_flex()
            .h(crate::ui::theme::bar_height(cx))
            .w_full()
            .px_2()
            .gap_1()
            .items_center()
            .border_b_1()
            .border_color(cx.theme().border)
            .child(icon("folder-open").xsmall().text_color(muted))
            .child(
                div()
                    .flex_1()
                    .truncate()
                    .text_xs()
                    .child(SharedString::from(name)),
            )
            .child(
                div()
                    .text_xs()
                    .text_color(muted)
                    .child(SharedString::from(count.to_string())),
            )
            .child(
                Button::new("files-search")
                    .ghost()
                    .xsmall()
                    .icon(icon("search"))
                    .tooltip(tr!("files-search"))
                    .on_click(cx.listener(|this, _, window, cx| {
                        // The panel becomes the search's target: the button is in
                        // its header, and clicking in it does not necessarily go
                        // through the content.
                        this.touch_pane(crate::ui::find::Pane::Files, cx);
                        this.open_find(window, cx);
                    })),
            )
            .child(
                Button::new("files-reveal")
                    .ghost()
                    .xsmall()
                    .icon(icon("crosshair"))
                    .tooltip(tr!("files-reveal"))
                    .on_click(cx.listener(|this, _, _window, cx| this.reveal_open_file(cx))),
            )
            .child(
                Button::new("files-collapse")
                    .ghost()
                    .xsmall()
                    .icon(icon("chevrons-down-up"))
                    .tooltip(tr!("files-collapse-all"))
                    .on_click(cx.listener(|this, _, _window, cx| this.collapse_project_tree(cx))),
            )
            .child(
                Button::new("files-more")
                    .ghost()
                    .xsmall()
                    .icon(icon("ellipsis"))
                    .tooltip(tr!("files-more"))
                    .dropdown_menu(move |menu, _window, _cx| {
                        let (file, dir, hidden) = (entity.clone(), entity.clone(), entity.clone());
                        menu.item(
                            PopupMenuItem::new(tr!("files-new-file"))
                                .icon(icon("file-plus"))
                                .on_click(move |_, window, cx| {
                                    file.update(cx, |this, cx| {
                                        this.prompt_new_path(None, false, window, cx)
                                    });
                                }),
                        )
                        .item(
                            PopupMenuItem::new(tr!("files-new-dir"))
                                .icon(icon("folder-plus"))
                                .on_click(move |_, window, cx| {
                                    dir.update(cx, |this, cx| {
                                        this.prompt_new_path(None, true, window, cx)
                                    });
                                }),
                        )
                        .separator()
                        .item(
                            PopupMenuItem::new(tr!("files-show-ignored"))
                                .icon(icon("eye"))
                                .icon(icon(if ignored { "eye" } else { "eye-off" }))
                                .on_click(move |_, _window, cx| {
                                    hidden.update(cx, |this, cx| this.toggle_ignored_files(cx));
                                }),
                        )
                    }),
            )
    }

    /// Asks for a path and creates the file or the folder.
    ///
    /// `parent` prefills the field: that is what makes the difference between
    /// "new file" and "new file *here*", the second being the gesture one really
    /// has from a right click on a folder.
    fn prompt_new_path(
        &mut self,
        parent: Option<PathBuf>,
        directory: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let start = parent
            .map(|parent| format!("{}/", parent.display()))
            .unwrap_or_default();
        self.open_text_dialog_with(
            if directory {
                tr!("files-new-dir")
            } else {
                tr!("files-new-file")
            },
            tr!("files-path-placeholder"),
            start,
            window,
            cx,
            move |this, value, _window, cx| {
                let path = PathBuf::from(value.trim());
                if path.as_os_str().is_empty() {
                    return;
                }
                this.file_op(
                    if directory {
                        files::Op::NewDir { path }
                    } else {
                        files::Op::NewFile { path }
                    },
                    cx,
                );
            },
        );
    }

    fn prompt_rename(&mut self, from: PathBuf, window: &mut Window, cx: &mut Context<Self>) {
        self.open_text_dialog(
            tr!("files-rename"),
            SharedString::from(from.display().to_string()),
            window,
            cx,
            move |this, value, _window, cx| {
                let to = PathBuf::from(value.trim());
                if to.as_os_str().is_empty() || to == from {
                    return;
                }
                this.file_op(
                    files::Op::Rename {
                        from: from.clone(),
                        to,
                    },
                    cx,
                );
            },
        );
    }

    /// Confirms before deleting: it is the explorer's only gesture git does not
    /// catch when the file is untracked.
    fn confirm_delete(&mut self, path: PathBuf, window: &mut Window, cx: &mut Context<Self>) {
        let label = SharedString::from(path.display().to_string());
        let entity = cx.entity();
        window.open_dialog(cx, move |dialog, _window, _cx| {
            let (entity, path, label) = (entity.clone(), path.clone(), label.clone());
            dialog
                .title(tr!("delete-title"))
                .child(
                    v_flex()
                        .gap_1()
                        .child(div().text_sm().child(label.clone()))
                        .child(div().text_xs().child(tr!("delete-warning"))),
                )
                .overlay_closable(false)
                .close_button(false)
                .on_ok(move |_, _window, cx| {
                    entity.update(cx, |this, cx| {
                        this.file_op(files::Op::Delete { path: path.clone() }, cx)
                    });
                    true
                })
        });
    }

    // — The editor ———————————————————————————————————————————

    /// The built-in editor, when a file is open in it.
    ///
    /// It takes the diff's place rather than occupying a panel of its own: one
    /// looks at one *or* the other, and two tabs to switch between for a gesture
    /// coming from the explorer would be one round trip too many.
    pub(super) fn render_editor(
        &mut self,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<impl IntoElement> {
        let editing = self.editing.as_ref()?;
        let (path, dirty, input) = (editing.path.clone(), editing.dirty, editing.input.clone());
        let mono = cx.theme().mono_font_family.clone();
        let label = SharedString::from(path.display().to_string());
        let for_external = path.clone();
        Some(
            v_flex()
                .size_full()
                .child(
                    h_flex()
                        .h(crate::ui::theme::bar_height(cx))
                        .w_full()
                        .px_2()
                        .gap_2()
                        .items_center()
                        .border_b_1()
                        .border_color(cx.theme().border)
                        .child(icon("file-text").xsmall())
                        .child(
                            div()
                                .flex_1()
                                .truncate()
                                .text_sm()
                                .font_family(mono)
                                .child(label),
                        )
                        // A badge and not an asterisk in the title: the title is
                        // already a truncated path, and one more character at the
                        // end goes unseen.
                        .when(dirty, |el| {
                            el.child(div().size(px(7.)).rounded_full().bg(cx.theme().warning))
                        })
                        .child(
                            Button::new("editor-external")
                                .ghost()
                                .xsmall()
                                .icon(icon("external-link"))
                                .tooltip(tr!("editor-external"))
                                .on_click(cx.listener(move |this, _, _window, cx| {
                                    this.open_externally(for_external.clone(), 1, cx);
                                })),
                        )
                        .child(
                            Button::new("editor-save")
                                .ghost()
                                .xsmall()
                                .icon(icon("save"))
                                .tooltip(tr!("editor-save"))
                                .on_click(cx.listener(|this, _, _window, cx| this.save_file(cx))),
                        )
                        .child(
                            Button::new("editor-close")
                                .ghost()
                                .xsmall()
                                .icon(icon("x"))
                                .tooltip(tr!("editor-close"))
                                .on_click(
                                    cx.listener(|this, _, window, cx| {
                                        this.close_editor(window, cx)
                                    }),
                                ),
                        ),
                )
                .child(div().flex_1().min_h_0().child(Editor::new(&input).h_full())),
        )
    }
}

/// What does not depend on the row: colours and geometry.
///
/// Read once per frame and not per visible entry — the virtualised list's
/// closure runs for every row on screen, wheel animation included, and
/// `cx.theme()` borrows the context.
struct Look {
    height: Pixels,
    /// A row's background radius. A hovered or open row is a pill laid in the
    /// list, not a band crossing it.
    radius: Pixels,
    muted: gpui::Hsla,
    accent: gpui::Hsla,
    /// The vertical rule of one indentation level.
    guide: gpui::Hsla,
    folder: gpui::Hsla,
}

impl Look {
    fn of(cx: &gpui::App) -> Self {
        Self {
            height: crate::ui::theme::row_height(cx),
            radius: cx.theme().radius,
            muted: cx.theme().muted_foreground,
            accent: cx.theme().accent,
            // Pale enough to read as a texture and not as a separator: these
            // rules are on screen by the dozen.
            guide: cx.theme().border.opacity(0.7),
            folder: cx.theme().muted_foreground,
        }
    }
}

/// The width of one indentation level, and of the rule marking it.
const INDENT: f32 = 12.;

/// The vertical rules of the parent levels.
///
/// It is what makes a deep tree readable: without them, at six levels of
/// indentation — the common case on a Laravel project — nothing says any more
/// which folder a row belongs to.
fn indent_guides(depth: usize, look: &Look) -> impl IntoIterator<Item = gpui::Div> + use<> {
    let guide = look.guide;
    (0..depth).map(move |_| {
        div()
            .w(px(INDENT))
            .h_full()
            .flex_none()
            .border_l_1()
            .border_color(guide)
    })
}

/// One row of the explorer: a collapsible folder or a file.
#[allow(clippy::too_many_arguments)]
fn render_row(
    rows: &Rc<Vec<tree::Entry>>,
    files: &Rc<Vec<PathBuf>>,
    index: usize,
    status: &Rc<std::collections::HashMap<PathBuf, crate::git::StatusCode>>,
    open: Option<&Path>,
    cursor: Option<&Path>,
    look: &Look,
    entity: &Entity<ClaudhubApp>,
    cx: &mut gpui::App,
) -> gpui::AnyElement {
    let Some(entry) = rows.get(index) else {
        return div().into_any_element();
    };
    match entry {
        tree::Entry::Dir {
            path,
            label,
            depth,
            collapsed,
            ..
        } => {
            let at_cursor = cursor == Some(path.as_path());
            let (path, entity) = (path.clone(), entity.clone());
            let (for_click, for_menu) = (path.clone(), path.clone());
            let click = entity.clone();
            h_flex()
                .id(("dir", index))
                .h(look.height)
                .rounded(look.radius)
                .pl_1()
                .pr_2()
                .items_center()
                .cursor_pointer()
                .when(at_cursor, |el| el.bg(look.accent.opacity(0.5)))
                .hover(|s| s.bg(look.accent.opacity(0.4)))
                .on_click(move |_, window, cx| {
                    click.update(cx, |this, cx| {
                        this.focus_project_tree(for_click.clone(), window, cx);
                        this.toggle_project_dir(for_click.clone(), cx);
                    });
                })
                .children(indent_guides(*depth, look))
                .child(
                    icon(if *collapsed {
                        "chevron-right"
                    } else {
                        "chevron-down"
                    })
                    .xsmall()
                    .text_color(look.muted),
                )
                // The folder carries its own glyph, open or closed: the chevron
                // says the state of the collapse, the icon says one is looking at
                // a folder — that is what tells a tree from an indented list.
                .child(
                    icon(if *collapsed { "folder" } else { "folder-open" })
                        .xsmall()
                        .text_color(look.folder),
                )
                .child(
                    div()
                        .pl_1()
                        .flex_1()
                        .truncate()
                        .text_sm()
                        .child(SharedString::from(label.clone())),
                )
                .context_menu(move |menu, _window, _cx| dir_menu(menu, &entity, &for_menu))
                .into_any_element()
        }
        tree::Entry::Leaf { index: leaf, depth } => {
            let Some(path) = files.get(*leaf).cloned() else {
                return div().into_any_element();
            };
            let name = path
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default();
            let code = status.get(&path).copied();
            let is_open = open == Some(path.as_path());
            let at_cursor = cursor == Some(path.as_path());
            let (for_open, for_menu) = (path.clone(), path.clone());
            let (open_entity, menu_entity) = (entity.clone(), entity.clone());
            h_flex()
                .id(("file", index))
                .h(look.height)
                .rounded(look.radius)
                .pl_1()
                .pr_2()
                .items_center()
                .cursor_pointer()
                // Open and under the cursor are two things: one browses the tree
                // with the keyboard without leaving the file being reviewed, and
                // showing only one of the two would lose the other.
                .when(is_open, |el| el.bg(look.accent))
                .when(at_cursor && !is_open, |el| el.bg(look.accent.opacity(0.5)))
                .hover(|s| s.bg(look.accent.opacity(0.4)))
                .on_click(move |_, window, cx| {
                    open_entity.update(cx, |this, cx| {
                        this.focus_project_tree(for_open.clone(), window, cx);
                        this.open_in_editor(for_open.clone(), cx);
                    });
                })
                .children(indent_guides(*depth, look))
                // The place of the chevron a file does not have: without it, file
                // names and folder names do not line up.
                .child(div().w(px(14.)).flex_none())
                .child(crate::ui::file_icons::file_icon(&path, cx))
                .child(
                    div()
                        .pl_1()
                        .flex_1()
                        .truncate()
                        .text_sm()
                        .when_some(code, |el, code| el.text_color(status_color(code, cx)))
                        .child(SharedString::from(name)),
                )
                .when_some(code, |el, code| {
                    el.child(
                        div()
                            .text_xs()
                            .text_color(look.muted)
                            .child(SharedString::new_static(code.letter())),
                    )
                })
                .context_menu(move |menu, _window, _cx| file_menu(menu, &menu_entity, &for_menu))
                .into_any_element()
        }
    }
}

/// A folder's menu: create inside it, and unfold or collapse it wholesale.
fn dir_menu(
    menu: gpui_component::menu::PopupMenu,
    entity: &Entity<ClaudhubApp>,
    path: &Path,
) -> gpui_component::menu::PopupMenu {
    let (new_file, new_dir) = (entity.clone(), entity.clone());
    let (expand, collapse, copy) = (entity.clone(), entity.clone(), entity.clone());
    let (p1, p2, p3, p4, p5) = (
        path.to_path_buf(),
        path.to_path_buf(),
        path.to_path_buf(),
        path.to_path_buf(),
        path.to_path_buf(),
    );
    menu.item(
        PopupMenuItem::new(tr!("files-new-here"))
            .icon(icon("file-plus"))
            .on_click(move |_, window, cx| {
                new_file.update(cx, |this, cx| {
                    this.prompt_new_path(Some(p1.clone()), false, window, cx)
                });
            }),
    )
    .item(
        PopupMenuItem::new(tr!("files-new-dir-here"))
            .icon(icon("folder-plus"))
            .on_click(move |_, window, cx| {
                new_dir.update(cx, |this, cx| {
                    this.prompt_new_path(Some(p2.clone()), true, window, cx)
                });
            }),
    )
    .separator()
    .item(
        PopupMenuItem::new(tr!("files-expand-under"))
            .icon(icon("chevrons-up-down"))
            .on_click(move |_, _window, cx| {
                expand.update(cx, |this, cx| this.expand_project_dir(p3.clone(), cx));
            }),
    )
    .item(
        PopupMenuItem::new(tr!("files-collapse-under"))
            .icon(icon("chevrons-down-up"))
            .on_click(move |_, _window, cx| {
                collapse.update(cx, |this, cx| this.collapse_project_dir(p4.clone(), cx));
            }),
    )
    .separator()
    .item(
        PopupMenuItem::new(tr!("action-copy-path"))
            .icon(icon("copy"))
            .on_click(move |_, _window, cx| {
                copy.update(cx, |this, cx| this.copy_project_path(&p5, false, cx));
            }),
    )
}

/// A file's menu.
fn file_menu(
    menu: gpui_component::menu::PopupMenu,
    entity: &Entity<ClaudhubApp>,
    path: &Path,
) -> gpui_component::menu::PopupMenu {
    let (external, copy, absolute) = (entity.clone(), entity.clone(), entity.clone());
    let (new_file, rename, delete) = (entity.clone(), entity.clone(), entity.clone());
    let parent = path.parent().map(Path::to_path_buf).unwrap_or_default();
    let (p1, p2, p3, p4, p5) = (
        path.to_path_buf(),
        path.to_path_buf(),
        path.to_path_buf(),
        path.to_path_buf(),
        path.to_path_buf(),
    );
    menu.item(
        PopupMenuItem::new(tr!("editor-external"))
            .icon(icon("external-link"))
            .on_click(move |_, _window, cx| {
                external.update(cx, |this, cx| this.open_externally(p1.clone(), 1, cx));
            }),
    )
    .separator()
    .item(
        PopupMenuItem::new(tr!("action-copy-path"))
            .icon(icon("copy"))
            .on_click(move |_, _window, cx| {
                copy.update(cx, |this, cx| this.copy_project_path(&p2, false, cx));
            }),
    )
    .item(
        PopupMenuItem::new(tr!("files-copy-absolute"))
            .icon(icon("copy"))
            .on_click(move |_, _window, cx| {
                absolute.update(cx, |this, cx| this.copy_project_path(&p3, true, cx));
            }),
    )
    .separator()
    // "Here" means in the file's folder: one right-clicks a neighbour of what is
    // to be created, never the folder itself when the list already shows its
    // contents.
    .item(
        PopupMenuItem::new(tr!("files-new-here"))
            .icon(icon("file-plus"))
            .on_click(move |_, window, cx| {
                new_file.update(cx, |this, cx| {
                    this.prompt_new_path(Some(parent.clone()), false, window, cx)
                });
            }),
    )
    .item(
        PopupMenuItem::new(tr!("files-rename"))
            .icon(icon("pencil"))
            .on_click(move |_, window, cx| {
                rename.update(cx, |this, cx| this.prompt_rename(p4.clone(), window, cx));
            }),
    )
    .item(
        PopupMenuItem::new(tr!("files-delete"))
            .icon(icon("trash-2"))
            .on_click(move |_, window, cx| {
                delete.update(cx, |this, cx| this.confirm_delete(p5.clone(), window, cx));
            }),
    )
}
