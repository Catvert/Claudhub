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
use crate::ui::surface::{Place, Surface};
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
    /// Where the jump came from, when it is one — a place in another file, or
    /// the screen one was reading. A step through the trail leaves this empty:
    /// recording it would put back the place one has just stepped away from,
    /// and the trail would never end.
    pub from: Option<crate::ui::jumps::Place>,
}

/// The files one worktree has open, and which of them is on screen.
///
/// A list and not one file: the tab bar is the dock's own — one panel per open
/// file, as there is one panel per terminal — and what the rest of the window
/// asks for is still "the file being edited", which is `active`.
#[derive(Default)]
pub struct Editors {
    pub open: Vec<Editing>,
    /// Index in `open`. Kept in range by every removal; an empty list has no
    /// active file and `active()` says so.
    pub active: usize,
}

impl Editors {
    pub fn active(&self) -> Option<&Editing> {
        self.open.get(self.active)
    }

    pub fn active_mut(&mut self) -> Option<&mut Editing> {
        self.open.get_mut(self.active)
    }

    pub fn index_of(&self, path: &Path) -> Option<usize> {
        self.open.iter().position(|editing| editing.path == path)
    }

    pub fn by_path_mut(&mut self, path: &Path) -> Option<&mut Editing> {
        let ix = self.index_of(path)?;
        self.open.get_mut(ix)
    }
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
    /// The modal harness — the vim state and the layers it paints on. One per
    /// open file: leaving a file in insert mode and coming back to another in
    /// normal mode is what a tabbed vim does anyway. It is the same type the SQL
    /// console holds; see `ui::surface`.
    pub host: crate::ui::surface::VimHost,
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
    /// What is on screen differs from what is on disk.
    pub dirty: bool,
    /// A change is already on its way to the language server.
    ///
    /// A keystroke emits a change event, and sending each one is fifty round
    /// trips for a state nobody asked about in between: the flag is what makes
    /// the debounce read the latest text once rather than every text in turn.
    pub lsp_pending: bool,
    /// The tab showing this file. It is the dock that draws it, and closing it
    /// — by the cross, by `Ctrl+W`, by the worktree going away — is what takes
    /// the file down.
    pub panel: Entity<crate::ui::panels::FilePanel>,
}

impl ClaudhubApp {
    // — The tree ————————————————————————————————————————————————

    fn explorer(&mut self) -> Option<&mut Explorer> {
        let worktree = self.active.clone()?;
        Some(self.explorers.entry(worktree).or_default())
    }

    /// The file on screen in the displayed worktree's editor, if there is one.
    ///
    /// One **set** of editors per worktree, as there is one set of terminals:
    /// what is on screen is what belongs to the tree the rest of the window
    /// shows, and among those the tab the dock is displaying.
    pub(super) fn editing(&self) -> Option<&Editing> {
        self.editings.get(&self.editing_root()?)?.active()
    }

    pub(super) fn editing_mut(&mut self) -> Option<&mut Editing> {
        let root = self.editing_root()?;
        self.editings.get_mut(&root)?.active_mut()
    }

    /// The files this worktree holds open, in tab order.
    pub(super) fn editors(&self, root: &Path) -> Option<&Editors> {
        self.editings.get(root)
    }

    /// Whether the "pick a file" panel has anything to say.
    ///
    /// It is the centre of the editing screen only while there is no file: as
    /// soon as one is open it is a tab of its own, and two tabs saying the same
    /// thing would be one too many.
    pub(super) fn empty_editor_visible(&self) -> bool {
        self.panel_visible(crate::ui::panels::EditorPanel::NAME)
            && self
                .editing_root()
                .and_then(|root| self.editings.get(&root))
                .is_none_or(|tabs| tabs.open.is_empty())
    }

    /// Whose file the editing screen shows.
    ///
    /// The displayed worktree, unless one went off to edit a plugin's script —
    /// which belongs to no worktree, and whose directory stands in for one. See
    /// `ClaudhubApp::editing_root`.
    pub(super) fn editing_root(&self) -> Option<PathBuf> {
        self.editing_root.clone().or_else(|| self.active.clone())
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
        let from = Some(self.here(cx)).filter(|from| from.path() != Some(path.as_path()));
        self.landing = Some(Pending {
            worktree: worktree.clone(),
            path: path.clone(),
            landing,
            from,
        });
        self.git.send(Cmd::ReadFile { worktree, path });
        cx.notify();
    }

