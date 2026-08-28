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

/// How long a drag has to stay on a closed folder before it opens.
///
/// Long enough that crossing the tree on the way somewhere else costs nothing,
/// short enough that staying reads as asking. It is the delay every file
/// manager uses, and it is not a setting.
const DRAG_HOVER: std::time::Duration = std::time::Duration::from_millis(600);

/// A row of the tree being dragged.
///
/// The payload of an internal drag, and the ghost that follows the pointer: a
/// drag's value must be a type of its own — that is what a drop listener reads
/// to know the drag is one of ours and not the desktop's files.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DraggedEntry {
    /// Relative to the worktree, folder or file.
    pub path: PathBuf,
}

impl gpui::Render for DraggedEntry {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let name = self
            .path
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_default();
        h_flex()
            .px_2()
            .py_0p5()
            .gap_1()
            .rounded(cx.theme().radius)
            .bg(cx.theme().popover)
            .border_1()
            .border_color(cx.theme().border)
            .text_xs()
            .child(SharedString::from(name))
    }
}

/// The move a drop is asking for, when there is one to make.
///
/// `None` for the three gestures that mean nothing and that a file manager
/// swallows silently: dropping something back where it already is, dropping a
/// folder on itself, and dropping it into one of its own children — the last
/// one being the only one that would do damage.
fn move_within(from: &Path, to: &Path) -> Option<files::Op> {
    if from == to || to.starts_with(from) {
        return None;
    }
    Some(files::Op::Rename {
        from: from.to_path_buf(),
        to: to.to_path_buf(),
    })
}

/// One flag per file: does `.gitignore` leave it out?
///
/// Both lists come sorted from git — the second is a subset of the first — so
/// one pass answers for every file. What used to be here was a binary search
/// per leaf **and per folder that carries it**, which on a checkout with a
/// `target/` is a hundred thousand path comparisons for a single chevron.
///
/// An unsorted list would only mean some rows are not greyed: the walk stops
/// naming what it has passed, and nothing else reads these flags.
fn excluded_flags(files: &[PathBuf], ignored: &[PathBuf]) -> Vec<bool> {
    if ignored.is_empty() {
        return Vec::new();
    }
    let mut rest = ignored.iter().peekable();
    let mut flags = Vec::with_capacity(files.len());
    for path in files {
        while rest.peek().is_some_and(|other| *other < path) {
            rest.next();
        }
        flags.push(rest.peek() == Some(&path));
    }
    flags
}

/// The folder a row hands a drop to: a folder takes it, a file gives it to the
/// one it lives in. The worktree's root is the empty path.
fn drop_dir(path: &Path, is_dir: bool) -> PathBuf {
    if is_dir {
        path.to_path_buf()
    } else {
        path.parent().unwrap_or(Path::new("")).to_path_buf()
    }
}

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
    ///
    /// Behind an `Rc` because the render hands it to the closure of a
    /// virtualised list: a clone of a hundred thousand `PathBuf` — what a
    /// checkout with a `target/` weighs — is three milliseconds **per frame**,
    /// wheel animation included.
    pub files: Rc<Vec<PathBuf>>,
    /// The tree those paths make, folds left out.
    ///
    /// Held from one fold to the next, and this is the whole answer to a
    /// `target/` that took a fifth of a second to open: building it costs some
    /// twenty milliseconds on a hundred thousand paths, and **a collapse
    /// changes none of it** — only which rows come out of it, which is a walk
    /// of what is visible.
    tree: tree::Tree,
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
    /// One flag per file, saying whether `.gitignore` leaves it out.
    ///
    /// A flag and not the sorted list git gave, which used to be searched: a
    /// directory is dimmed only when **everything** under it is ignored, so the
    /// walk visits every leaf of every folder on screen — a hundred thousand
    /// path comparisons on a checkout with a `target/`, two tenths of a second
    /// for one chevron. Read once when the list arrives, at the cost of one
    /// pass over two sorted lists.
    excluded: Vec<bool>,
    /// The excluded paths, sorted, as `excluded` was computed from.
    ///
    /// Kept rather than deduced back from the flags: opening a folder shifts
    /// every index after it, and a flag read by index would then name another
    /// file. Five hundred entries on a Laravel checkout, since git stops at a
    /// directory it excludes whole.
    ignored_paths: Vec<PathBuf>,
    /// The paths that name a **directory**, sorted.
    ///
    /// What the tree needs to draw a chevron on `vendor/` rather than a file
    /// icon: a path with nothing under it is a file as far as a tree built from
    /// paths can tell. A directory stays here once it has been read, or it
    /// would be drawn twice.
    dirs: Vec<PathBuf>,
    /// Those of `dirs` nobody has looked inside yet, sorted.
    ///
    /// What says which folder a chevron has to go and read. A folder leaves
    /// this list the moment its contents arrive; a fresh listing puts every
    /// one of them back, git having stopped at each again.
    unexplored: Vec<PathBuf>,
    /// The reads under way, so that a chevron pressed twice, or a rebuild
    /// arriving while one is in flight, does not send a second command.
    reading: std::collections::HashSet<PathBuf>,
    /// One flag per row of `rows`, saying whether it is dimmed.
    ///
    /// Computed with the tree and not at paint time: a directory is dimmed only
    /// if **everything** under it is ignored, and `vendor/` carries thirty
    /// thousand leaves. That is a price a gesture can pay and a frame cannot.
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
    /// The folder a drag is hovering over, and which is about to open by
    /// itself. Cleared as soon as the drag names another one, so that the
    /// timer that opens it can tell "still there" from "gone".
    pub drag_hover: Option<PathBuf>,
    /// What git says about each file, ready to be looked up by a row.
    ///
    /// Built when a status arrives (`index_file_status`) and not at render:
    /// the panel renders at every frame, and a map of a hundred `PathBuf` keys
    /// built sixty times a second is a map nobody asked for. Behind an `Rc`
    /// because the render hands it to a virtualised list's closure.
    pub status: Rc<std::collections::HashMap<PathBuf, crate::git::StatusCode>>,
}

impl Default for Explorer {
    fn default() -> Self {
        Self {
            files: Rc::new(Vec::new()),
            tree: tree::Tree::default(),
            rows: Rc::new(Vec::new()),
            expanded: std::collections::HashSet::new(),
            state: Listing::Idle,
            ignored: false,
            excluded: Vec::new(),
            ignored_paths: Vec::new(),
            dirs: Vec::new(),
            unexplored: Vec::new(),
            reading: std::collections::HashSet::new(),
            dimmed: Rc::new(Vec::new()),
            query: String::new(),
            cursor: None,
            drag_hover: None,
            status: Rc::default(),
        }
    }
}

impl Explorer {
    /// The list git has just given, and what of it is ignored.
    ///
    /// The one door in: the tree and the exclusion flags are read from the
    /// paths, and holding a list they do not match is what would make a row
    /// open the wrong file.
    fn set_files(
        &mut self,
        files: Vec<PathBuf>,
        ignored: Vec<PathBuf>,
        dirs: Vec<PathBuf>,
        unexplored: Vec<PathBuf>,
    ) {
        self.excluded = excluded_flags(&files, &ignored);
        self.ignored_paths = ignored;
        self.dirs = dirs;
        self.unexplored = unexplored;
        self.tree = tree::Tree::with_dirs(&files, &self.dirs);
        self.files = Rc::new(files);
        self.rebuild();
    }

    /// What one level of an excluded directory holds, folded into the lists.
    ///
    /// Everything under a directory git stopped at is excluded by construction
    /// — git never descends into one — so what arrives goes into both lists at
    /// once, and the folders arrive unexplored in their turn.
    ///
    /// Merged and re-sorted rather than appended: `excluded_flags` walks two
    /// sorted lists in step, and a tail out of order silently stops greying
    /// everything past it. The directory itself stays in both — it is a row of
    /// the tree, and the index that says the whole of it is excluded.
    fn add_dir(&mut self, dir: &Path, dirs: Vec<PathBuf>, files: Vec<PathBuf>) {
        self.reading.remove(dir);
        let Ok(at) = self
            .unexplored
            .binary_search_by(|known| known.as_path().cmp(dir))
        else {
            // Read twice, or the list was replaced while this was in flight.
            return;
        };
        self.unexplored.remove(at);

        let mut unexplored = std::mem::take(&mut self.unexplored);
        unexplored.extend(dirs.iter().cloned());
        unexplored.sort_unstable();
        // `dir` stays: what leaves is the not-yet-read list, never the list of
        // what is a directory.
        let mut known = std::mem::take(&mut self.dirs);
        known.extend(dirs.iter().cloned());
        known.sort_unstable();
        known.dedup();

        let arrived = || dirs.iter().chain(files.iter()).cloned();
        let mut all = Rc::try_unwrap(std::mem::take(&mut self.files))
            .unwrap_or_else(|shared| (*shared).clone());
        all.extend(arrived());
        all.sort_unstable();
        all.dedup();

        let mut ignored = std::mem::take(&mut self.ignored_paths);
        ignored.extend(arrived());
        ignored.sort_unstable();
        ignored.dedup();

        self.set_files(all, ignored, known, unexplored);
    }

