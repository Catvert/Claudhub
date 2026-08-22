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

use gpui::{div, prelude::*, px, uniform_list, App, Context, Entity, Pixels, SharedString, Window};
use gpui_component::{
    button::{Button, ButtonVariants},
    h_flex,
    input::{Editor, EditorState},
    menu::{ContextMenuExt, DropdownMenu, PopupMenuItem},
    v_flex, ActiveTheme, Disableable, Sizable, WindowExt,
};

use crate::files;
use crate::runtime::Cmd;
use crate::tr;
use crate::ui::app::ClaudhubApp;
use crate::ui::icons::icon;
use crate::ui::settings::Settings;
use crate::ui::theme::status_color;
use crate::ui::tree;

/// Where the reading of a worktree's file list stands.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Listing {
    /// Never asked for, or something has invalidated it.
    Idle,
    /// A command has gone out and has not come back.
    Loading,
    Ready,
    /// git refused. Not asked again by itself: the panel renders at every
    /// frame, and retrying there would be a git command per frame for as long
    /// as the cause lasts.
    Failed,
}

/// A worktree's files, and the tree drawn from them.
pub struct Explorer {
    /// The flat list, as git returns it: it is the reference, and the tree is
    /// only a display of it.
    pub files: Vec<PathBuf>,
    /// The displayed tree, rebuilt on every collapse and never in a render.
    pub rows: Rc<Vec<tree::Entry>>,
    /// The folders the user opened.
    ///
    /// Opened and not collapsed, unlike the review list: this tree is the
    /// whole worktree, and a Laravel project unfolded is forty thousand rows
    /// that say nothing. It therefore starts shut, and what is recorded is the
    /// exception — a set seeded with every directory would be the size of the
    /// tree, rebuilt on every fold. See `tree::Folds`.
    pub expanded: std::collections::HashSet<PathBuf>,
    /// Where the reading of the list stands.
    ///
    /// Four states and not a `pending` flag, for the reason the database tree
    /// already had them: "not asked for yet", "under way", "here" and "it
    /// failed" are four different things, and the panel renders at every frame.
    /// Confusing "under way" with "not asked for" restarts `ls-files` sixty
    /// times a second; confusing "here" with "not asked for" does the same on
    /// an empty project; and dropping "it failed" on the floor left the panel
    /// stuck on "under way" for good — one `ls-files` that fails, and the tree
    /// never came back for the rest of the session.
    pub state: Listing,
    /// Were the ignored files asked for, so we know when to re-read.
    pub ignored: bool,
    /// The subset of `files` that `.gitignore` excludes, sorted as git gave it.
    ///
    /// Kept apart and searched, rather than folded into `files` as a pair: it
    /// is empty on a project that ignores nothing, and the rows it dims are
    /// worked out once per rebuild, never in a render.
    pub ignored_files: Vec<PathBuf>,
    /// One flag per row of `rows`, saying whether it is dimmed.
    ///
    /// Computed with the tree and not at paint time: a leaf costs a binary
    /// search, but a directory is dimmed only if **everything** under it is
    /// ignored, and `vendor/` carries thirty thousand leaves. That is a price
    /// a gesture can pay and a frame cannot.
    pub dimmed: Rc<Vec<bool>>,
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
            expanded: std::collections::HashSet::new(),
            state: Listing::Idle,
            ignored: false,
            ignored_files: Vec::new(),
            dimmed: Rc::new(Vec::new()),
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
        // During a search everything is open, whatever was folded.
        let all = std::collections::HashSet::new();
        let folds = if keep.is_some() {
            tree::Folds::OpenBut(&all)
        } else {
            tree::Folds::ShutBut(&self.expanded)
        };
        let rows = tree::build_subset(&self.files, keep.as_deref(), folds);
        self.dimmed = Rc::new(rows.iter().map(|entry| self.is_ignored(entry)).collect());
        self.rows = Rc::new(rows);
    }

    /// Is this row one that `.gitignore` leaves out?
    ///
    /// A directory counts as ignored only when everything under it is —
    /// `vendor/` is, `app/` with one ignored log file in it is not. Anything
    /// else would grey out a folder holding code one is looking for.
    fn is_ignored(&self, entry: &tree::Entry) -> bool {
        if self.ignored_files.is_empty() {
            return false;
        }
        let excluded = |path: &PathBuf| self.ignored_files.binary_search(path).is_ok();
        match entry {
            tree::Entry::Leaf { index, .. } => self.files.get(*index).is_some_and(excluded),
            tree::Entry::Dir { leaves, .. } => {
                !leaves.is_empty()
                    && leaves
                        .iter()
                        .all(|index| self.files.get(*index).is_some_and(excluded))
            }
        }
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
    /// Naming each ancestor is enough, merged chains included:
    /// `app/Http/Livewire/Forms` fits on one line but is still an ancestor of
    /// the file it contains. The ancestors that are not rows of their own cost
    /// an entry in the set and nothing else.
    fn reveal(&mut self, path: &Path) {
        let mut changed = false;
        for ancestor in path.ancestors().skip(1) {
            if !ancestor.as_os_str().is_empty() {
                changed |= self.expanded.insert(ancestor.to_path_buf());
            }
        }
        if changed {
            self.rebuild();
        }
    }

    /// Collapses everything, which is the state the tree opens in.
    fn collapse_all(&mut self) {
        self.expanded.clear();
        self.rebuild();
    }

    /// Unfolds a whole subtree, its root included.
    fn expand_under(&mut self, root: &Path) {
        // Every folder under the root, and not only the visible ones: what a
        // closed folder hides has to be open too once it is reopened.
        for path in &self.files {
            if !path.starts_with(root) {
                continue;
            }
            for ancestor in path.ancestors().skip(1) {
                if ancestor.starts_with(root) {
                    self.expanded.insert(ancestor.to_path_buf());
                }
            }
        }
        self.expanded.insert(root.to_path_buf());
        self.rebuild();
    }

    /// Collapses a whole subtree, its root included.
    fn collapse_under(&mut self, root: &Path) {
        self.expanded
            .retain(|path| !path.starts_with(root) && path != root);
        self.rebuild();
    }
}

/// A file open in the built-in editor.
/// The key of the editor's wheel smoothing, as `ui::scroll` keys the others.
const EDITOR_SCROLL: &str = "editor-scroll";

/// Where a caret must come to rest.
///
/// Two shapes because the two callers hold two different things: a step through
/// the trail knows a byte offset — it is what the editor handed out — and a
/// language server answers in lines and UTF-16 columns. Converting the second
/// needs the text, which for another file does not exist yet.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Landing {
    Offset(usize),
    Position { line: u32, character: u32 },
}