    /// Where one stands, for the trail to remember.
    ///
    /// The caret when a file is being edited **and shown** — one is on another
    /// screen most of the time, and a file that stayed open behind it is not
    /// where the gesture is being made. Everywhere else the answer is the
    /// screen, which is all a screen ever needs to be put back: what it shows
    /// is its own state, still there when one comes back to it.
    pub(super) fn here(&self, cx: &App) -> crate::ui::jumps::Place {
        // Written out rather than imported: this file has a `Place` of its own,
        // which says where a line comes to rest on screen.
        use crate::ui::jumps::Place;
        use crate::ui::workspace::Workspace;
        match self.workspace {
            Workspace::Files => {
                if let Some(editing) = self.editing() {
                    let offset = editing.input.read(cx).selected_range().start;
                    return Place::Editor(crate::ui::jumps::Spot::new(
                        editing.path.clone(),
                        offset,
                    ));
                }
            }
            // A console with nothing sent yet is the screen and no more: there
            // is no result to come back to.
            Workspace::Db => {
                if let (Some(connection), Some(sql)) =
                    (self.query.connection.as_ref(), self.query.sent.clone())
                {
                    return Place::Query {
                        connection: connection.key(),
                        database: self.query.database.clone(),
                        sql,
                    };
                }
            }
            _ => {}
        }
        Place::Screen(self.workspace)
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
        let from = self.here(cx);
        let Some(offset) = self.land(&landing, window, cx) else {
            return;
        };
        let to = crate::ui::jumps::Place::Editor(crate::ui::jumps::Spot::new(path, offset));
        if from != to {
            self.jumps.entry(worktree).or_default().jump(from, to);
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
        let here = self.here(cx);
        let Some(trail) = self.jumps.get_mut(&worktree) else {
            return;
        };
        let Some(place) = (if back {
            trail.back(here)
        } else {
            trail.forward(here)
        }) else {
            return;
        };
        match place {
            // A screen is put back by going to it: what it shows never left.
            crate::ui::jumps::Place::Screen(workspace) => {
                self.enter_workspace(workspace, window, cx)
            }
            crate::ui::jumps::Place::Query {
                connection,
                database,
                sql,
            } => self.replay_db_query(connection, database, sql, window, cx),
            crate::ui::jumps::Place::Editor(spot) => {
                // The file is opened on its screen, the step having been made
                // from somewhere else as often as not.
                if self
                    .editing()
                    .is_some_and(|editing| editing.path == spot.path)
                {
                    self.enter_workspace(crate::ui::workspace::Workspace::Files, window, cx);
                    self.land(&Landing::Offset(spot.offset), window, cx);
                } else {
                    // No origin: the step is already written in the trail, and
                    // putting it back would be a place one could go back from
                    // for ever.
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
            }
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
        // The file is captured too, and not only its worktree: several are open
        // now, and marking "the current one" dirty would put the star on the
        // wrong tab the moment an edit lands anywhere else.
        let owner = worktree.clone();
        let edited = path.clone();
        cx.subscribe(&input, move |this, _, event, cx| {
            if !matches!(event, gpui_component::input::InputEvent::Change) {
                return;
            }
            if let Some(editing) = this
                .editings
                .get_mut(&owner)
                .and_then(|tabs| tabs.by_path_mut(&edited))
            {
                editing.dirty = true;
            }
            this.lsp_editor_changed(&owner, cx);
            cx.notify();
        })
        .detach();
        // The server must forget the document we are leaving. One session per
        // worktree holds **one** open document — the tab being read — so
        // arriving in a file closes the one that was there, exactly as before
        // tabs existed. Switching tabs does the same, in `show_file`.
        if let Some(previous) = self
            .editings
            .get(&worktree)
            .and_then(|tabs| tabs.active())
            .map(|editing| (editing.worktree.clone(), editing.path.clone()))
        {
            if previous.1 != path {
                self.lsp_editor_closed(previous.0, previous.1);
            }
        }
        let host = crate::ui::surface::VimHost::new(&input, cx);
        let opened = (worktree.clone(), path.clone());
        // The tab of a file already open is **reused**, its content replaced:
        // reopening the same file is what a save-and-reread does, and a second
        // tab on one file is the one thing a tab bar must never grow.
        let reopened = self
            .editings
            .get(&worktree)
            .and_then(|tabs| tabs.index_of(&path));
        let panel = match reopened {
            Some(ix) => {
                let panel = self.editings[&worktree].open[ix].panel.clone();
                let fresh = input.clone();
                panel.update(cx, |panel, cx| panel.rebind(fresh, cx));
                panel
            }
            None => self.open_file_tab(&worktree, &path, input.clone(), window, cx),
        };
        let editing = Editing {
            worktree: worktree.clone(),
            path: path.clone(),
            input,
            hash: content.hash,
            dirty: false,
            lsp_pending: false,
            reveal_at: None,
            reveal_tries: 0,
            host,
            panel,
        };
        let tabs = self.editings.entry(worktree.clone()).or_default();
        match reopened {
            Some(ix) => {
                tabs.open[ix] = editing;
                tabs.active = ix;
            }
            None => {
                tabs.open.push(editing);
                tabs.active = tabs.open.len() - 1;
            }
        }
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
                    self.jumps.entry(opened.0.clone()).or_default().jump(
                        from,
                        crate::ui::jumps::Place::Editor(crate::ui::jumps::Spot::new(
                            opened.1.clone(),
                            offset,
                        )),
                    );
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
        } else {
            // The next remembered tab, one at a time: see `read_next_file`.
            self.continue_restore(window, cx);
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

    /// Opens a tab for a file, and puts it in the editing screen's dock.
    ///
    /// The dock's bar **is** the tab bar, as it is for the terminals: that is
    /// what lets a file be dragged into a split, zoomed, or sent beside
    /// another. A bar of our own would have been a second chrome painted under
    /// the one the window already has.
    ///
    /// One panel and not one per screen, unlike a terminal: a file is only ever
    /// read on the editing screen.
    fn open_file_tab(
        &mut self,
        worktree: &Path,
        path: &Path,
        input: Entity<EditorState>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Entity<crate::ui::panels::FilePanel> {
        let app = cx.entity();
        // Handed over rather than read: the panel is built inside an `update`
        // on the application, so it cannot read the entity to find out for
        // itself. The observation set up in its constructor takes over.
        let visible = self.editing_root().as_deref() == Some(worktree)
            && self.panel_visible(crate::ui::panels::EditorPanel::NAME);
        let panel = cx.new(|cx| {
            crate::ui::panels::FilePanel::new(
                &app,
                worktree.to_path_buf(),
                path.to_path_buf(),
                input,
                visible,
                cx,
            )
        });
        // The tab group the new one joins is the one holding the files already
        // open; failing that, `Anywhere` lands it in the centre — where the
        // editor panel is, which is precisely the group wanted.
        let sibling = self
            .editings
            .get(worktree)
            .and_then(|tabs| tabs.open.last())
            .map(|editing| editing.panel.clone());
        if let Some(dock) = self
            .docks
            .get(&crate::ui::workspace::Workspace::Files)
            .cloned()
        {
            dock.update(cx, |dock, cx| {
                crate::ui::panels::dock_panel_at(
                    dock,
                    gpui_component::dock::panel_handle(panel.clone()),
                    |dock| {
                        dock.layout(gpui_component::dock::DockPlacement::Center)
                            .and_then(|layout| {
                                layout.find_panel_node(gpui_component::dock::PanelId::from(
                                    sibling?.entity_id(),
                                ))
                            })
                            .map(|node| gpui_component::dock::InsertTarget::Tabs {
                                node,
                                ix: None,
                                // The file one has just opened is the file one
                                // reads.
                                activate: true,
                            })
                    },
                    window,
                    cx,
                );
            });
        }
        // The file one has just opened is the file one reads. Said here as well
        // as in the insert target: with no sibling there is no move to carry
        // the flag, and the group would go on showing whatever tab it had.
        crate::ui::panels::FilePanel::activate(&panel, window, cx);
        panel
    }

    /// Makes an open file the tab on screen.
    ///
    /// Called by the panel that is being drawn: the dock renders exactly one
    /// tab of a group, so **the panel that renders is the file being read** —
    /// a fact to be read off the frame rather than an event to be caught. The
    /// language server follows: one session holds one open document, so the
    /// tab one leaves is closed and the one arriving is opened.
    pub(super) fn show_file(
        &mut self,
        worktree: PathBuf,
        path: PathBuf,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(tabs) = self.editings.get(&worktree) else {
            return;
        };
        let Some(ix) = tabs.index_of(&path) else {
            return;
        };
        if tabs.active == ix {
            return;
        }
        let left = tabs.active().map(|editing| editing.path.clone());
        if let Some(tabs) = self.editings.get_mut(&worktree) {
            tabs.active = ix;
        }
        if let Some(left) = left {
            if left != path {
                self.lsp_editor_closed(worktree.clone(), left);
            }
        }
        self.lsp_sync_editor(window, cx);
        self.persist_session(cx);
        cx.notify();
    }

    /// Drops a file's tab: the panel goes out of the dock and the state with it.
    ///
    /// Idempotent, and it has to be: the dock's cross and `Ctrl+W` both come
    /// through here, and removing the panel makes the dock call `on_removed`,
    /// which comes back through here a second time with nothing left to find.
    pub(super) fn close_file(
        &mut self,
        worktree: PathBuf,
        path: PathBuf,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(tabs) = self.editings.get_mut(&worktree) else {
            return;
        };
        let Some(ix) = tabs.index_of(&path) else {
            return;
        };
        let editing = tabs.open.remove(ix);
        // The finger stays on the tab that takes its place, and on the last one
        // when the file closed was itself the last.
        tabs.active = tabs.active.min(tabs.open.len().saturating_sub(1));
        let empty = tabs.open.is_empty();
        if empty {
            self.editings.remove(&worktree);
        }
        if let Some(dock) = self
            .docks
            .get(&crate::ui::workspace::Workspace::Files)
            .cloned()
        {
            dock.update(cx, |dock, cx| {
                dock.remove_panel(editing.panel.clone(), window, cx)
            });
        }
        self.lsp_editor_closed(editing.worktree, editing.path);
        // The focus was on a node nobody renders any more, and such a handle
        // resolves no binding: every shortcut would stay dead until a click put
        // it back. The tree is where one is left looking.
        if empty {
            let tree = self.explorer_focus.clone();
            tree.focus(window, cx);
        }
        self.persist_session(cx);
        cx.notify();
    }

    /// Drops every tab of a worktree — it is going away.
    pub(super) fn close_files_of(
        &mut self,
        worktree: &Path,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let doomed: Vec<PathBuf> = self
            .editings
            .get(worktree)
            .map(|tabs| tabs.open.iter().map(|e| e.path.clone()).collect())
            .unwrap_or_default();
        for path in doomed {
            self.close_file(worktree.to_path_buf(), path, window, cx);
        }
    }

    /// Closes the editor, asking for confirmation if the file has changed.
    pub(super) fn close_editor(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(editing) = self.editing() else {
            return;
        };
        // The worktree the gesture was made in, and not the one selected when
        // the dialog is answered: one browses while a question is open.
        let worktree = editing.worktree.clone();
        let path = editing.path.clone();
        if !editing.dirty {
            self.close_file(worktree, path, window, cx);
            return;
        }
        let label = SharedString::from(editing.path.display().to_string());
        let entity = cx.entity();
        window.open_dialog(cx, move |dialog, _window, _cx| {
            let (entity, label, worktree) = (entity.clone(), label.clone(), worktree.clone());
            let path = path.clone();
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
                .on_ok(move |_, window, cx| {
                    let (worktree, path) = (worktree.clone(), path.clone());
                    entity.update(cx, |this, cx| {
                        this.close_file(worktree, path, window, cx);
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
        let (back, forward) = self.can_travel();
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
        let mode = editing.host.vim.mode();
        // The `/` line being typed, or the keys of a command not complete yet:
        // vim shows both, and they are the only thing that says why the next key
        // will not do what it usually does.
        let hint = editing
            .host
            .vim
            .prompt()
            .unwrap_or_else(|| editing.host.vim.pending().to_string());
        // The smoothing advances by one frame, as the diff's does.
        self.advance_surface_scroll(Surface::File, &input, window, cx);
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
        self.sync_block_cursor(Surface::File, vim, cx);
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
                                this.vim_key(Surface::File, event, window, cx)
                            }))
                            // `Ctrl+V` is a binding of the input's before it is
                            // a keystroke: see `vim_paste`.
                            .capture_action(cx.listener(
                                |this, _: &gpui_component::input::Paste, window, cx| {
                                    if this.vim_paste(Surface::File, window, cx) {
                                        cx.stop_propagation();
                                    }
                                },
                            ))
                        })
                        .child(
                            // **No card of its own.** `Input` paints a
                            // background, a radius and a border; under the tab
                            // group's own card that drew a rounded box inset in
                            // a frame, and its fill is `editor.background` —
                            // the very colour the card is already painted in.
                            // All that was left of it was a grey line stitching
                            // back what the dock had just unstitched. The code
                            // sits on the card, the way the diff next door
                            // does.
                            Editor::new(&input)
                                .appearance(false)
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
                                                this.on_surface_scroll(
                                                    Surface::File,
                                                    event,
                                                    window,
                                                    cx,
                                                )
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
            guide: crate::ui::theme::indent_guide(cx),
            folder: cx.theme().muted_foreground,
        }
    }
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
                .children(crate::ui::theme::indent_guides(*depth, look.guide))
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
                .children(crate::ui::theme::indent_guides(*depth, look.guide))
                .child(crate::ui::theme::chevron_space())
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