    /// The rows to display, given the folds and the search.
    ///
    /// Note what is **not** here: the tree itself. A collapse changes which
    /// rows come out of it and nothing else, and rebuilding it on every chevron
    /// is what made a `target/` of a hundred thousand files take a fifth of a
    /// second to open.
    fn rebuild(&mut self) {
        let rows = if self.query.trim().is_empty() {
            self.tree.rows(tree::Folds::ShutBut(&self.expanded))
        } else {
            // During a search, collapses are ignored and the tree is reduced to
            // what matches: a file found in a closed folder would not be
            // visible, and the search would look as if it had found nothing.
            let keep: Vec<usize> = self
                .files
                .iter()
                .enumerate()
                .filter(|(_, path)| crate::ui::find::matches(&self.query, &path.to_string_lossy()))
                .map(|(index, _)| index)
                .collect();
            // `dirs` and not `unexplored`, as `set_files` builds the whole
            // tree: a directory whose contents have arrived leaves the
            // unexplored list, and telling the subset that it is a file draws
            // it twice — once as the folder its children make, once as a leaf
            // of its own.
            tree::Tree::subset(&self.files, &keep, &self.dirs)
                .rows(tree::Folds::OpenBut(&std::collections::HashSet::new()))
        };
        self.dimmed = Rc::new(rows.iter().map(|entry| self.is_ignored(entry)).collect());
        self.rows = Rc::new(rows);
    }

    /// Is this row one that `.gitignore` leaves out?
    ///
    /// A directory counts as ignored only when everything under it is —
    /// `vendor/` is, `app/` with one ignored log file in it is not. Anything
    /// else would grey out a folder holding code one is looking for.
    fn is_ignored(&self, entry: &tree::Entry) -> bool {
        if self.excluded.is_empty() {
            return false;
        }
        let excluded = |index: &usize| self.excluded.get(*index).copied().unwrap_or(false);
        match entry {
            tree::Entry::Leaf { index, .. } => excluded(index),
            tree::Entry::Dir { leaves, .. } => !leaves.is_empty() && leaves.iter().all(excluded),
        }
    }

    /// A displayed entry's path, folder or file.
    fn path_at(&self, index: usize) -> Option<&Path> {
        match self.rows.get(index)? {
            tree::Entry::Dir { path, .. } => Some(path.as_path()),
            tree::Entry::Leaf { index, .. } => self.files.get(*index).map(PathBuf::as_path),
        }
    }