/// A file being read, and what must happen to the caret when it arrives.
/// Where a line should sit once the view has moved.
#[derive(Clone, Copy)]
enum Place {
    /// The least scrolling that brings it into view, and none at all when it is
    /// already there. What a motion of one line wants.
    Nearest,
    /// What `zz`, `zt` and `zb` name — and where a jump lands, which is
    /// `Centre`: arriving at a symbol on the last line of the panel shows the
    /// code that leads to it and none of what follows, and reading a definition
    /// is reading what is around it.
    Asked(crate::ui::vim::Reveal),
}

/// The line a byte offset falls on, counted from zero.
fn line_at(text: &str, offset: usize) -> usize {
    text[..offset.min(text.len())]
        .bytes()
        .filter(|byte| *byte == b'\n')
        .count()
}

/// A landing resolved against the text it lands in.
///
/// The offset is clamped: a trail entry is a byte offset in a file that may
/// have been rewritten while one was away, and a server's position may name a
/// line the document no longer has.
fn offset_of(text: &gpui_component::input::Rope, landing: &Landing) -> usize {
    match landing {
        Landing::Offset(offset) => (*offset).min(text.len()),
        Landing::Position { line, character } => {
            use gpui_component::input::RopeExt;
            text.position_to_offset(&lsp_types::Position::new(*line, *character))
        }
    }
}

pub struct Pending {
    pub worktree: PathBuf,
    pub path: PathBuf,
    /// Where the caret goes, or `None` to leave it where the editor puts it:
    /// opening a file from the explorer is a jump to a file, not to a place.
    pub landing: Option<Landing>,
    /// Where the jump came from, when it is one. A step through the trail
    /// leaves this empty: recording it would put back the place one has just
    /// stepped away from, and the trail would never end.
    pub from: Option<crate::ui::jumps::Spot>,
}

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
    /// The modal state, when vim keys are on. One per open file: leaving a file
    /// in insert mode and coming back to another in normal mode is what a tabbed
    /// vim does anyway.
    pub vim: crate::ui::vim::Vim,
    /// The layer a yank lights up on, created once with the file: a collection
    /// follows the text through its edits, and asking for a new one per yank
    /// would leave the old ones stacked up for as long as the file is open.
    pub flash: gpui_component::input::TextDecorationCollection,
    /// What puts the light out, held so that it can be **dropped**: a second
    /// yank replaces the task, and dropping a gpui task cancels it — without
    /// that, the first timer would darken the second yank.
    pub flash_timer: Option<gpui::Task<()>>,
    /// The layer the block cursor is painted on, created once with the file like
    /// the flash's.
    ///
    /// It is **ours** and not the editor's selection, for two reasons that are
    /// the same reason: the selection is only ever written by a keystroke, so
    /// there was no cursor at all until the first key was pressed and none again
    /// after a click of the mouse; and its colour is the theme's `selection`,
    /// which is a few percent of lightness away from the background — a block
    /// one has to look for is a block one does not see.
    pub cursor: gpui_component::input::TextDecorationCollection,
    /// A caret waiting for the editor to be measured before it can be revealed.
    ///
    /// A file opened by a jump installs a brand-new `EditorState`, which has
    /// never been laid out: it has neither a visible row range nor a line
    /// height, so scrolling to the caret is a division by nothing and returns
    /// silently. The caret was in the right place and the view stayed at the
    /// top of the file, which reads as "the jump missed". The reveal is
    /// therefore kept until a frame can measure — the same answer as the diff's
    /// first width, and bounded for the same reason.
    pub reveal_at: Option<usize>,
    pub reveal_tries: u8,
    /// Where `zm` and `zr` have got to: the nesting level below which folds are
    /// closed. `None` is everything open, which is one past the deepest — the
    /// state `zR` puts the file back into, and the one it opens in.
    pub fold_level: Option<usize>,
    /// The mode, caret and text length the cursor was last painted for.
    ///
    /// The block is recomputed at a frame only when one of the three has moved:
    /// `value()` copies the whole file, and this runs at every frame.
    pub cursor_at: Option<(crate::ui::vim::Mode, usize, usize, bool)>,
    /// What is on screen differs from what is on disk.
    pub dirty: bool,
    /// A change is already on its way to the language server.
    ///
    /// A keystroke emits a change event, and sending each one is fifty round
    /// trips for a state nobody asked about in between: the flag is what makes
    /// the debounce read the latest text once rather than every text in turn.
    pub lsp_pending: bool,
}

impl ClaudhubApp {
    // — The tree ————————————————————————————————————————————————

    fn explorer(&mut self) -> Option<&mut Explorer> {
        let worktree = self.active.clone()?;
        Some(self.explorers.entry(worktree).or_default())
    }

    /// The file open in the displayed worktree's editor, if there is one.
    ///
    /// There is one editor per worktree, as there is one set of terminals: what
    /// is on screen is what belongs to the tree the rest of the window shows.
    pub(super) fn editing(&self) -> Option<&Editing> {
        self.editings.get(self.active.as_ref()?)
    }

    pub(super) fn editing_mut(&mut self) -> Option<&mut Editing> {
        let worktree = self.active.clone()?;
        self.editings.get_mut(&worktree)
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
        match explorer.state {
            // Waiting for an answer: asking again would only queue a second
            // command behind the first.
            Listing::Loading => return,
            // Already answered for the setting in force. A failure is not
            // retried either, until something invalidates it — a toggle, a file
            // operation, a refresh.
            Listing::Ready | Listing::Failed if explorer.ignored == ignored => return,
            _ => {}
        }
        explorer.state = Listing::Loading;
        explorer.ignored = ignored;
        self.git.send(Cmd::ListFiles { worktree, ignored });
    }

    pub(super) fn project_files_arrived(
        &mut self,
        worktree: PathBuf,
        files: Vec<PathBuf>,
        ignored: Vec<PathBuf>,
    ) {
        let explorer = self.explorers.entry(worktree).or_default();
        explorer.state = Listing::Ready;
        explorer.files = files;
        explorer.ignored_files = ignored;
        explorer.rebuild();
    }

    /// git refused to list the files.
    ///
    /// Only what was under way: a failure that names this worktree while
    /// nothing was expected of it — another read, another panel — has nothing
    /// to say about the tree.
    pub(super) fn project_files_failed(&mut self, worktree: &Path) {
        if let Some(explorer) = self.explorers.get_mut(worktree) {
            if explorer.state == Listing::Loading {
                explorer.state = Listing::Failed;
            }
        }
    }