    /// Where a path sits in the displayed list, if it is still there.
    ///
    /// A walk, and it has to stay one — the rows are what the eye follows, so
    /// the answer is a position in them. What it must not do is **allocate**:
    /// it is called on every arrow key, and a `PathBuf` per row is a hundred
    /// thousand allocations for one keystroke.
    fn row_of(&self, wanted: &Path) -> Option<usize> {
        (0..self.rows.len()).find(|index| self.path_at(*index) == Some(wanted))
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
        let files = self.files.clone();
        for path in files.iter() {
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

/// The index of a buffer's last line, as `hunks::Hunk::covers` counts them.
///
/// `split('\n')` and never `lines()`: a final newline is a line of its own —
/// the ruling `ui::hunks` already makes.
fn last_line(text: &str) -> usize {
    text.split('\n').count().saturating_sub(1)
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

/// Which tab closes to make room, given what each holds and when it was read.
///
/// A decision of its own, before the view that acts on it: the rule is short and
/// every part of it was a bug waiting to happen. Never a tab holding unsaved
/// text — losing work to make room for a glance is the one thing the limit must
/// not do — and never the file about to open, which would close the tab being
/// reused. `None` when every tab is spoken for: the bar then goes over the limit
/// and stays there, which is what PhpStorm does too.
fn oldest_spare<'a>(
    tabs: impl Iterator<Item = (&'a Path, bool, u64)>,
    keep: &Path,
) -> Option<&'a Path> {
    tabs.filter(|(path, dirty, _)| !dirty && *path != keep)
        .min_by_key(|(_, _, used)| *used)
        .map(|(path, _, _)| path)
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
    /// The gesture only wants a look: the tab it opens is the preview tab, and
    /// the next look replaces it. Said here rather than read at arrival because
    /// it belongs to the **gesture**, and by the time the content lands the
    /// gesture is over.
    pub ephemeral: bool,
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

    pub fn by_path(&self, path: &Path) -> Option<&Editing> {
        self.open.get(self.index_of(path)?)
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
    /// The key this file's wheel smoothing is filed under, built once with the
    /// tab: `Surface::file_scroll_key` formats the path, and the smoothing asks
    /// for its key at every frame it advances.
    pub scroll_key: SharedString,
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
    ///
    /// One, as a terminal has one: a panel belongs to a single dock area at a
    /// time, and a file used to be read on the editing screen *and* in the home
    /// screen's centre. There is one area now.
    pub panel: Entity<crate::ui::panels::FilePanel>,
    /// What `HEAD` holds for this file, and what the gutter compares against.
    /// `None` until the answer arrives, and for a file git does not track.
    pub base: Option<String>,
    /// Whether the base has been asked for, so that a file with no base does
    /// not ask again on every keystroke.
    pub base_asked: bool,
    /// The gutter's marks, recomputed whenever the buffer changes. Held rather
    /// than derived at render: the render closure runs every frame and this
    /// walks the whole file.
    pub hunks: Rc<Vec<crate::ui::hunks::Hunk>>,
    /// The index of the buffer's last line, as `hunks::Hunk::covers` counts
    /// them. Written with `hunks`, by the one place that recomputes them: it
    /// was read back by splitting a copy of the whole text, three times over —
    /// once per keystroke for the marks, and once per frame for an open
    /// popover.
    pub last_line: usize,
    /// The picture this tab shows, when what was opened is one to look at
    /// rather than to edit. `None` for a file, and that is what every gesture
    /// reads to know which of the two a tab holds — see `ui::preview`.
    pub preview: Option<crate::ui::preview::Preview>,
    /// The hunk whose popover is open, named by the buffer line it was opened
    /// from. A line and not an index: the list is rebuilt on every keystroke,
    /// and an index into the old one names a different hunk in the new one.
    pub hunk_open: Option<usize>,
    /// This tab is the preview tab: opened for a look, and given up as soon as
    /// another file is looked at. Typing in it keeps it, and so does opening it
    /// deliberately — see `ClaudhubApp::browse_in_editor`.
    pub ephemeral: bool,
    /// When this tab was last read, on `ClaudhubApp::tab_clock`'s counter.
    ///
    /// A stamp and not a position in a list: the tab order is the bar's, which
    /// the user rearranges by dragging, and the limit closes on *reading* age.
    pub used: u64,
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

    /// One named open file, whichever tab is displayed.
    ///
    /// What a **gesture** on a code surface reaches, where `editing()` is what
    /// the rest of the window means by "the file being edited". The two part
    /// company as soon as two files are on screen at once — see `ui::surface`.
    pub(super) fn editing_at(&self, path: &Path) -> Option<&Editing> {
        self.editings.get(&self.editing_root()?)?.by_path(path)
    }

    pub(super) fn editing_at_mut(&mut self, path: &Path) -> Option<&mut Editing> {
        let root = self.editing_root()?;
        self.editings.get_mut(&root)?.by_path_mut(path)
    }

    /// The files this worktree holds open, in tab order.
    pub(super) fn editors(&self, root: &Path) -> Option<&Editors> {
        self.editings.get(root)
    }

    /// Whether the "pick a file" panel has anything to say.
    ///
    /// It stands for the centre only while the centre has nothing else: as soon
    /// as a file, a diff, a query or a hit arrives it is a tab beside them, and
    /// a tab saying "open a file" among four documents is one of the empty
    /// rooms the `needed:` rule exists to remove.
    pub(super) fn empty_centre_visible(&self) -> bool {
        self.panel_visible(crate::ui::panels::EditorPanel::NAME)
            && !self.diff_on_screen()
            && !self.db_console_open()
            && !self.search_preview_open()
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
        let due = match explorer.state {
            // Waiting for an answer: asking again would only queue a second
            // command behind the first.
            Listing::Loading => false,
            // Already answered for the setting in force. A failure is not
            // retried either, until something invalidates it — a toggle, a file
            // operation, a refresh.
            Listing::Ready | Listing::Failed if explorer.ignored == ignored => false,
            _ => true,
        };
        if due {
            explorer.state = Listing::Loading;
            explorer.ignored = ignored;
            self.git.send(Cmd::ListFiles {
                worktree: worktree.clone(),
                ignored,
            });
        }
        // Here and not in each gesture that opens a folder: five of them do —
        // the chevron, the right arrow, a drag that lingers, a drop, a restored
        // session — and the one that gets forgotten is a chevron that opens on
        // nothing. It walks `expanded`, the handful of folders opened by hand,
        // and `reading` keeps a frame from asking twice.
        self.read_open_dirs(&worktree);
    }

    pub(super) fn project_files_arrived(
        &mut self,
        worktree: PathBuf,
        files: Vec<PathBuf>,
        ignored: Vec<PathBuf>,
        dirs: Vec<PathBuf>,
    ) {
        let explorer = self.explorers.entry(worktree).or_default();
        explorer.state = Listing::Ready;
        explorer.set_files(files, ignored, dirs.clone(), dirs);
        // A fresh listing puts every directory back to unread, so what was open
        // inside `vendor/` is read again on the next frame — a chevron that
        // shuts under the hand on every `git add` is worse than the reads it
        // saves. See `ensure_project_files`.
    }

    /// Files the status by path, for the tree to colour its rows.
    ///
    /// Called when a status arrives, which is the only thing that changes the
    /// answer — the tree itself may not have been asked for yet, and an
    /// explorer built here starts idle like any other.
    pub(super) fn index_file_status(&mut self, worktree: &Path) {
        let status = self
            .review
            .get(worktree)
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
            .unwrap_or_default();
        self.explorers
            .entry(worktree.to_path_buf())
            .or_default()
            .status = Rc::new(status);
    }

    /// Reads the excluded directories that are open and whose contents are
    /// unknown.
    ///
    /// Called from the render, where the panel already asks for its list: five
    /// gestures open a folder, and one place answers for all of them. It also
    /// covers a listing that has just replaced everything the chevrons had
    /// read, and a restored session — what `expanded` remembers is asked for
    /// again. It walks `expanded`, the handful of folders opened by hand, never
    /// the tree.
    fn read_open_dirs(&mut self, worktree: &Path) {
        let Some(explorer) = self.explorers.get_mut(worktree) else {
            return;
        };
        let due: Vec<PathBuf> = explorer
            .expanded
            .iter()
            .filter(|path| {
                explorer
                    .unexplored
                    .binary_search_by(|known| known.as_path().cmp(path))
                    .is_ok()
                    && !explorer.reading.contains(*path)
            })
            .cloned()
            .collect();
        for dir in due {
            explorer.reading.insert(dir.clone());
            self.git.send(Cmd::ReadDir {
                worktree: worktree.to_path_buf(),
                dir,
            });
        }
    }

    /// One level of an excluded directory has arrived.
    pub(super) fn dir_listed(
        &mut self,
        worktree: PathBuf,
        dir: PathBuf,
        result: Result<(Vec<PathBuf>, Vec<PathBuf>), String>,
    ) {
        let Some(explorer) = self.explorers.get_mut(&worktree) else {
            return;
        };
        match result {
            Ok((dirs, files)) => explorer.add_dir(&dir, dirs, files),
            Err(_) => {
                // The folder is gone, or is not readable. It keeps its chevron
                // and stays empty rather than vanishing: what git listed is
                // still what git listed, and the failure is already in the log.
                explorer.reading.remove(&dir);
                explorer.expanded.remove(&dir);
                explorer.rebuild();
            }
        }
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
        explorer.cursor = explorer.path_at(next).map(Path::to_path_buf);
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
        explorer.cursor = explorer
            .path_at(if last { count - 1 } else { 0 })
            .map(Path::to_path_buf);
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
            (true, Some(worktree)) => crate::wslpath::join(worktree, path).display().to_string(),
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
            explorer.set_files(Vec::new(), Vec::new(), Vec::new(), Vec::new());
        }
        cx.notify();
    }

    // — Reading and writing ———————————————————————————————————

    /// Opens a file in the built-in editor, in a tab of its own.
    pub(super) fn open_in_editor(&mut self, path: PathBuf, cx: &mut Context<Self>) {
        self.open_at(path, None, cx);
    }

    /// Opens a file to be **looked at**: it takes the preview tab, the one the
    /// next look replaces.
    ///
    /// The gesture the tree makes on a single click. Browsing a project opens
    /// ten files for one that is read, and a bar that grows by one per glance
    /// is a bar one stops reading — the same answer as VS Code's, and the
    /// reason its preview tab exists at all.
    pub(super) fn browse_in_editor(&mut self, path: PathBuf, cx: &mut Context<Self>) {
        let ephemeral = crate::ui::settings::Settings::global(cx).editor_preview_tab;
        self.open_where(path, None, ephemeral, cx);
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
        self.open_where(path, landing, false, cx);
    }

    /// The same, saying whether the tab it opens is only for a look.
    fn open_where(
        &mut self,
        path: PathBuf,
        landing: Option<Landing>,
        ephemeral: bool,
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
            ephemeral,
        });
        self.git.send(self.read_file_cmd(worktree, path));
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
        use crate::ui::panels::*;
        // The document in front of the centre is where one is. It used to be
        // the screen, which said the same thing when a screen held one centre.
        let front = self.front_document(cx);
        match front.as_deref() {
            Some(EditorPanel::NAME) => {
                if let Some(editing) = self.editing() {
                    let offset = editing.input.read(cx).selected_range().start;
                    return Place::Editor(crate::ui::jumps::Spot::new(
                        editing.path.clone(),
                        offset,
                    ));
                }
            }
            // A console with nothing sent yet is the view and no more: there is
            // no result to come back to.
            Some(ConsolePanel::NAME) => {
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
        // A name the registry knows, so that a place can be replayed: the trail
        // holds `&'static str`, and what a running tree gives back is a
        // `String`.
        front
            .as_deref()
            .and_then(crate::ui::panels::registered_name)
            .map(Place::Panel)
            .unwrap_or(Place::Panel(DiffPanel::NAME))
    }

    /// The document the centre is showing — a file's own tab included.
    ///
    /// `None` when the centre shows nothing at all, which a fresh window with
    /// every conditional view still empty is entitled to.
    pub(super) fn front_document(&self, cx: &App) -> Option<String> {
        self.seats(cx)
            .into_iter()
            .find(|seat| seat.anchor.is_none() && seat.shown)
            .map(|seat| seat.panel)
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
            // A document is put back by bringing it forward: what it shows
            // never left.
            crate::ui::jumps::Place::Panel(name) => self.reveal_panel(name, window, cx),
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
                    self.reveal_panel(crate::ui::panels::EditorPanel::NAME, window, cx);
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
                        // Stepping through the trail is not browsing: the file
                        // one comes back to is one already worked in.
                        ephemeral: false,
                    });
                    self.git.send(self.read_file_cmd(worktree, spot.path));
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

    /// Stamps a tab as the one being read, and gives back the stamp.
    pub(super) fn touch_tab(&mut self) -> u64 {
        self.tab_clock += 1;
        self.tab_clock
    }

    /// Whether a gesture asked for this very file, and whether it only wants a
    /// look. `None` when the content is an arrival nobody asked for — a save, an
    /// agent's write, the watcher — which must move no tab.
    pub(super) fn asked_for(&self, worktree: &Path, path: &Path) -> Option<bool> {
        self.landing
            .as_ref()
            .filter(|pending| pending.worktree == worktree && pending.path == path)
            .map(|pending| pending.ephemeral)
    }

    /// Makes room for a tab about to open: the preview tab it replaces, then
    /// the oldest read over the limit.
    ///
    /// Called **before** the tab is installed, and that is the whole reason it
    /// is a step of its own: closing shifts every index, and the code that
    /// follows works in indices.
    pub(super) fn make_tab_room(
        &mut self,
        worktree: &Path,
        path: &Path,
        ephemeral: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        // The preview tab is the one being replaced, so it goes — unless it is
        // this very file, which is then simply read again.
        if ephemeral {
            let previous = self.editors(worktree).and_then(|tabs| {
                tabs.open
                    .iter()
                    .find(|editing| editing.ephemeral && editing.path != path)
                    .map(|editing| editing.path.clone())
            });
            if let Some(previous) = previous {
                self.close_file(worktree.to_path_buf(), previous, window, cx);
            }
        }
        let limit = crate::ui::settings::Settings::global(cx).editor_tab_limit;
        if limit == 0 {
            return;
        }
        // A file already open takes no room: its tab is reused.
        let opening = self
            .editors(worktree)
            .is_none_or(|tabs| tabs.index_of(path).is_none());
        while opening
            && self
                .editors(worktree)
                .is_some_and(|tabs| tabs.open.len() >= limit)
        {
            let oldest = self.editors(worktree).and_then(|tabs| {
                oldest_spare(
                    tabs.open
                        .iter()
                        .map(|editing| (editing.path.as_path(), editing.dirty, editing.used)),
                    path,
                )
                .map(Path::to_path_buf)
            });
            let Some(oldest) = oldest else {
                return;
            };
            self.close_file(worktree.to_path_buf(), oldest, window, cx);
        }
    }

    /// Lets the language server forget the document being left.
    ///
    /// One session per worktree holds **one** open document — the tab being
    /// read — so arriving in a file closes the one that was there, exactly as
    /// before tabs existed. Switching tabs does the same, in `show_file`.
    pub(super) fn close_previous_document(&mut self, worktree: &Path, opening: &Path) {
        let Some(previous) = self
            .editings
            .get(worktree)
            .and_then(|tabs| tabs.active())
            .map(|editing| (editing.worktree.clone(), editing.path.clone()))
        else {
            return;
        };
        if previous.1 != opening {
            self.lsp_editor_closed(previous.0, previous.1);
        }
    }

    /// The panel an arriving tab is painted in, and the tab it replaces.
    ///
    /// The tab of a file already open is **reused**, its content rebound:
    /// reopening the same file is what a save-and-reread does, and a second tab
    /// on one file is the one thing a tab bar must never grow. Such a tab is
    /// also brought to the front — `open_file_tab` says it for a tab it
    /// creates, and a reused one had nobody to say it, so the group went on
    /// showing whatever was there, which reads as a click that did nothing.
    ///
    /// Only when a gesture asked for the file, which is what a pending landing
    /// on this very file means: a reread — a save, an agent's write, the
    /// watcher — must not pull the group away from the tab being read.
    ///
    /// Room is made by the caller (`make_tab_room`) — and not always: a tab the
    /// session is putting back takes the place it already had.
    pub(super) fn tab_panel(
        &mut self,
        worktree: &Path,
        path: &Path,
        input: &Entity<EditorState>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> (Option<usize>, Entity<crate::ui::panels::FilePanel>) {
        let reopened = self
            .editings
            .get(worktree)
            .and_then(|tabs| tabs.index_of(path));
        let asked = self.asked_for(worktree, path).is_some();
        let panel = match reopened {
            Some(ix) => {
                let panel = self.editings[worktree].open[ix].panel.clone();
                {
                    let fresh = input.clone();
                    panel.update(cx, |panel, cx| panel.rebind(fresh, cx));
                    if asked {
                        crate::ui::panels::FilePanel::activate(&panel, window, cx);
                    }
                }
                panel
            }
            None => self.open_file_tab(worktree, path, input.clone(), window, cx),
        };
        (reopened, panel)
    }

    /// Files a freshly built tab, and makes it the one being read.
    pub(super) fn place_tab(&mut self, worktree: &Path, reopened: Option<usize>, editing: Editing) {
        let tabs = self.editings.entry(worktree.to_path_buf()).or_default();
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
    }

    /// What every arrival does once its tab holds something: the caret asked
    /// for, the step in the trail, the screen it is read on, and the session.
    ///
    /// The pending landing that names another file is stale — one asked for a
    /// second file before the first arrived — and is dropped, not put back:
    /// what it pointed at is not what is on screen. `caret` is false for a
    /// picture, which has nowhere to put one; the trail still records the
    /// place, which is the file itself.
    pub(super) fn finish_tab(
        &mut self,
        worktree: &Path,
        path: &Path,
        restored: bool,
        caret: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(pending) = self.landing.take() {
            if (pending.worktree.as_path(), pending.path.as_path()) == (worktree, path) {
                let offset = pending
                    .landing
                    .filter(|_| caret)
                    .and_then(|landing| self.land(&landing, window, cx))
                    .unwrap_or(0);
                if let Some(from) = pending.from {
                    self.jumps.entry(worktree.to_path_buf()).or_default().jump(
                        from,
                        crate::ui::jumps::Place::Editor(crate::ui::jumps::Spot::new(
                            path.to_path_buf(),
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
        // Unless it is a place being put back — the previous session's, or the
        // one this worktree was left in: there is no gesture then, and the
        // screen that comes back is the place's own, set a step earlier.
        if !restored {
            self.reveal_panel(crate::ui::panels::EditorPanel::NAME, window, cx);
        } else {
            // The next remembered tab, one at a time: see `read_next_file`.
            self.continue_restore(window, cx);
        }
        self.persist_session(cx);
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
        let restored = self.take_restored_editing(&worktree, &path);
        // A tab put back by the session is one that was kept, however it was
        // opened: the preview tab is a gesture's, and there is no gesture here.
        let ephemeral = !restored && self.asked_for(&worktree, &path).unwrap_or(false);
        if !restored {
            self.make_tab_room(&worktree, &path, ephemeral, window, cx);
        }
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
                // A file one types in is a file one stays in: the preview tab
                // becomes a tab like the others, and nothing replaces it.
                editing.ephemeral = false;
            }
            this.lsp_editor_changed(&owner, cx);
            // The gutter answers while one types, which is the whole point of
            // comparing against the buffer rather than against the file on
            // disk. It costs a walk of the document per keystroke — the same
            // order as the highlighter's, and far below a frame.
            this.recompute_hunks(&owner, &edited, cx);
            cx.notify();
        })
        .detach();
        // The server must forget the document we are leaving.
        self.close_previous_document(&worktree, &path);
        let host = crate::ui::surface::VimHost::new(&input, cx);
        let asked = self.asked_for(&worktree, &path).is_some();
        let (reopened, panel) = self.tab_panel(&worktree, &path, &input, window, cx);
        // The base a reread had is kept as a stand-in while the fresh one is on
        // its way: saving does not move `HEAD`, so it is almost always the same
        // answer, and dropping it would blank the gutter on every save.
        let base = reopened.and_then(|ix| self.editings[&worktree].open[ix].base.clone());
        let editing = Editing {
            worktree: worktree.clone(),
            path: path.clone(),
            scroll_key: crate::ui::surface::Surface::file_scroll_key(&path),
            input,
            hash: content.hash,
            dirty: false,
            lsp_pending: false,
            reveal_at: None,
            reveal_tries: 0,
            host,
            panel,
            base,
            base_asked: false,
            hunks: Rc::default(),
            last_line: 0,
            hunk_open: None,
            preview: None,
            // A reread keeps what the tab was: saving a file looked at does not
            // make it a file one stays in.
            ephemeral: reopened
                .map(|ix| self.editings[&worktree].open[ix].ephemeral && !asked)
                .unwrap_or(false)
                || ephemeral,
            used: self.touch_tab(),
        };
        self.place_tab(&worktree, reopened, editing);
        // Starts the server this file's language asks for, opens the document
        // and posts the providers — all of it a no-op when the button is off.
        self.lsp_sync_editor(window, cx);
        // What the gutter marks against. Asked for on every read and not only
        // on the first: a save is a reread, and between two of them a commit
        // may have moved `HEAD` under the file.
        self.ask_file_base(&worktree, &path);
        self.recompute_hunks(&worktree, &path, cx);
        // The caret the opening was asked for, the screen it is read on, and
        // the session: see `finish_tab`.
        self.finish_tab(&worktree, &path, restored, true, window, cx);
    }

    /// `Ctrl+S`: writes what has been typed.
    ///
    /// **Every unsaved tab of the checkout**, and not only the one on screen —
    /// the setting is on by default. One edits a file, follows a call into
    /// another, edits that one too, and the gesture one makes at the end of it
    /// means "put my work on disk", not "put on disk whichever of it the dock
    /// happens to be showing". It is what an IDE does — PhpStorm has no
    /// per-file save at all — and the risk it removes is the one that costs:
    /// a tab left unsaved behind another, and a build run on a file nobody
    /// wrote.
    ///
    /// The tab on screen is written whether or not it is marked, which is what
    /// the single-file gesture always did: `Ctrl+S` on a file one has not
    /// touched is a way of saying "write it anyway".
    pub(super) fn save_file(&mut self, cx: &mut Context<Self>) {
        let Some(root) = self.editing_root() else {
            return;
        };
        let mut doomed: Vec<PathBuf> = Vec::new();
        if crate::ui::settings::Settings::global(cx).save_all_tabs {
            if let Some(tabs) = self.editors(&root) {
                doomed.extend(
                    tabs.open
                        .iter()
                        .filter(|editing| editing.dirty)
                        .map(|editing| editing.path.clone()),
                );
            }
        }
        if let Some(path) = self.editing().map(|editing| editing.path.clone()) {
            if !doomed.contains(&path) {
                doomed.push(path);
            }
        }
        for path in doomed {
            self.save_tab(&path, cx);
        }
        // A server that runs a formatter or an external analyser on save — as
        // PHPantom does with PHPStan — has no other way of knowing.
        self.lsp_editor_saved();
        cx.notify();
    }

    /// Writes one open file back, named by its path.
    ///
    /// By path and not by "the file being edited": saving them all walks tabs
    /// the dock is not showing, and the index of a tab is not a thing to hold
    /// across a write.
    fn save_tab(&mut self, path: &Path, cx: &mut Context<Self>) {
        let Some(editing) = self.editing_at(path) else {
            return;
        };
        // Nothing to write back from a picture: the tab holds an empty editor
        // nobody typed in, and saving it would truncate the file to nothing.
        if editing.preview.is_some() {
            return;
        }
        let content = editing.input.read(cx).value().to_string();
        self.git.send(Cmd::WriteFile {
            worktree: editing.worktree.clone(),
            path: path.to_path_buf(),
            content: content.clone(),
            // The digest of what we had read: an agent that wrote in the
            // meantime makes the save be refused rather than be overwritten.
            expect: Some(editing.hash),
        });
        // The digest follows what has just been sent: without that, two saves in
        // a row would make the second be refused, the file having changed — by
        // us.
        if let Some(editing) = self.editing_at_mut(path) {
            editing.hash = files::digest(&content);
            editing.dirty = false;
        }
    }

    // — The gutter's change marks ————————————————————————————————

    /// Asks git what `HEAD` holds for a file, once per outstanding question.
    pub(super) fn ask_file_base(&mut self, worktree: &Path, path: &Path) {
        let Some(editing) = self
            .editings
            .get_mut(worktree)
            .and_then(|tabs| tabs.by_path_mut(path))
        else {
            return;
        };
        if editing.base_asked {
            return;
        }
        editing.base_asked = true;
        self.git.send(Cmd::ReadFileBase {
            worktree: worktree.to_path_buf(),
            path: path.to_path_buf(),
        });
    }

    /// Asks again what `HEAD` holds, for every file open in a worktree.
    ///
    /// `None` means every worktree: a few operations answer without naming one,
    /// and there are never more than a handful of tabs.
    pub(super) fn refresh_editor_bases(&mut self, worktree: Option<&Path>) {
        let asking: Vec<(PathBuf, PathBuf)> = self
            .editings
            .iter()
            .filter(|(root, _)| worktree.is_none_or(|only| only == root.as_path()))
            .flat_map(|(root, tabs)| {
                tabs.open
                    .iter()
                    .map(move |editing| (root.clone(), editing.path.clone()))
            })
            .collect();
        for (root, path) in asking {
            self.ask_file_base(&root, &path);
        }
    }

    /// What `HEAD` holds for a file has come back.
    pub(super) fn file_base_arrived(
        &mut self,
        worktree: PathBuf,
        path: PathBuf,
        text: Option<String>,
        cx: &mut Context<Self>,
    ) {
        let Some(editing) = self
            .editings
            .get_mut(&worktree)
            .and_then(|tabs| tabs.by_path_mut(&path))
        else {
            return;
        };
        editing.base_asked = false;
        editing.base = text;
        self.recompute_hunks(&worktree, &path, cx);
    }

    /// Recomputes a file's gutter marks from its base and its live buffer.
    ///
    /// Held on the `Editing` rather than worked out at render: the render
    /// closure runs every frame, and this walks the document. It runs on every
    /// keystroke instead, which is a few hundred times less often.
    pub(super) fn recompute_hunks(&mut self, worktree: &Path, path: &Path, cx: &mut Context<Self>) {
        let Some(editing) = self
            .editings
            .get(worktree)
            .and_then(|tabs| tabs.by_path(path))
        else {
            return;
        };
        // No base means no marks rather than "every line is new": a file git
        // does not track is not a file one has changed, and painting it green
        // from top to bottom says something the gutter does not mean.
        let now = editing.input.read(cx).value();
        let hunks = match &editing.base {
            Some(base) => crate::ui::hunks::compare(base, &now),
            None => Vec::new(),
        };
        let last = last_line(&now);
        let Some(editing) = self
            .editings
            .get_mut(worktree)
            .and_then(|tabs| tabs.by_path_mut(path))
        else {
            return;
        };
        // A popover open on a line the edit has just taken away has nothing
        // left to show; one whose line still carries a hunk stays.
        if let Some(row) = editing.hunk_open {
            if !hunks.iter().any(|hunk| hunk.covers(row, last)) {
                editing.hunk_open = None;
            }
        }
        editing.hunks = Rc::new(hunks);
        editing.last_line = last;
        self.install_gutter_marks(worktree, path, cx);
        cx.notify();
    }

    /// Hands the editor the closure that draws its change strip.
    ///
    /// Reinstalled rather than reread: the renderer the element calls takes a
    /// line and nothing else — no `App` to look anything up in — so everything
    /// it answers with has to be captured. That is affordable because it is
    /// only ever redone on a gesture (an edit, a click), never on a frame.
    ///
    /// The colours are the exception, read inside the canvas at paint time: a
    /// theme change is signalled to nobody, and a strip captured in the old
    /// palette would sit there in the wrong colour until the next keystroke.
    fn install_gutter_marks(&mut self, worktree: &Path, path: &Path, cx: &mut Context<Self>) {
        let Some(editing) = self
            .editings
            .get(worktree)
            .and_then(|tabs| tabs.by_path(path))
        else {
            return;
        };
        let input = editing.input.clone();
        let hunks = editing.hunks.clone();
        // A file with no base has no renderer at all, and one that has a base
        // keeps its own even with nothing to mark: the strip lives in a margin
        // the gutter already had, so installing it costs no width, and taking
        // it off and putting it back on either side of an edit would be work
        // for nothing.
        if editing.base.is_none() {
            input.update(cx, |state, cx| state.set_gutter_marks(None, cx));
            return;
        }
        // Written by `recompute_hunks`, which is the only thing that changes
        // what the marks answer.
        let last = editing.last_line;
        let app = cx.entity().downgrade();
        let (worktree, path) = (worktree.to_path_buf(), path.to_path_buf());
        let render: gpui_component::input::GutterMarkRenderer = Rc::new(move |row| {
            let kind = hunks.iter().find(|hunk| hunk.covers(row, last))?.kind();
            let (worktree, path, app) = (worktree.clone(), path.clone(), app.clone());
            Some(
                div()
                    .id(("claudhub-hunk", row))
                    .size_full()
                    // The canvas below is absolute, and an absolute child needs
                    // a positioned parent or it anchors to something further up
                    // than this strip.
                    .relative()
                    .cursor_pointer()
                    .child(
                        gpui::canvas(
                            |_, _, _| {},
                            move |bounds, _, window, cx| paint_hunk_mark(kind, bounds, window, cx),
                        )
                        .absolute()
                        .inset_0(),
                    )
                    .on_mouse_down(gpui::MouseButton::Left, move |_, _, cx| {
                        // The strip belongs to the gutter, and the gutter is
                        // not the text: without this the click would also land
                        // in the editor and move the caret.
                        cx.stop_propagation();
                        app.update(cx, |this, cx| this.open_hunk(&worktree, &path, row, cx))
                            .ok();
                    })
                    .into_any_element(),
            )
        });
        input.update(cx, |state, cx| state.set_gutter_marks(Some(render), cx));
    }

    /// Shows — or hides again — what a hunk replaced.
    fn open_hunk(&mut self, worktree: &Path, path: &Path, row: usize, cx: &mut Context<Self>) {
        if let Some(editing) = self
            .editings
            .get_mut(worktree)
            .and_then(|tabs| tabs.by_path_mut(path))
        {
            // Clicking the strip a second time on the same line puts it away:
            // the marker is the only thing there is to click, so it has to be
            // the way back out too.
            editing.hunk_open = (editing.hunk_open != Some(row)).then_some(row);
        }
        self.install_gutter_marks(worktree, path, cx);
        cx.notify();
    }

    /// Puts a hunk's old lines back, as an ordinary edit.
    ///
    /// An edit and not a `git apply`: the buffer may hold unsaved work, git
    /// only ever sees the file on disk, and `apply_patch` writes to the index
    /// anyway. Going through the editor means the rollback lands in the same
    /// transaction log as everything else — `u` takes it back, and the file is
    /// left dirty for the save the user will make.
    pub(super) fn rollback_hunk(
        &mut self,
        worktree: PathBuf,
        path: PathBuf,
        row: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(editing) = self
            .editings
            .get(&worktree)
            .and_then(|tabs| tabs.by_path(&path))
        else {
            return;
        };
        let input = editing.input.clone();
        let text = input.read(cx).value();
        let last = editing.last_line;
        let Some(hunk) = editing
            .hunks
            .iter()
            .find(|hunk| hunk.covers(row, last))
            .cloned()
        else {
            return;
        };
        let (span, replacement) = crate::ui::hunks::rollback(&text, &hunk);
        input.update(cx, |state, cx| {
            state.set_selected_range(span.clone(), cx);
            state.replace(replacement, window, cx);
            // Where the restored lines begin, so the caret is at what just
            // came back rather than at the end of it.
            state.set_selected_range(span.start..span.start, cx);
        });
        if let Some(editing) = self
            .editings
            .get_mut(&worktree)
            .and_then(|tabs| tabs.by_path_mut(&path))
        {
            editing.hunk_open = None;
        }
        self.recompute_hunks(&worktree, &path, cx);
    }

    /// Opens a file's tabs, and puts one in the dock of each screen that reads
    /// files.
    ///
    /// The dock's bar **is** the tab bar, as it is for the terminals: that is
    /// what lets a file be dragged into a split, zoomed, or sent beside
    /// another. A bar of our own would have been a second chrome painted under
    /// the one the window already has.
    ///
    /// One panel, as a terminal has one. A file used to have a face on each
    /// screen that read files, a panel belonging to a single area at a time;
    /// there is one area now.
    pub(super) fn open_file_tab(
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
        let panel = {
            let (app, input) = (app.clone(), input.clone());
            cx.new(|cx| {
                crate::ui::panels::FilePanel::new(
                    &app,
                    worktree.to_path_buf(),
                    path.to_path_buf(),
                    input,
                    visible,
                    cx,
                )
            })
        };
        // The tab group the new one joins is the one holding the files already
        // open; failing that, `Anywhere` lands it in the centre — where the
        // empty-centre panel is, which is precisely the group wanted.
        let sibling = self
            .editings
            .get(worktree)
            .and_then(|tabs| tabs.open.last())
            .map(|editing| editing.panel.clone());
        let dock = self.dock.clone();
        dock.update(cx, |dock, cx| {
            crate::ui::panels::dock_panel_at(
                dock,
                gpui_component::dock::panel_handle(panel.clone()),
                gpui_component::dock::DockPlacement::Center,
                None,
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
        // The tab one arrives in is the most recently read, which is what the
        // tab limit closes on. Switching does not keep a preview tab, though:
        // browsing through the bar is browsing all the same.
        let stamp = self.touch_tab();
        if let Some(tabs) = self.editings.get_mut(&worktree) {
            tabs.active = ix;
            if let Some(editing) = tabs.open.get_mut(ix) {
                editing.used = stamp;
            }
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
        // `close_file` is idempotent — removing the panel makes the dock call
        // `on_removed`, which comes back here with nothing left to find.
        let dock = self.dock.clone();
        dock.update(cx, |dock, cx| {
            dock.remove_panel(editing.panel.clone(), window, cx)
        });
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

    /// Closes the file on screen, asking about it if it has changed.
    pub(super) fn close_editor(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(editing) = self.editing() else {
            return;
        };
        // The worktree the gesture was made in, and not the one selected when
        // the dialog is answered: one browses while a question is open.
        let (worktree, path) = (editing.worktree.clone(), editing.path.clone());
        self.ask_close_file(worktree, path, window, cx);
    }

    /// The gestures that close a tab: the cross, the wheel button, `Ctrl+W`.
    ///
    /// **Unsaved text is asked about**, with the three answers the question
    /// really has — write it, drop it, or stay. A two-button dialog would have
    /// made "close" mean "lose", which is the reading nobody wants to make at
    /// speed on a tab bar.
    ///
    /// The dock's own removal does **not** come through here: by the time
    /// `on_removed` fires the tab is gone, and a cancelled dialogue would leave
    /// an editor nothing renders. It is the same limit the terminals have, and
    /// the reason both draw their cross themselves.
    pub(super) fn ask_close_file(
        &mut self,
        worktree: PathBuf,
        path: PathBuf,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let dirty = self
            .editors(&worktree)
            .and_then(|tabs| tabs.by_path(&path))
            .is_some_and(|editing| editing.dirty);
        if !dirty {
            self.close_file(worktree, path, window, cx);
            return;
        }
        let label = SharedString::from(path.display().to_string());
        let entity = cx.entity();
        window.open_dialog(cx, move |dialog, _window, _cx| {
            let (entity, label, worktree) = (entity.clone(), label.clone(), worktree.clone());
            let path = path.clone();
            // The button that drops the work, and the one Enter makes: saving.
            // Enter is the answer one gives without reading, so it is the one
            // that keeps what was typed.
            let dropping = {
                let (entity, worktree, path) = (entity.clone(), worktree.clone(), path.clone());
                move |window: &mut Window, cx: &mut App| {
                    let (worktree, path) = (worktree.clone(), path.clone());
                    entity.update(cx, |this, cx| this.close_file(worktree, path, window, cx));
                }
            };
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
                .footer(super::dialogs::choose(
                    tr!("editor-discard-save"),
                    tr!("editor-discard-drop"),
                    dropping,
                ))
                .on_ok(move |_, window, cx| {
                    let (worktree, path) = (worktree.clone(), path.clone());
                    entity.update(cx, |this, cx| {
                        this.save_tab(&path, cx);
                        this.lsp_editor_saved();
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
        let Some((path, line)) = self.diff_place(cx) else {
            return;
        };
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

    // — Dragging into the tree ————————————————————————————————

    /// Files dropped on the tree from the desktop's file manager.
    ///
    /// A **copy**, and never a move: what the drop names belongs to somewhere
    /// else — a download, an export from a design tool — and one gesture of the
    /// hand has no business emptying the folder it came from. The exception is
    /// a path that is already inside this worktree, dropped in from a file
    /// manager opened on the project: there the gesture reads as the one the
    /// tree makes on its own, and it moves.
    pub(super) fn drop_external(
        &mut self,
        dir: PathBuf,
        paths: Vec<PathBuf>,
        cx: &mut Context<Self>,
    ) {
        let Some(worktree) = self.active.clone() else {
            return;
        };
        for source in paths {
            // On Windows the drop enters from *this* world while the workers
            // are in the distribution: the path changes worlds here, like the
            // target of a CSV export. What the distribution cannot reach — a
            // network share — is refused rather than copied nowhere.
            let source = if cfg!(windows) {
                match crate::wslpath::for_server(&source) {
                    Some(path) => path,
                    None => {
                        self.announce(tr!("files-drop-unreachable"), cx);
                        continue;
                    }
                }
            } else {
                source
            };
            let Some(to) = files::drop_target(&dir, &source) else {
                continue;
            };
            let op = match source.strip_prefix(&worktree) {
                Ok(inside) => match move_within(inside, &to) {
                    Some(op) => op,
                    None => continue,
                },
                Err(_) => files::Op::Import { from: source, to },
            };
            self.land_drop(&dir, op, cx);
        }
    }

    /// A row dragged from the tree and dropped on a folder of the same tree.
    pub(super) fn drop_entry(&mut self, from: PathBuf, dir: PathBuf, cx: &mut Context<Self>) {
        let Some(to) = files::drop_target(&dir, &from) else {
            return;
        };
        let Some(op) = move_within(&from, &to) else {
            return;
        };
        self.land_drop(&dir, op, cx);
    }

    /// Sends the operation, and opens the folder it lands in.
    ///
    /// Opening it is the whole answer to "where did it go": the list is re-read
    /// a git command later, and a file that lands in a closed folder lands
    /// nowhere as far as the eye is concerned. The cursor follows for the same
    /// reason.
    fn land_drop(&mut self, dir: &Path, op: files::Op, cx: &mut Context<Self>) {
        let landing = op.target().to_path_buf();
        self.file_op(op, cx);
        if let Some(explorer) = self.explorer() {
            explorer.reveal(&landing);
            explorer.expanded.insert(dir.to_path_buf());
            explorer.cursor = Some(landing);
            explorer.drag_hover = None;
            explorer.rebuild();
        }
    }

    /// A drag hovering over a closed folder opens it.
    ///
    /// The gesture of every file manager: one drags towards a folder that is
    /// not open, and staying on it is how one asks to go in. It is deferred —
    /// a drag crosses a dozen rows on its way — and the deferral is what makes
    /// crossing free.
    pub(super) fn hover_drop_dir(&mut self, path: PathBuf, cx: &mut Context<Self>) {
        let Some(explorer) = self.explorer() else {
            return;
        };
        if explorer.drag_hover.as_deref() == Some(path.as_path()) {
            return; // the same folder, one mouse move later
        }
        explorer.drag_hover = Some(path.clone());
        if explorer.expanded.contains(&path) {
            return;
        }
        cx.spawn(async move |this, cx| {
            cx.background_executor().timer(DRAG_HOVER).await;
            let _ = this.update(cx, |this, cx| {
                let Some(explorer) = this.explorer() else {
                    return;
                };
                if explorer.drag_hover.as_deref() != Some(path.as_path()) {
                    return; // the hand moved on
                }
                if explorer.expanded.insert(path) {
                    explorer.rebuild();
                    cx.notify();
                }
            });
        })
        .detach();
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
        let files = explorer.files.clone();
        let cursor = explorer.cursor.clone();
        let count = rows.len();
        // The git status is already filed by path (`index_file_status`):
        // showing it costs only one lookup per visible row, and it is what
        // makes the difference between a file list and a project explorer.
        let status = explorer.status.clone();
        let open = self.editing().map(|editing| editing.path.clone());
        let entity = cx.entity();
        let look = Look::of(cx);
        let edge = cx.theme().primary;

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
                    // The panel itself is a drop target, for the worktree's
                    // root: a project whose root holds three files leaves most
                    // of the column empty, and aiming at a row there would be
                    // aiming at nothing. The border is always reserved —
                    // one that appears would move every row one pixel.
                    .border_1()
                    .border_color(gpui::transparent_black())
                    .drag_over::<gpui::ExternalPaths>(move |style, _, _, _| {
                        style.border_color(edge)
                    })
                    .drag_over::<DraggedEntry>(move |style, _, _, _| style.border_color(edge))
                    .on_drop({
                        let entity = entity.clone();
                        move |paths: &gpui::ExternalPaths, _window, cx| {
                            let paths = paths.paths().to_vec();
                            entity.update(cx, |this, cx| {
                                this.drop_external(PathBuf::new(), paths, cx)
                            });
                        }
                    })
                    .on_drop({
                        let entity = entity.clone();
                        move |entry: &DraggedEntry, _window, cx| {
                            let from = entry.path.clone();
                            entity.update(cx, |this, cx| this.drop_entry(from, PathBuf::new(), cx));
                        }
                    })
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
    pub(super) fn render_jump_buttons(&self, cx: &mut Context<Self>) -> Option<impl IntoElement> {
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
        // Whether there is a server for this file, without cloning the list of
        // them at every frame: see `lsp_serves`.
        if !self.lsp_serves(&editing.worktree, &editing.path, cx) {
            return None;
        }
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

    /// The band that shows what a hunk replaced, and puts it back.
    ///
    /// The old lines are shown as code — same family and size as the editor
    /// under it — because that is what they are, and reading them in the
    /// interface's proportional font would misalign every indentation. It
    /// scrolls rather than grows: a hunk can be two hundred lines long, and a
    /// band that pushed the editor off the bottom of the panel would take the
    /// file away to show a piece of it.
    fn render_hunk_peek(
        &self,
        row: usize,
        kind: crate::ui::hunks::Kind,
        old: String,
        code_size: Pixels,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        use crate::ui::hunks::Kind;
        let mono = cx.theme().mono_font_family.clone();
        let diff = crate::ui::theme::DiffColors::of(cx);
        let (title, tint) = match kind {
            Kind::Added => (tr!("editor-hunk-added"), diff.added_fg),
            Kind::Removed => (tr!("editor-hunk-removed"), diff.removed_fg),
            Kind::Changed => (tr!("editor-hunk-changed"), cx.theme().primary),
        };
        v_flex()
            .w_full()
            // The editor beside it is `flex_1`, so without this the band is
            // what a column short of room takes the room from — and a band
            // squeezed to nothing is a band whose buttons cannot be pressed.
            .flex_shrink_0()
            .border_b_1()
            .border_color(cx.theme().border)
            .bg(cx.theme().secondary)
            .child(
                h_flex()
                    .h(crate::ui::theme::bar_height(cx))
                    .w_full()
                    .px_2()
                    .gap_2()
                    .items_center()
                    .child(div().w(HUNK_MARK_WIDTH).h_4().bg(tint))
                    .child(div().text_xs().child(title))
                    .child(div().flex_1())
                    .child(
                        Button::new("editor-hunk-rollback")
                            .ghost()
                            .xsmall()
                            .icon(icon("undo-2"))
                            .label(tr!("editor-hunk-rollback"))
                            .tooltip(tr!("editor-hunk-rollback-help"))
                            .on_click(cx.listener(move |this, _, window, cx| {
                                let Some((worktree, path)) =
                                    this.editing().map(|e| (e.worktree.clone(), e.path.clone()))
                                else {
                                    return;
                                };
                                this.rollback_hunk(worktree, path, row, window, cx);
                            })),
                    )
                    .child(
                        Button::new("editor-hunk-close")
                            .ghost()
                            .xsmall()
                            .icon(icon("x"))
                            .tooltip(tr!("editor-close"))
                            .on_click(cx.listener(move |this, _, _, cx| {
                                let Some((worktree, path)) =
                                    this.editing().map(|e| (e.worktree.clone(), e.path.clone()))
                                else {
                                    return;
                                };
                                this.open_hunk(&worktree, &path, row, cx);
                            })),
                    ),
            )
            .when(!old.is_empty(), |el| {
                el.child(
                    div()
                        .id("editor-hunk-old")
                        .max_h(px(160.))
                        .w_full()
                        .px_2()
                        .pb_1()
                        .overflow_y_scroll()
                        .font_family(mono)
                        .text_size(code_size)
                        .bg(diff.removed_bg)
                        .child(old),
                )
            })
    }

    /// The file's own bar: what is open, whether it is unsaved, and the four
    /// gestures that act on the file rather than on its text.
    ///
    /// A method of its own because `render_editor` is the whole panel — the
    /// bar, the hunk band, the modal harness and the wheel — and a bar is the
    /// piece of it that has nothing to do with any of the others.
    fn render_editor_bar(
        &mut self,
        path: &Path,
        dirty: bool,
        mono: SharedString,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let label = SharedString::from(path.display().to_string());
        let for_external = path.to_path_buf();
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
                    .on_click(cx.listener(|this, _, window, cx| this.close_editor(window, cx))),
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
        let prompt = editing.host.vim.prompt();
        let pending = editing.host.vim.pending().to_string();
        // The surface is named by the file this panel holds, and not by "the
        // file being edited": the dock can show two of them at once, and the
        // active tab then alternates between them from frame to frame. See
        // `ui::surface`.
        let surface = Surface::File(path.clone());
        // The smoothing advances by one frame, as the diff's does.
        self.advance_surface_scroll(&surface, &input, window, cx);
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
        self.sync_block_cursor(&surface, vim, cx);
        // The occurrences of the last search, lit as `Ctrl+F` lights them:
        // see `sync_search_matches`.
        self.sync_search_matches(&surface, vim, cx);
        // And the occurrence the bar has just jumped to, put in the middle of
        // the panel rather than on its edge: see `centre_search_match`.
        self.centre_search_match(&surface, cx);
        // Read before the editor is built, for its context menu: the closure
        // that builds it cannot read the state itself. See `editor_menu`.
        let has_selection = !input.read(cx).selected_range().is_empty();
        let mono = cx.theme().mono_font_family.clone();
        // The editor is code, like the diff on the screen next door: same
        // family, same size. Without saying so it inherits the interface's
        // proportional font, where an indentation no longer lines up.
        let code_size = px(crate::ui::settings::Settings::global(cx).diff_font_size);
        // What the gutter's strip was clicked to show. A band under the file
        // bar and **not a popover on the line**: the editor's lines come and go
        // with the scroll, so an anchor on one is an anchor that the first
        // wheel notch carries off screen — the ruling `notes_view` already
        // made for an annotated row.
        let peek = self.editing().and_then(|editing| {
            let row = editing.hunk_open?;
            // `last_line` and not a fresh count: this runs at every frame for
            // as long as the band is open, and the buffer's line count is
            // written with the hunks it is read against.
            let hunk = editing
                .hunks
                .iter()
                .find(|hunk| hunk.covers(row, editing.last_line))?;
            Some((row, hunk.kind(), hunk.old.join("\n")))
        });
        Some(
            v_flex()
                .size_full()
                .child(self.render_editor_bar(&path, dirty, mono.clone(), cx))
                .when_some(peek, |el, (row, kind, old)| {
                    el.child(self.render_hunk_peek(row, kind, old, code_size, cx))
                })
                .child(
                    div()
                        .id("editor-zoom")
                        // Three keys of navigation live here and nowhere else:
                        // see `shortcuts::EDITOR_PREDICATE`.
                        .key_context(crate::ui::shortcuts::editor_context(vim))
                        .relative()
                        .flex_1()
                        .min_h_0()
                        // `Ctrl`+click follows a symbol, and the two halves of
                        // it are here: the flag is cleared before the editor
                        // sees the click, and read after — see
                        // `on_surface_definition_click`.
                        .capture_any_mouse_down(cx.listener(
                            |this, _: &gpui::MouseDownEvent, _, _| {
                                this.followed_definition = false;
                            },
                        ))
                        .on_mouse_down(gpui::MouseButton::Left, {
                            let surface = surface.clone();
                            cx.listener(move |this, event, window, cx| {
                                this.on_surface_definition_click(&surface, event, window, cx)
                            })
                        })
                        // The four keys vim takes before the editor sees
                        // them, installed only when the mode is on: see
                        // `surface::vim_capture`.
                        .map(|el| match vim {
                            true => crate::ui::surface::vim_capture(el, surface.clone(), cx),
                            false => el,
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
                                // The menu is **rebuilt whole**: a custom
                                // builder replaces the editor's own, which is
                                // where cut, copy and paste come from.
                                //
                                // **The selection is read here and captured**,
                                // never inside the closure: the builder runs
                                // from the editor's own right-click handler,
                                // that is to say while its state is being
                                // updated, and reading the entity there is a
                                // panic. The closure is rebuilt on every frame,
                                // so what it carries is a frame old at worst.
                                .context_menu({
                                    let path = path.clone();
                                    let selection = has_selection;
                                    move |menu, _window, cx| editor_menu(menu, selection, &path, cx)
                                })
                                .h_full(),
                        )
                        // The wheel, taken before the editor sees it: see
                        // `surface::wheel_capture`.
                        .child(crate::ui::surface::wheel_capture(surface, cx)),
                )
                // vim's status line, at the foot of the file it belongs to: the
                // mode, and the `:` or `/` line being written. See
                // `render_vim_status`.
                .when(vim, |el| {
                    el.child(self.render_vim_status(mode, prompt, &pending, cx))
                }),
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
    muted: gpui::Hsla,
    accent: gpui::Hsla,
    /// The colour of the folder a drop is about to land in.
    drop: gpui::Hsla,
    /// The vertical rule of one indentation level.
    guide: gpui::Hsla,
    folder: gpui::Hsla,
}

impl Look {
    fn of(cx: &gpui::App) -> Self {
        Self {
            height: crate::ui::theme::row_height(cx),
            muted: cx.theme().muted_foreground,
            accent: cx.theme().accent,
            // Not the accent again: a drop target has to be told apart from the
            // row one happens to be over, and the two are on screen together.
            drop: cx.theme().primary.opacity(0.35),
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
                // The band crosses the panel, whatever the name is long: it says
                // which row one is on, and a pill drawn around the text says
                // instead where the text ends.
                .w_full()
                .pl_1()
                .pr(crate::ui::theme::scroll_gutter())
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
                .map(|el| accepts_drops(el, &entity, &path, true, look))
                .map(|el| draggable(el, &path))
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
                .w_full()
                .pl_1()
                .pr(crate::ui::theme::scroll_gutter())
                .items_center()
                .cursor_pointer()
                // Open and under the cursor are two things: one browses the tree
                // with the keyboard without leaving the file being reviewed, and
                // showing only one of the two would lose the other.
                .when(is_open, |el| el.bg(look.accent))
                .when(at_cursor && !is_open, |el| el.bg(look.accent.opacity(0.5)))
                .hover(|s| s.bg(look.accent.opacity(0.4)))
                // One click shows the file, two keep it: the preview tab is
                // what a single click fills, and a double click is the gesture
                // by which one says "this one I am staying in". The count comes
                // from the event — gpui counts the clicks for us — so there is
                // no second handler and no delay waiting for one.
                .on_click(move |event: &gpui::ClickEvent, window, cx| {
                    let kept = event.click_count() > 1;
                    open_entity.update(cx, |this, cx| {
                        this.focus_project_tree(for_open.clone(), window, cx);
                        if kept {
                            this.open_in_editor(for_open.clone(), cx);
                        } else {
                            this.browse_in_editor(for_open.clone(), cx);
                        }
                    });
                })
                // A file takes a drop too, and hands it to the folder it lives
                // in: aiming at a folder's own line, in a list where a folder
                // holding thirty files is one row in thirty, is a precision
                // nobody should have to have.
                .map(|el| accepts_drops(el, entity, &path, false, look))
                .map(|el| draggable(el, &path))
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

/// Makes a row somewhere a drag can land.
///
/// Two drags at once, and the same row takes both: the desktop's files — gpui
/// turns a platform drop into an `ExternalPaths` drag, so a file manager's drop
/// is read with the very same listeners — and a row of this same tree.
fn accepts_drops(
    row: gpui::Stateful<gpui::Div>,
    entity: &Entity<ClaudhubApp>,
    path: &Path,
    is_dir: bool,
    look: &Look,
) -> gpui::Stateful<gpui::Div> {
    let dir = drop_dir(path, is_dir);
    let colour = look.drop;
    let (dropping, moving) = (entity.clone(), entity.clone());
    let (outside, inside) = (entity.clone(), entity.clone());
    let (for_drop, for_move) = (dir.clone(), dir);
    let (from_outside, from_inside) = (path.to_path_buf(), path.to_path_buf());
    row.drag_over::<gpui::ExternalPaths>(move |style, _, _, _| style.bg(colour))
        .drag_over::<DraggedEntry>(move |style, _, _, _| style.bg(colour))
        .on_drop(move |paths: &gpui::ExternalPaths, _window, cx| {
            let paths = paths.paths().to_vec();
            dropping.update(cx, |this, cx| {
                this.drop_external(for_drop.clone(), paths, cx)
            });
        })
        .on_drop(move |entry: &DraggedEntry, _window, cx| {
            let from = entry.path.clone();
            moving.update(cx, |this, cx| this.drop_entry(from, for_move.clone(), cx));
        })
        // A folder opens when a drag stays on it. The bounds are checked here
        // and not by gpui: `on_drag_move` fires on **every** mouse move of the
        // drag, wherever it is, so without this every row of the panel would
        // claim to be the one under the hand.
        .when(is_dir, |row| {
            row.on_drag_move(
                move |event: &gpui::DragMoveEvent<gpui::ExternalPaths>, _, cx| {
                    if event.bounds.contains(&event.event.position) {
                        let path = from_outside.clone();
                        outside.update(cx, |this, cx| this.hover_drop_dir(path, cx));
                    }
                },
            )
            .on_drag_move(move |event: &gpui::DragMoveEvent<DraggedEntry>, _, cx| {
                if event.bounds.contains(&event.event.position) {
                    let path = from_inside.clone();
                    inside.update(cx, |this, cx| this.hover_drop_dir(path, cx));
                }
            })
        })
}

/// Makes a row something one can pick up.
///
/// The ghost is the row's own name in a small card: the platform draws nothing
/// for an internal drag, and a drag with nothing following the pointer reads as
/// a click that did not take.
fn draggable(row: gpui::Stateful<gpui::Div>, path: &Path) -> gpui::Stateful<gpui::Div> {
    row.on_drag(
        DraggedEntry {
            path: path.to_path_buf(),
        },
        |entry, _offset, _window, cx| {
            let entry = entry.clone();
            cx.new(|_| entry)
        },
    )
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
/// The editor's right-click menu.
///
/// The items the editor itself offers, plus the one gesture that is ours: the
/// history of the lines under the pointer. They are `NativeMenu` actions and
/// not closures — the menu carries a `gpui::Action` and nothing else, which is
/// why `ShowLineHistory` names its file rather than relying on "the file being
/// edited".
fn editor_menu(
    menu: gpui_component::native_menu::NativeMenu,
    // Whether the editor had a selection when the frame was painted — see the
    // call site: it cannot be read from here.
    selection: bool,
    path: &Path,
    cx: &gpui::App,
) -> gpui_component::native_menu::NativeMenu {
    use gpui_base::input as base;
    menu.menu(
        tr!("shortcut-goto-definition"),
        Box::new(base::GoToDefinition),
    )
    .separator()
    .menu(
        tr!("history-selection"),
        Box::new(crate::ui::shortcuts::ShowLineHistory {
            path: path.to_path_buf(),
        }),
    )
    .separator()
    .menu_with_disabled(tr!("editor-cut"), !selection, Box::new(base::Cut))
    .menu_with_disabled(tr!("editor-copy"), !selection, Box::new(base::Copy))
    .menu_with_disabled(
        tr!("editor-paste"),
        cx.read_from_clipboard().is_none(),
        Box::new(base::Paste),
    )
    .separator()
    .menu(tr!("editor-select-all"), Box::new(base::SelectAll))
}

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

/// The width of the change strip, inside the column the gutter gives it.
const HUNK_MARK_WIDTH: Pixels = px(3.);

/// What the strip leaves between itself and the first character.
///
/// The strip's column is the gutter's own right margin, so drawing at its very
/// edge puts the filet against the text — it then reads as a rule the line is
/// written on rather than as a mark beside it. Backing off a few pixels gives
/// it air on the right and, since it moves left inside a margin that was empty
/// anyway, closes a little of the gap on the other side at the same time.
const HUNK_MARK_GAP: Pixels = px(4.);

/// Paints one line's change mark, beside the text.
///
/// A quad and not a glyph: the strip is a shape, and asking the text system for
/// one would put it on the font's baseline rather than on the line's edge. The
/// colours are read here, at paint time, so a theme change is picked up without
/// anything having to say that it happened.
fn paint_hunk_mark(
    kind: crate::ui::hunks::Kind,
    bounds: gpui::Bounds<Pixels>,
    window: &mut Window,
    cx: &mut App,
) {
    use crate::ui::hunks::Kind;
    let diff = crate::ui::theme::DiffColors::of(cx);
    let colour = match kind {
        Kind::Added => diff.added_fg,
        Kind::Removed => diff.removed_fg,
        Kind::Changed => gpui_component::ActiveTheme::theme(cx).primary,
    };
    // A deletion has no lines of its own, so it cannot have the full height:
    // it is drawn as a stub on the boundary it sits on, which is what says
    // "between these two lines" rather than "this line".
    let (width, height) = match kind {
        Kind::Removed => (HUNK_MARK_WIDTH * 2., px(3.)),
        _ => (HUNK_MARK_WIDTH, bounds.size.height),
    };
    let rect = gpui::Bounds::new(
        gpui::point(
            bounds.origin.x + bounds.size.width - width - HUNK_MARK_GAP,
            bounds.origin.y,
        ),
        gpui::size(width, height),
    );
    window.paint_quad(gpui::fill(rect, colour));
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The tab limit's whole rule. Each clause here was a way of losing
    /// something: the oldest read is the one one has finished with, unsaved text
    /// is not a thing to close for a glance, and closing the file about to open
    /// would take down the tab being reused.
    #[test]
    fn the_tab_that_makes_room_is_the_oldest_one_can_spare() {
        let tabs = [
            (Path::new("a.rs"), false, 1),
            (Path::new("b.rs"), false, 2),
            (Path::new("c.rs"), false, 3),
        ];
        assert_eq!(
            oldest_spare(tabs.iter().copied(), Path::new("d.rs")),
            Some(Path::new("a.rs"))
        );
        // The file about to open is never the one that closes for it.
        assert_eq!(
            oldest_spare(tabs.iter().copied(), Path::new("a.rs")),
            Some(Path::new("b.rs"))
        );
        // Unsaved text is passed over, however old.
        let dirty = [(Path::new("a.rs"), true, 1), (Path::new("b.rs"), false, 2)];
        assert_eq!(
            oldest_spare(dirty.iter().copied(), Path::new("d.rs")),
            Some(Path::new("b.rs"))
        );
        // Nothing to spare: the bar goes over the limit rather than lose work.
        let all_dirty = [(Path::new("a.rs"), true, 1)];
        assert_eq!(
            oldest_spare(all_dirty.iter().copied(), Path::new("d.rs")),
            None
        );
    }

    /// What a drop on a row asks for. This is the whole decision of dragging
    /// inside the tree, and the only part of it that can be wrong in silence:
    /// a folder dropped into its own child would move a tree into itself.
    #[test]
    fn a_row_hands_a_drop_to_its_folder() {
        assert_eq!(drop_dir(Path::new("src/ui"), true), PathBuf::from("src/ui"));
        assert_eq!(
            drop_dir(Path::new("src/main.rs"), false),
            PathBuf::from("src")
        );
        // A file of the root gives the root, which is the empty path.
        assert_eq!(drop_dir(Path::new("README.md"), false), PathBuf::new());
    }

    /// The greying of what `.gitignore` leaves out, read in one pass over two
    /// sorted lists. It is the flag a folder's whole subtree is tested against,
    /// so what it must not do is miss one.
    #[test]
    fn what_git_ignores_is_flagged_in_one_pass() {
        let files: Vec<PathBuf> = ["README.md", "src/main.rs", "target/a.rs", "target/b.rs"]
            .iter()
            .map(PathBuf::from)
            .collect();
        let ignored: Vec<PathBuf> = ["target/a.rs", "target/b.rs"]
            .iter()
            .map(PathBuf::from)
            .collect();
        assert_eq!(
            excluded_flags(&files, &ignored),
            vec![false, false, true, true]
        );
        // Nothing ignored is no flags at all, which is also what says "nothing
        // to grey": the panel never asked for them.
        assert!(excluded_flags(&files, &[]).is_empty());
    }

    #[test]
    fn a_move_that_means_nothing_is_not_one() {
        let from = Path::new("src/ui/tree.rs");
        assert_eq!(
            move_within(from, Path::new("src/tree.rs")),
            Some(files::Op::Rename {
                from: from.to_path_buf(),
                to: PathBuf::from("src/tree.rs"),
            })
        );
        // Back where it already is.
        assert_eq!(move_within(from, from), None);
        // A folder into itself, and into one of its own children.
        let dir = Path::new("src/ui");
        assert_eq!(move_within(dir, dir), None);
        assert_eq!(move_within(dir, Path::new("src/ui/ui")), None);
    }
}