    pub(super) fn toggle_project_dir(&mut self, path: PathBuf, cx: &mut Context<Self>) {
        let Some(explorer) = self.explorer() else {
            return;
        };
        if !explorer.expanded.remove(&path) {
            explorer.expanded.insert(path);
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
            let is_collapsed = !explorer.expanded.contains(&path);
            if open == is_collapsed {
                if open {
                    explorer.expanded.insert(path);
                } else {
                    explorer.expanded.remove(&path);
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
            .editing()
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
            explorer.state = Listing::Idle;
            explorer.files.clear();
            explorer.rebuild();
        }
        cx.notify();
    }

    // — Reading and writing ———————————————————————————————————

    /// Opens a file in the built-in editor.
    pub(super) fn open_in_editor(&mut self, path: PathBuf, cx: &mut Context<Self>) {
        self.open_at(path, None, cx);
    }

    /// The same, with a place in the file to come to rest at.
    ///
    /// This is the one funnel every opening goes through — the explorer, a diff
    /// line, a Sentry frame, a definition — and therefore the one place the
    /// trail is written. Restoring the previous session does not come here: it
    /// asks for its file directly, which is right, since coming back to where
    /// one was is not a jump one should be able to undo.
    pub(super) fn open_at(
        &mut self,
        path: PathBuf,
        landing: Option<Landing>,
        cx: &mut Context<Self>,
    ) {
        let Some(worktree) = self.active.clone() else {
            return;
        };
        // Reopening the file already open is not a jump: it would put the same
        // place on the trail twice and make one step back do nothing visible.
        let from = self.here(cx).filter(|spot| spot.path != path);
        self.landing = Some(Pending {
            worktree: worktree.clone(),
            path: path.clone(),
            landing,
            from,
        });
        self.git.send(Cmd::ReadFile { worktree, path });
        cx.notify();
    }

    /// Where the caret stands, for the trail to remember.
    pub(super) fn here(&self, cx: &App) -> Option<crate::ui::jumps::Spot> {
        let editing = self.editing()?;
        let offset = editing.input.read(cx).selected_range().start;
        Some(crate::ui::jumps::Spot::new(editing.path.clone(), offset))
    }

    /// Goes to a place and writes the step down.
    pub(super) fn jump_to(
        &mut self,
        path: PathBuf,
        landing: Landing,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(worktree) = self.active.clone() else {
            return;
        };
        let same = self.editing().is_some_and(|editing| editing.path == path);
        if !same {
            self.open_at(path, Some(landing), cx);
            return;
        }
        // Same file: there is nothing to read, so the trail is written here
        // rather than on a content that will not arrive.
        let Some(from) = self.here(cx) else {
            return;
        };
        let Some(offset) = self.land(&landing, window, cx) else {
            return;
        };
        if offset != from.offset {
            self.jumps
                .entry(worktree)
                .or_default()
                .jump(from, crate::ui::jumps::Spot::new(path, offset));
        }
        cx.notify();
    }

    pub(super) fn jump_back(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.step(true, window, cx);
    }

    pub(super) fn jump_forward(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.step(false, window, cx);
    }

    fn step(&mut self, back: bool, window: &mut Window, cx: &mut Context<Self>) {
        let Some(worktree) = self.active.clone() else {
            return;
        };
        let Some(here) = self.here(cx) else {
            return;
        };
        let Some(trail) = self.jumps.get_mut(&worktree) else {
            return;
        };
        let Some(spot) = (if back {
            trail.back(here)
        } else {
            trail.forward(here)
        }) else {
            return;
        };
        if self
            .editing()
            .is_some_and(|editing| editing.path == spot.path)
        {
            self.land(&Landing::Offset(spot.offset), window, cx);
        } else {
            // No origin: the step is already written in the trail, and putting
            // it back would be a place one could go back from for ever.
            self.landing = Some(Pending {
                worktree: worktree.clone(),
                path: spot.path.clone(),
                landing: Some(Landing::Offset(spot.offset)),
                from: None,
            });
            self.git.send(Cmd::ReadFile {
                worktree,
                path: spot.path,
            });
        }
        cx.notify();
    }

    /// Brings the caret to rest, and the view with it. Gives back the offset it
    /// settled on, which is what the trail records.
    ///
    /// **And gives the editor the focus.** A jump is a keyboard gesture whose
    /// next keystroke is another one — `Ctrl+O` to come straight back, a motion
    /// to read around where one landed — and a caret in a field nobody is
    /// typing into answers none of them. Crossing into another file builds a
    /// new `EditorState`, which starts unfocused; `enter_workspace` only
    /// focuses when the screen actually changes, so a jump made from the
    /// editing screen focused nothing at all. Opening a file from the tree does
    /// **not** come through here: browsing with the arrows must leave them to
    /// the tree.
    fn land(
        &mut self,
        landing: &Landing,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<usize> {
        let input = self.editing()?.input.clone();
        let offset = offset_of(input.read(cx).text(), landing);
        input.update(cx, |state, cx| {
            state.set_selected_range(offset..offset, cx);
        });
        let centred = Place::Asked(crate::ui::vim::Reveal::Centre);
        if !self.scroll_to_line(&input, offset, centred, cx) {
            if let Some(editing) = self.editing_mut() {
                editing.reveal_at = Some(offset);
                editing.reveal_tries = 0;
            }
        }
        gpui::Focusable::focus_handle(&input, cx).focus(window, cx);
        Some(offset)
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
        let restored = self.take_restored_editing(&worktree, &path);
        let language = crate::ui::highlight::language_for_path(&path).unwrap_or("text");
        let input = cx.new(|cx| {
            // `EditorState` and not `InputState`: the input rework split the
            // three modes into three types — one line, multi-line text, code.
            // The code features (language, line numbers, LSP) only exist on the
            // third.
            let mut state = EditorState::new(window, cx)
                .language(language)
                .line_number(true)
                .default_value(content.text);
            // A Blade view gets a highlighter of ours: the language it declares
            // is PHP, whose grammar reads a `@php` block and every directive as
            // plain HTML text. See `ui::blade::BladeHighlighter`. The default
            // adapter is installed by `ensure_`, which leaves ours in place.
            if crate::ui::blade::is_blade(&path) {
                state.set_highlighter_factory(crate::ui::blade::input_highlighter_factory(), cx);
            }
            state
        });
        // The subscription is set up here, once per opened file: it is what
        // lights the unsaved-change indicator.
        // The worktree is captured rather than read off the selection: an event
        // arriving after one has switched worktrees would otherwise mark the
        // wrong file unsaved.
        let owner = worktree.clone();
        cx.subscribe(&input, move |this, _, event, cx| {
            if !matches!(event, gpui_component::input::InputEvent::Change) {
                return;
            }
            if let Some(editing) = this.editings.get_mut(&owner) {
                editing.dirty = true;
            }
            this.lsp_editor_changed(&owner, cx);
            cx.notify();
        })
        .detach();
        // The server must forget the file we are leaving, or it goes on
        // answering about a text nobody holds any more. Only this worktree's:
        // the others keep their document open, as they keep their editor.
        if let Some(previous) = self.editings.remove(&worktree) {
            self.lsp_editor_closed(previous.worktree, previous.path);
        }
        // The cursor's layer is created **first**, and that is what decides who
        // wins where the two overlap: collections are composed in creation
        // order, the first one keeping its properties. The block stays visible
        // through a yank's flash, which is the right way round — the flash says
        // what was taken, the block says where one is.
        let (cursor, flash) = input.update(cx, |state, cx| {
            (
                state.create_decorations_collection(Vec::new(), cx),
                state.create_decorations_collection(Vec::new(), cx),
            )
        });
        let opened = (worktree.clone(), path.clone());
        self.editings.insert(
            worktree.clone(),
            Editing {
                worktree,
                path,
                input,
                hash: content.hash,
                dirty: false,
                lsp_pending: false,
                reveal_at: None,
                reveal_tries: 0,
                fold_level: None,
                vim: crate::ui::vim::Vim::default(),
                flash,
                flash_timer: None,
                cursor,
                cursor_at: None,
            },
        );
        // Starts the server this file's language asks for, opens the document
        // and posts the providers — all of it a no-op when the button is off.
        self.lsp_sync_editor(window, cx);
        // The caret the opening was asked for, and the step in the trail that
        // goes with it. A pending that names another file is stale — one asked
        // for a second file before the first arrived — and is dropped, not put
        // back: what it pointed at is not what is on screen.
        if let Some(pending) = self.landing.take() {
            if (pending.worktree.as_path(), pending.path.as_path()) == (&*opened.0, &*opened.1) {
                let offset = pending
                    .landing
                    .and_then(|landing| self.land(&landing, window, cx))
                    .unwrap_or(0);
                if let Some(from) = pending.from {
                    self.jumps
                        .entry(opened.0.clone())
                        .or_default()
                        .jump(from, crate::ui::jumps::Spot::new(opened.1.clone(), offset));
                }
            }
        }
        // A file that opens calls up the screen it is edited on. The gesture
        // comes from the explorer — so from that screen most of the time — but
        // also from a diff line, and answering it silently on the screen next
        // door would be an opened file nobody sees.
        //
        // Unless it is the previous session being put back: there is no gesture
        // then, and the screen that comes back is the one `layout.json` carries.
        if !restored {
            self.enter_workspace(crate::ui::workspace::Workspace::Files, window, cx);
            self.set_panel_visible(crate::ui::panels::EditorPanel::NAME, true, cx);
        }
        self.persist_session(cx);
        cx.notify();
    }

    pub(super) fn save_file(&mut self, cx: &mut Context<Self>) {
        let Some(editing) = self.editing() else {
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
        if let Some(editing) = self.editing_mut() {
            editing.hash = files::digest(&content);
            editing.dirty = false;
        }
        // A server that runs a formatter or an external analyser on save — as
        // PHPantom does with PHPStan — has no other way of knowing.
        self.lsp_editor_saved();
        cx.notify();
    }

    /// Closes the editor, asking for confirmation if the file has changed.
    pub(super) fn close_editor(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(editing) = self.editing() else {
            return;
        };
        // The worktree the gesture was made in, and not the one selected when
        // the dialog is answered: one browses while a question is open.
        let worktree = editing.worktree.clone();
        if !editing.dirty {
            if let Some(previous) = self.editings.remove(&worktree) {
                self.lsp_editor_closed(previous.worktree, previous.path);
            }
            self.persist_session(cx);
            cx.notify();
            return;
        }
        let label = SharedString::from(editing.path.display().to_string());
        let entity = cx.entity();
        window.open_dialog(cx, move |dialog, _window, _cx| {
            let (entity, label, worktree) = (entity.clone(), label.clone(), worktree.clone());
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
                .footer(super::dialogs::confirm())
                .on_ok(move |_, _window, cx| {
                    entity.update(cx, |this, cx| {
                        if let Some(previous) = this.editings.remove(&worktree) {
                            this.lsp_editor_closed(previous.worktree, previous.path);
                        }
                        this.persist_session(cx);
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
            explorer.state = Listing::Idle;
        }
        cx.notify();
    }

    // — The vim keys ————————————————————————————————————————————

    /// One keystroke, when vim keys are on and the built-in editor has the
    /// focus.
    ///
    /// It is listened for in the **capture** phase, on an ancestor of the
    /// editor, and that placement is the whole mechanism. A key listener runs
    /// *after* the bindings have had their turn — which is what leaves `Ctrl+S`
    /// and `Alt+2` to the window — but *before* the platform hands the character
    /// to the focused input handler. Consuming the event is therefore what keeps
    /// a bare `d` from being typed into the file; letting it through is what
    /// makes insert mode an ordinary editor again.
    fn vim_key(&mut self, event: &gpui::KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        use crate::ui::vim::{Command, Response};

        if !Settings::global(cx).vim_mode || self.editing().is_none() {
            return;
        }
        let keystroke = &event.keystroke;
        let modifiers = keystroke.modifiers;
        // Alt belongs to the window — it is how one changes screen — and a
        // function key has nothing vim wants.
        if modifiers.alt || modifiers.function {
            return;
        }
        let ctrl = modifiers.control || modifiers.platform;
        // The character the keystroke **produced**, and not the key it was
        // pressed on: that is what puts `$`, `^` and `0` where they belong on an
        // AZERTY keyboard, where they are shifted or in the digit row.
        let ch = keystroke
            .key_char
            .as_deref()
            .filter(|_| !ctrl)
            .and_then(|typed| {
                let mut chars = typed.chars();
                let ch = chars.next()?;
                (chars.next().is_none() && !ch.is_control()).then_some(ch)
            });
        let key = crate::ui::vim::Key {
            ch,
            name: keystroke.key.clone(),
            ctrl,
        };

        let Some(input) = self.editing().map(|editing| editing.input.clone()) else {
            return;
        };
        let (text, cursor, rows) = {
            let state = input.read(cx);
            let rows = state
                .visible_row_range()
                .map(|rows| rows.len())
                .unwrap_or(DEFAULT_ROWS)
                .max(2);
            (
                state.value().to_string(),
                state.selected_range().start,
                rows,
            )
        };
        // The clipboard is read **when a paste is about to happen**, and not at
        // every keystroke: a read is a round trip to the display server, and
        // pasting is the only gesture that consumes a register.
        let clipboard = Settings::global(cx).vim_clipboard;
        let pasting = clipboard && matches!(key.ch, Some('p') | Some('P'));
        let pasted = pasting
            .then(|| cx.read_from_clipboard().and_then(|item| item.text()))
            .flatten();
        let Some(editing) = self.editing_mut() else {
            return;
        };
        if let Some(text) = pasted {
            editing.vim.set_register(text);
        }
        let response = editing.vim.press(&key, &text, cursor, rows);
        if matches!(response, Response::Ignored) {
            return;
        }
        // Everything else is ours: the key must not reach the file.
        cx.stop_propagation();
        match response {
            Response::Ignored | Response::Consumed => {}
            Response::Apply(change) => {
                if let Some(yank) = change.yank.filter(|_| clipboard) {
                    cx.write_to_clipboard(gpui::ClipboardItem::new_string(yank.text));
                }
                if let Some(range) = change.flash {
                    self.flash_yank(range, cx);
                }
                input.update(cx, |state, cx| {
                    if let Some(edit) = change.edit {
                        state.set_selected_range(edit.range, cx);
                        state.replace(edit.text, window, cx);
                    }
                    state.set_selected_range(change.selection, cx);
                });
                self.scroll_to_line(&input, change.head, Place::Nearest, cx);
            }
            Response::Command(command) => match command {
                // Undo and redo belong to the editor, which is the only one that
                // knows what the last transaction was.
                Command::Undo => window.dispatch_action(Box::new(gpui_component::input::Undo), cx),
                Command::Redo => window.dispatch_action(Box::new(gpui_component::input::Redo), cx),
                Command::Save => self.save_file(cx),
                Command::Close => self.close_editor(window, cx),
                Command::SaveAndClose => {
                    self.save_file(cx);
                    self.close_editor(window, cx);
                }
                Command::GoToDefinition => self.goto_definition(window, cx),
                Command::Reveal(at) => self.place_caret_line(&input, at, cx),
                Command::Scroll(lines) => self.scroll_by_lines(&input, lines, cx),
                Command::Fold(op) => self.fold(op, cx),
            },
        }
        cx.notify();
    }

    /// Paints the block cursor, and takes the editor's caret out from under it.
    ///
    /// It is **ours** and not a selection of one character, which is what it was
    /// at first. A selection is only ever written by a keystroke, so there was no
    /// cursor at all before the first key was pressed on a file that had just
    /// opened, and none again after a click of the mouse; and it is painted in
    /// the theme's `selection`, a few percent of lightness away from the
    /// background — on One Dark one had to look for it. A decoration is painted
    /// **over** the selection (the text runs come after the selection paths) and
    /// in colours of ours: the caret's, with the background showing through the
    /// glyph, which is the block cursor of every terminal.
    ///
    /// The caret is only given back where the block has nothing to cover — an
    /// empty line, the end of the file — since a cursor that disappears there
    /// would be worse than two.
    fn sync_block_cursor(&mut self, on: bool, cx: &mut Context<Self>) {
        let Some(editing) = self.editing() else {
            return;
        };
        let (input, layer, mode) = (
            editing.input.clone(),
            editing.cursor.clone(),
            editing.vim.mode(),
        );
        let (caret, len) = {
            let state = input.read(cx);
            // The rope's length, which is borrowed: `value()` would copy the
            // whole file to learn one number.
            (state.selected_range().start, state.text().len())
        };
        let at = (mode, caret, len, on);
        if editing.cursor_at == Some(at) {
            return;
        }
        let block = on
            .then(|| {
                let text = input.read(cx).value();
                editing.vim.cursor(&text, caret)
            })
            .flatten()
            .filter(|range| !range.is_empty());
        let colour = vim_mode_colour(mode, cx);
        let style = gpui::HighlightStyle {
            color: Some(ink_on(colour, cx)),
            background_color: Some(colour),
            ..Default::default()
        };
        layer.set(
            block
                .clone()
                .map(|range| gpui_component::input::TextDecoration::new(range, style))
                .into_iter()
                .collect(),
            cx,
        );
        input.update(cx, |state, cx| {
            state.set_cursor_hidden(block.is_some(), cx);
        });
        if let Some(editing) = self.editing_mut() {
            editing.cursor_at = Some(at);
        }
    }

    /// Lights up what a yank has just copied, and puts it out again.
    ///
    /// A yank changes nothing on screen: without a sign, one is never sure it
    /// took, and one yanks again. It is what vim-highlightedyank exists for.
    ///
    /// Three things it does not do: it does not touch the selection — the block
    /// cursor is right there and two marks fighting over one character would say
    /// less than one —, it does not last a second like the plugin's default,
    /// which was chosen for a terminal where nothing else moves, and it does not
    /// hold a timer per yank: the task is **replaced**, and dropping a gpui task
    /// cancels it.
    fn flash_yank(&mut self, range: std::ops::Range<usize>, cx: &mut Context<Self>) {
        let Some(editing) = self.editing() else {
            return;
        };
        let flash = editing.flash.clone();
        // The tone of a search occurrence: it is already the colour this
        // interface lays over code to say "here", and it follows the theme.
        let style = gpui::HighlightStyle {
            background_color: Some(crate::ui::find::highlight_color(false, cx)),
            ..Default::default()
        };
        flash.set(
            vec![gpui_component::input::TextDecoration::new(range, style)],
            cx,
        );
        let timer = cx.spawn(async move |this, cx| {
            cx.background_executor().timer(YANK_FLASH).await;
            cx.update(|cx| flash.clear(cx));
            // The editor repaints of its own accord — the collection notifies —
            // but the panel is what holds the file, and it is what a stale
            // frame would show.
            let _ = this.update(cx, |_, cx| cx.notify());
        });
        if let Some(editing) = self.editing_mut() {
            editing.flash_timer = Some(timer);
        }
    }

    /// `zz`, `zt`, `zb`: puts the caret's line where the eye wants it.
    ///
    /// Nothing here moves the caret — that is what tells these from `z.` and
    /// `z-`, which also go to the first non-blank and are therefore two answers
    /// in one, where `Response` carries a single one.
    fn place_caret_line(
        &mut self,
        input: &Entity<EditorState>,
        at: crate::ui::vim::Reveal,
        cx: &mut Context<Self>,
    ) {
        let head = input.read(cx).selected_range().start;
        self.scroll_to_line(input, head, Place::Asked(at), cx);
    }

    /// `Ctrl+E` and `Ctrl+Y`: the page moves, the caret stays.
    ///
    /// Vim drags the caret along only when the page walks out from under it,
    /// and that part is left to the editor: `set_scroll_offset` is clamped to
    /// the real range, and the caret is where it was — which is the whole point
    /// of these two keys, reading ahead without losing one's place.
    fn scroll_by_lines(
        &mut self,
        input: &Entity<EditorState>,
        lines: isize,
        cx: &mut Context<Self>,
    ) {
        input.update(cx, |state, cx| {
            let Some(line_height) = state.line_height() else {
                return;
            };
            let offset = state.scroll_offset();
            let moved = offset.y - line_height * lines as f32;
            state.set_scroll_offset(gpui::point(offset.x, moved.min(gpui::px(0.))), cx);
        });
    }

    /// The `z` commands that fold.
    ///
    /// Where a fold begins and ends is the grammar's answer, never ours: the
    /// editor holds the candidates, and all that is decided here is which of
    /// them to close. `zc`, `zo` and `za` act on the **innermost** fold holding
    /// the caret, which is the one being read.
    fn fold(&mut self, op: crate::ui::vim::Fold, cx: &mut Context<Self>) {
        use crate::ui::vim::Fold;
        let Some(editing) = self.editing() else {
            return;
        };
        let input = editing.input.clone();
        let level = editing.fold_level;
        let ranges: Vec<crate::ui::folds::Range> = input
            .read(cx)
            .fold_candidates()
            .iter()
            .map(|range| (range.start_line, range.end_line))
            .collect();
        if ranges.is_empty() {
            return;
        }
        let caret = {
            let state = input.read(cx);
            line_at(&state.value(), state.selected_range().start)
        };
        let next = match op {
            Fold::Close | Fold::Open | Fold::Toggle => {
                let Some((start, _)) = ranges
                    .iter()
                    .filter(|(start, end)| *start <= caret && caret <= *end)
                    .max_by_key(|(start, _)| *start)
                    .copied()
                else {
                    return;
                };
                input.update(cx, |state, cx| {
                    let folded = match op {
                        Fold::Close => true,
                        Fold::Open => false,
                        _ => !state.is_folded_at(start),
                    };
                    state.set_folded(start, folded, cx);
                });
                return; // a single fold does not move the level
            }
            Fold::CloseAll => Some(0),
            Fold::OpenAll => None,
            // The ceiling is one past the deepest nesting: that is "everything
            // open", and `zr` reaching it is `zR`.
            Fold::More | Fold::Less => {
                let ceiling = crate::ui::folds::max_depth(&ranges).unwrap_or(0) + 1;
                let current = level.unwrap_or(ceiling);
                let wanted = match op {
                    Fold::More => current.saturating_sub(1),
                    _ => current + 1,
                };
                (wanted <= ceiling.saturating_sub(1)).then_some(wanted)
            }
        };
        input.update(cx, |state, cx| {
            // Rebuilt from nothing each time rather than folded on top of what
            // is there: `zr` opens a level, and reopening is not something the
            // fold map does by adding.
            state.unfold_all(cx);
            if let Some(level) = next {
                for start in crate::ui::folds::at_level(&ranges, level) {
                    state.set_folded(start, true, cx);
                }
            }
        });
        if let Some(editing) = self.editing_mut() {
            editing.fold_level = next;
        }
        cx.notify();
    }

    /// Scrolls a line into view, and says whether it could.
    ///
    /// `set_selected_range` scrolls to the **end** of what it is given, which is
    /// where the caret is in normal mode but the wrong end of a selection grown
    /// upwards: `V` then `k` would walk off the top of the panel without the
    /// view ever following. A landing needs it for a plainer reason: it writes
    /// an empty range, and the editor does not always take that for a move.
    ///
    /// It answers `false` when the editor has never been laid out and there is
    /// nothing to divide by — see `Editing::reveal_at`.
    fn scroll_to_line(
        &mut self,
        input: &Entity<EditorState>,
        head: usize,
        place: Place,
        cx: &mut Context<Self>,
    ) -> bool {
        use crate::ui::vim::Reveal;
        input.update(cx, |state, cx| {
            let (Some(rows), Some(line_height)) = (state.visible_row_range(), state.line_height())
            else {
                return false;
            };
            let row = line_at(&state.value(), head);
            let span = rows.len().max(1);
            let first = match place {
                // A line already in view stays where it is: a motion that moves
                // the caret one line must not move the page under the eye.
                Place::Nearest if rows.contains(&row) => return true,
                Place::Nearest if row < rows.start => row,
                Place::Nearest => row.saturating_sub(span - 1),
                Place::Asked(Reveal::Top) => row,
                Place::Asked(Reveal::Centre) => row.saturating_sub(span / 2),
                Place::Asked(Reveal::Bottom) => row.saturating_sub(span - 1),
            };
            // The input's scroll handle counts downwards as negative.
            let offset = state.scroll_offset();
            state.set_scroll_offset(gpui::point(offset.x, -(line_height * first as f32)), cx);
            true
        })
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
        let state = explorer.state;
        let rows = explorer.rows.clone();
        let dimmed = explorer.dimmed.clone();
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
        let open = self.editing().map(|editing| editing.path.clone());
        let entity = cx.entity();
        let look = Look::of(cx);

        // Nothing to show, and nothing under way: it is an empty project or a
        // search with no result. During the first `ls-files`, the list stays
        // blank — announcing "no file" and then showing them reads as a display
        // glitch.
        if count == 0 && state != Listing::Loading {
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
                                            &dimmed,
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
                .footer(super::dialogs::confirm())
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
    /// The "LSP" button of the editor's bar.
    ///
    /// **Here and not in the worktree's menu**, though the setting is per
    /// worktree: this is where its effect is read — the underlines, the
    /// completion, the count of what the server has found — and an action goes
    /// where the gesture it ends is made. It is also the only place with room
    /// to say what state the server is in.
    ///
    /// It only appears when something serves this file. A button that can only
    /// answer "no server for a Markdown file" is worse than no button.
    /// The two arrows of the trail.
    ///
    /// They are the same gesture as `Ctrl+O` and `Ctrl+I`, for whoever does not
    /// have vim mode on — and the only thing on this bar that says the trail
    /// exists at all. Both are always shown, greyed when there is nowhere to
    /// go: an arrow that appears and disappears moves the four buttons beside
    /// it every time one follows a definition.
    fn render_jump_buttons(&self, cx: &mut Context<Self>) -> Option<impl IntoElement> {
        let trail = self.active.as_ref().and_then(|w| self.jumps.get(w));
        let (back, forward) = match trail {
            Some(trail) => (trail.can_back(), trail.can_forward()),
            None => (false, false),
        };
        Some(
            h_flex()
                .child(
                    Button::new("editor-jump-back")
                        .ghost()
                        .xsmall()
                        .icon(icon("arrow-left"))
                        .disabled(!back)
                        .tooltip(tr!("editor-jump-back"))
                        .on_click(cx.listener(|this, _, window, cx| this.jump_back(window, cx))),
                )
                .child(
                    Button::new("editor-jump-forward")
                        .ghost()
                        .xsmall()
                        .icon(icon("arrow-right"))
                        .disabled(!forward)
                        .tooltip(tr!("editor-jump-forward"))
                        .on_click(cx.listener(|this, _, window, cx| this.jump_forward(window, cx))),
                ),
        )
    }

    fn render_lsp_button(&self, cx: &mut Context<Self>) -> Option<impl IntoElement> {
        use crate::ui::lsp::Status;
        let editing = self.editing()?;
        let servers = self.lsp_servers(&editing.worktree, cx);
        crate::lsp::pick(&servers, &editing.path)?;
        let on = self.lsp_enabled(&editing.worktree);
        let session = self.lsp_session(&editing.worktree);
        let status = session.map(|session| session.status.clone());
        let problems = session.map(|session| session.problems()).unwrap_or(0);
        let colour = match (&status, on) {
            (_, false) => cx.theme().muted_foreground,
            (Some(Status::Failed(_)), _) => cx.theme().danger,
            (Some(Status::Ready), _) if problems > 0 => cx.theme().warning,
            (Some(Status::Ready), _) => cx.theme().success,
            // Starting, or nothing yet: neither on nor in trouble.
            _ => cx.theme().muted_foreground,
        };
        let label = match (on, &status) {
            (true, Some(Status::Ready)) if problems > 0 => format!("LSP {problems}"),
            _ => "LSP".to_string(),
        };
        let tooltip = crate::ui::lsp::tooltip(session).unwrap_or_else(|| tr!("editor-lsp"));
        Some(
            Button::new("editor-lsp")
                .ghost()
                .xsmall()
                .label(label)
                .text_color(colour)
                .tooltip(tooltip)
                .on_click(cx.listener(|this, _, window, cx| this.toggle_lsp(window, cx))),
        )
    }

    pub(super) fn render_editor(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<impl IntoElement> {
        let editing = self.editing()?;
        let (path, dirty, input) = (editing.path.clone(), editing.dirty, editing.input.clone());
        let vim = Settings::global(cx).vim_mode;
        let mode = editing.vim.mode();
        // The `/` line being typed, or the keys of a command not complete yet:
        // vim shows both, and they are the only thing that says why the next key
        // will not do what it usually does.
        let hint = editing
            .vim
            .prompt()
            .unwrap_or_else(|| editing.vim.pending().to_string());
        // The smoothing advances by one frame, as the diff's does. The offset
        // is written back through `InputState`, which is the only way in: the
        // editor has no `ScrollHandle` of its own to hand out.
        let (offset, max) = editor_extent(&input, cx);
        if let Some(next) = self
            .owned_motion(EDITOR_SCROLL.into(), crate::ui::motion::Axes::Vertical)
            .advance_at(offset, max, window)
        {
            input.update(cx, |state, cx| state.set_scroll_offset(next, cx));
        }
        // The caret a jump left waiting for a measurement. The first frame of a
        // freshly opened file has none; asking for another is what gets the
        // view to the symbol instead of leaving it at the top of the file, and
        // the count is what stops a panel shrunk to nothing from asking for
        // ever.
        if let Some(head) = self.editing().and_then(|editing| editing.reveal_at) {
            let centred = Place::Asked(crate::ui::vim::Reveal::Centre);
            if self.scroll_to_line(&input, head, centred, cx) {
                if let Some(editing) = self.editing_mut() {
                    editing.reveal_at = None;
                }
            } else if let Some(editing) = self.editing_mut() {
                editing.reveal_tries += 1;
                if editing.reveal_tries > 8 {
                    editing.reveal_at = None;
                } else {
                    window.request_animation_frame();
                }
            }
        }
        // The block cursor, and the caret that goes away under it. Both are
        // reread at every frame and not set once, like `TerminalView::sync_font`:
        // the mode changes under the keys and the setting under the form, the
        // calls are idempotent, and it is what makes turning vim mode off give
        // the caret back without anything else to do.
        self.sync_block_cursor(vim, cx);
        let entity = cx.entity();
        let mono = cx.theme().mono_font_family.clone();
        // The editor is code, like the diff on the screen next door: same
        // family, same size. Without saying so it inherits the interface's
        // proportional font, where an indentation no longer lines up.
        let code_size = px(crate::ui::settings::Settings::global(cx).diff_font_size);
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
                                .font_family(mono.clone())
                                .child(label),
                        )
                        // The mode, where the eye already is: on the file's
                        // own bar, and not in the window's status bar at the
                        // other end of the screen.
                        .when(vim, |el| el.child(self.render_vim_mode(mode, &hint, cx)))
                        // A badge and not an asterisk in the title: the title is
                        // already a truncated path, and one more character at the
                        // end goes unseen.
                        .when(dirty, |el| {
                            el.child(div().size(px(7.)).rounded_full().bg(cx.theme().warning))
                        })
                        .children(self.render_jump_buttons(cx))
                        .children(self.render_lsp_button(cx))
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
                .child(
                    div()
                        .id("editor-zoom")
                        // Three keys of navigation live here and nowhere else:
                        // see `shortcuts::EDITOR_PREDICATE`.
                        .key_context(crate::ui::shortcuts::editor_context(vim))
                        .relative()
                        .flex_1()
                        .min_h_0()
                        // The capture phase, on an ancestor of the editor: see
                        // `vim_key`. Installed only when the mode is on, so that
                        // nothing stands between the keyboard and the input
                        // otherwise.
                        .when(vim, |el| {
                            el.capture_key_down(cx.listener(|this, event, window, cx| {
                                this.vim_key(event, window, cx)
                            }))
                        })
                        .child(
                            Editor::new(&input)
                                .font_family(mono)
                                .text_size(code_size)
                                // **And its line height with it.** `Input`
                                // declares `line_height(Rems(1.25))`, which is
                                // rem-based and therefore deaf to the text
                                // size: zoomed in, the glyphs grew and the
                                // lines did not, so each one was drawn over the
                                // one above. Our refinement is applied after
                                // theirs, and it is the diff's own spacing —
                                // the same code read the same way on both
                                // screens.
                                .line_height(crate::ui::diff_view::line_height(code_size))
                                .h_full(),
                        )
                        // **The wheel is taken before the editor sees it.**
                        // `InputState::on_scroll_wheel` scrolls and then calls
                        // `stop_propagation` as soon as the offset moved: a
                        // listener on the ancestor — the diff's arrangement —
                        // was never called, except at the very top and bottom.
                        // Nothing smoothed anything, and `Ctrl`+wheel zoomed
                        // while the editor went on scrolling underneath. A
                        // window mouse listener in the **capture** phase runs
                        // first, and consuming the event there leaves the whole
                        // movement to us.
                        .child(
                            gpui::canvas(
                                |_, _, _| (),
                                move |bounds: gpui::Bounds<Pixels>, _, window, _cx| {
                                    window.on_mouse_event(
                                        move |event: &gpui::ScrollWheelEvent, phase, window, cx| {
                                            if phase != gpui::DispatchPhase::Capture
                                                || !bounds.contains(&event.position)
                                            {
                                                return;
                                            }
                                            cx.stop_propagation();
                                            entity.update(cx, |this, cx| {
                                                this.on_editor_scroll(event, window, cx)
                                            });
                                        },
                                    );
                                },
                            )
                            .absolute()
                            .inset_0(),
                        ),
                ),
        )
    }

    /// The mode pill, and what is being typed towards a command.
    fn render_vim_mode(
        &self,
        mode: crate::ui::vim::Mode,
        hint: &str,
        cx: &Context<Self>,
    ) -> impl IntoElement {
        let colour = vim_mode_colour(mode, cx);
        h_flex()
            .gap_1()
            .items_center()
            .child(
                div()
                    .px_1p5()
                    .rounded(cx.theme().radius)
                    .text_xs()
                    .border_1()
                    .border_color(colour)
                    .text_color(colour)
                    .child(tr!(mode.key())),
            )
            .when(!hint.is_empty(), |el| {
                el.child(
                    div()
                        .text_xs()
                        .font_family(cx.theme().mono_font_family.clone())
                        .text_color(cx.theme().muted_foreground)
                        .child(SharedString::from(hint.to_string())),
                )
            })
    }
}

/// The editor's wheel: zoom with the platform key, smoothed scrolling
/// otherwise — the diff's two gestures, on the other screen that shows code.
///
/// The same inversion as `on_diff_scroll`, and for the same reason: gpui has no
/// capture phase for the wheel, so the editor has **already** scrolled when this
/// runs. We give the jump back rather than try to prevent it.
impl ClaudhubApp {
    pub(super) fn on_editor_scroll(
        &mut self,
        event: &gpui::ScrollWheelEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(input) = self.editing().map(|editing| editing.input.clone()) else {
            return;
        };
        let (offset, max) = editor_extent(&input, cx);
        // The editor's own line height and not the ambient one: it is what its
        // handler would have used, and three lines apart make a visible
        // difference on a font that is neither the interface's nor its size.
        let line = input
            .read(cx)
            .line_height()
            .unwrap_or_else(|| window.line_height())
            .max(px(1.));
        let delta = event.delta.pixel_delta(line);

        if event.modifiers.secondary() {
            // A zoom during a smoothed scroll: the destination was computed on
            // lines that no longer have the same height. Nothing to give back —
            // the event was consumed before the editor could scroll on it.
            self.owned_motion(EDITOR_SCROLL.into(), crate::ui::motion::Axes::Vertical)
                .cancel();
            let steps = crate::ui::terminal_view::zoom_steps(delta.y);
            if steps != 0. {
                // The diff's size, and not one of its own: it is code on both
                // sides, never shown at the same time, and two sizes to keep in
                // step would be one too many.
                crate::ui::settings::Settings::update_global(cx, |s| {
                    s.zoom(crate::ui::settings::Zoom::Diff, steps);
                });
            }
            cx.notify();
            return;
        }

        let next = match event.delta {
            // A trackpad is already gradual, and attached to the finger:
            // smoothing it would add lag to a direct gesture.
            gpui::ScrollDelta::Pixels(_) => {
                self.owned_motion(EDITOR_SCROLL.into(), crate::ui::motion::Axes::Vertical)
                    .cancel();
                gpui::point(offset.x, offset.y + delta.y)
            }
            gpui::ScrollDelta::Lines(_) => self
                .owned_motion(EDITOR_SCROLL.into(), crate::ui::motion::Axes::Vertical)
                .push(offset, delta, max),
        };
        input.update(cx, |state, cx| state.set_scroll_offset(next, cx));
        cx.notify();
    }
}

/// The editor's scroll offset, and how far it can go.
///
/// The travel is **worked out** and not read: `scroll_size` and the viewport's
/// bounds are `pub(crate)` in gpui-component, where the offset is not. Visible
/// rows times the line height is the viewport, the file's lines times the same
/// is the content, and their difference is the travel. It is an approximation —
/// a soft-wrapped line counts for one — and it does not have to be better than
/// that: `set_scroll_offset` clamps to the real range, and the next frame reads
/// back what it actually got.
fn editor_extent(
    input: &Entity<EditorState>,
    cx: &App,
) -> (gpui::Point<Pixels>, gpui::Point<Pixels>) {
    let state = input.read(cx);
    let offset = state.scroll_offset();
    let travel = match (state.line_height(), state.visible_row_range()) {
        (Some(line_height), Some(visible)) => {
            let lines = state.value().lines().count();
            let hidden = lines.saturating_sub(visible.len()) as f32;
            line_height * hidden
        }
        // Before the first layout there is nothing to clamp against: the motion
        // aims where it is asked to, and the editor cuts it back.
        _ => px(f32::MAX / 4.),
    };
    (offset, gpui::point(px(0.), travel))
}

/// How many lines `Ctrl+D` moves by half of, before the editor has been laid out
/// once and can say how tall it is.
const DEFAULT_ROWS: usize = 20;

/// The colour a mode is said in — the pill on the file's bar, and the block
/// cursor alike.
///
/// One table and not two: the word and the cursor say the same thing, and a
/// mode one reads in the corner while the block says another colour would be one
/// of them too many. It is also what makes the mode legible without looking away
/// from the caret, which is the point of a modal editor.
fn vim_mode_colour(mode: crate::ui::vim::Mode, cx: &gpui::App) -> gpui::Hsla {
    match mode {
        // The theme's own cursor colour, which is what the eye expects there.
        crate::ui::vim::Mode::Normal => cx.theme().caret,
        crate::ui::vim::Mode::Insert => cx.theme().success,
        crate::ui::vim::Mode::Visual => cx.theme().magenta,
        crate::ui::vim::Mode::VisualLine => cx.theme().cyan,
    }
}

/// The ink a block cursor's glyph takes: whichever of the theme's two extremes
/// stands out against the block.
///
/// Not `background` alone: it is the right answer on a dark theme, where the
/// block is a light colour, and the wrong one on a light theme, where a pale
/// glyph on a pale block is a hole.
fn ink_on(colour: gpui::Hsla, cx: &gpui::App) -> gpui::Hsla {
    let (background, foreground) = (cx.theme().background, cx.theme().foreground);
    let (darker, lighter) = if background.l < foreground.l {
        (background, foreground)
    } else {
        (foreground, background)
    };
    if colour.l > 0.5 {
        darker
    } else {
        lighter
    }
}

/// How long a yank stays lit.
///
/// Not vim-highlightedyank's second, which was chosen for a terminal where
/// nothing else moves: here the mode pill, the dirty badge and an agent's
/// writing are all on the same screen, and a mark that outstays the gesture
/// reads as a state rather than as an acknowledgement.
const YANK_FLASH: std::time::Duration = std::time::Duration::from_millis(300);

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
    dimmed: &Rc<Vec<bool>>,
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
    // What `.gitignore` leaves out is shown, but shown as secondary — PhpStorm's
    // convention, and the only thing that keeps `vendor/` from reading like part
    // of the project.
    let dim = dimmed.get(index).copied().unwrap_or(false);
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
                        .when(dim, |el| el.text_color(look.muted))
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
                        .when(dim, |el| el.text_color(look.muted))
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
