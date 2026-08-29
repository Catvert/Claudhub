//! The root entity: the window's state and the event pump.
//!
//! The sub-views are not separate entities but `impl ClaudhubApp` blocks spread
//! across files (`sidebar`, `review`, `branches`). Everything they show comes
//! from the same state, and passing it between entities would cost more code
//! than it saves. The terminals are the exception: they have their own lifetime
//! and are `Entity<TerminalView>`.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::rc::Rc;

use gpui::{
    div, prelude::*, px, App, Context, Entity, FocusHandle, Focusable, Render, SharedString, Window,
};
use gpui_component::{
    button::{Button, ButtonVariants},
    dock::{DockArea, DockSkin},
    h_flex,
    input::{InputState, TextareaState},
    select::{SearchableVec, SelectEvent, SelectState},
    separator::Separator as Divider,
    v_flex, ActiveTheme, Root, Sizable, WindowExt,
};

use crate::git::{Branch, Commit, DiffFile, DiffRange, GraphRow, LogRange, Status, Worktree};
use crate::runtime::{self, Action, Cmd, Evt};
use crate::tr;

use crate::ui::base_select::BaseChoice;
use crate::ui::diff_view::Rendered;
use crate::ui::icons::icon;
use crate::ui::settings::Settings;
use crate::ui::store::Store;
use crate::ui::terminal_view::OpenTerminal;

/// Version of the saved layout. To be incremented when the panels change name or
/// nature, so gpui-component discards a layout it could no longer rebuild.
// 9: `DockAreaState`'s schema changed with the dock rework. A layout written by
// the previous version would read back crooked rather than be plainly refused,
// which gives a window full of empty frames.
// 10: the "Databases" panel appeared. A layout written before it does not
// contain it, and nothing would add it: the view would exist with no tab.
// 11: the screens. The file no longer carries one layout but one per screen, and the
// central panel split into three — a file from before has neither the shape nor
// the names needed.
// 13: the repositories and the branches stopped being panels — the top bar
// carries them as pickers. A layout from before names two panels the registry
// can no longer build, and three screens lost their left column with them.
// 14: the notes left the tabs for the bottom third of the review's column. A
// saved layout would keep them where they were, which is the one place this
// change is about.
// 19: the SQL history did the same on the databases screen, and it is the same
// bargain — the only lever there is throws away every screen's arrangement,
// which is dear, and without it the new default is one nobody would ever see.
// 23: the tests panel joined the review's bottom third, the home's column and
// the editing screen's sidebar.
// 24: the tests screen arrived, and the run panel with it.
// 25: the screens went. One area instead of nine, the panels shared out
// between three tool zones and a centre of documents, and the file's shape
// went with them — a table keyed by screen names builds nothing here.
const LAYOUT_VERSION: usize = 25;

/// The window's saved arrangement.
///
/// One area now, so one state. The version wraps it: panels appear, disappear
/// and are renamed, and a tree of unknown names builds nothing.
///
/// **This version is the gate**, and it is the only one consulted on reading.
/// The number handed to `DockSkin::dock_area` is written into the area's own
/// dump so the blob describes itself, and nothing ever compares it.
#[derive(serde::Serialize, serde::Deserialize, Default)]
struct Layouts {
    version: Option<usize>,
    area: Option<gpui_component::dock::DockAreaState>,
}

fn load_layouts() -> Layouts {
    let Some(path) = crate::ui::settings::layout_path() else {
        return Layouts::default();
    };
    let Ok(text) = std::fs::read_to_string(path) else {
        return Layouts::default();
    };
    match serde_json::from_str::<Layouts>(&text) {
        // An unreadable layout is no reason not to start: we begin again from
        // the default one.
        Ok(layouts) if layouts.version == Some(LAYOUT_VERSION) => layouts,
        Ok(_) => Layouts::default(),
        Err(e) => {
            log::warn!("disposition illisible : {e}");
            Layouts::default()
        }
    }
}

/// Takes out of a layout the panels that must not be in it.
///
/// Two callers, and they prune at the two opposite ends. **Before writing**:
/// the terminals, the one panel whose content is a process — `layout.json` is
/// read on the next launch, when that process is long gone. **After reading**:
/// every name the dock's registry cannot build — a plugin uninstalled between
/// two sessions, a panel of ours that has gone. Left in, they come back as an
/// empty frame or as the registry's own message printed across the screen, and
/// they come back at **every** start; `LAYOUT_VERSION` answers only by throwing
/// away every screen's arrangement, which is a heavy price for one dead name. A
/// group left empty by the pruning goes too, otherwise the window would reopen
/// with a bare tab bar.
///
/// Returns whether the node itself should be dropped by its parent.
/// Points a saved layout's tab group back at one of its panels.
///
/// A group remembers the tab that was on screen when the window closed, and
/// that is right for the tabs of the work — one comes back to the history one
/// was reading. It is wrong for the one tab a session should never *open* on:
/// the Git screen answers "what is there to review", and a plugin's board is
/// not that answer. Hence at **startup only**, and not on every visit to the
/// screen: within a session the tab belongs to whoever clicked it, which is the
/// rule the review's own base follows (`range_chosen`).
///
/// Answers whether it found the panel, which is what stops the walk.
fn open_on(state: &mut gpui_component::dock::PanelState, name: &str) -> bool {
    if let gpui_component::dock::PanelInfo::Tabs { active_index } = &mut state.info {
        if let Some(ix) = state
            .children
            .iter()
            .position(|child| child.panel_name == name)
        {
            *active_index = ix;
            return true;
        }
    }
    state.children.iter_mut().any(|child| open_on(child, name))
}

fn prune(state: &mut gpui_component::dock::PanelState, doomed: &impl Fn(&str) -> bool) -> bool {
    // **Only a leaf is judged by its name.** A container carries one of the
    // dock's own — `TabPanel`, `StackPanel` — which is not one of ours and is
    // therefore not in the registry we ask: asked about the root, the reading
    // side's predicate said "doomed" and the walk stopped there, so **nothing
    // was ever pruned**. The symptom is what the pruning exists to prevent —
    // "the `ClaudhubMultiplexer` panel type is not registered" across the
    // screen, at every start, for a panel that stopped existing the day that
    // screen became the terminals' own dock.
    if state.children.is_empty() {
        return doomed(&state.panel_name);
    }
    let doomed: Vec<usize> = state
        .children
        .iter_mut()
        .enumerate()
        .filter_map(|(ix, child)| prune(child, doomed).then_some(ix))
        .collect();
    for ix in doomed.into_iter().rev() {
        state.children.remove(ix);
        // A stack's sizes are positional: dropping a child without its size
        // shifts every size that follows on to the wrong pane.
        if let gpui_component::dock::PanelInfo::Stack { sizes, .. } = &mut state.info {
            if ix < sizes.len() {
                sizes.remove(ix);
            }
        }
    }
    // A split left with **one** slot is not a split, and leaving it as one is
    // not harmless: the size it keeps is the height that slot had *while the
    // pruned panel was beside it* — a number measured in a layout that no
    // longer exists. Read back, it pins the survivor rigidly, and the next
    // panel opening beneath it — a terminal, always — gets whatever is left of
    // a container that number no longer describes. Collapsing the wrapper
    // drops the stale size with it.
    if state.children.len() == 1
        && matches!(state.info, gpui_component::dock::PanelInfo::Stack { .. })
    {
        let child = state.children.remove(0);
        *state = child;
        return false;
    }
    // An empty container is not a container: only a leaf legitimately has no
    // child, and a leaf is not what we have just emptied.
    state.children.is_empty()
}

/// The same pruning, for one of the three side zones.
///
/// **`DockState` keeps its fields to itself, but its constructor and its four
/// readers are enough**: a zone is taken apart and put back together around a
/// pruned tree. That is what brings a panel dragged into a side zone within
/// reach — for a long time it was not, and the comment saying so was right
/// only about the direct route.
///
/// It stops being a detail the moment the terminals live in the bottom zone:
/// without this, `layout.json` keeps `ClaudhubTerminal` under `bottom_dock`,
/// nothing can build one back — a terminal is a **process** — and the next
/// start paints "panel type is not registered" across the strip, every time.
///
/// A zone whose tree is doomed whole becomes `None` rather than an empty
/// `DockState`: an empty zone read back would reopen on nothing, and nothing
/// is lost by letting it go — `add_panel_view` makes the region again for
/// whoever names it.
fn prune_dock(dock: &mut Option<gpui_component::dock::DockState>, doomed: &impl Fn(&str) -> bool) {
    let Some(zone) = dock.as_ref() else {
        return;
    };
    let mut tree = zone.panel().clone();
    let (placement, size, open) = (zone.placement(), zone.size(), zone.open());
    if prune(&mut tree, doomed) {
        *dock = None;
        return;
    }
    *dock = Some(gpui_component::dock::DockState::new(
        tree, placement, size, open,
    ));
}

/// `open_on`, for one of the three side zones — taken apart and put back the
/// way `prune_dock` does, and for the same reason.
fn open_dock_on(dock: &mut Option<gpui_component::dock::DockState>, name: &str) {
    let Some(zone) = dock.as_ref() else {
        return;
    };
    let mut tree = zone.panel().clone();
    if !open_on(&mut tree, name) {
        return;
    }
    *dock = Some(gpui_component::dock::DockState::new(
        tree,
        zone.placement(),
        zone.size(),
        zone.open(),
    ));
}

fn save_layouts(layouts: &Layouts) {
    let Some(path) = crate::ui::settings::layout_path() else {
        return;
    };
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    match serde_json::to_string_pretty(layouts) {
        Ok(json) => {
            if let Err(e) = std::fs::write(&path, json) {
                log::warn!("writing the layout: {e}");
            }
        }
        Err(e) => log::warn!("serialising the layout: {e}"),
    }
}

/// What the review shows for a given worktree.
///
/// One state per worktree, and not a single global state: switching from one
/// worktree to another to compare is the tool's central gesture, and it must not
/// cost the loss of the file being read.
pub struct ReviewState {
    /// A language server runs on this worktree. Off by default, remembered by
    /// the store — see `ui::lsp`.
    pub lsp: bool,
    /// The range of the diff shown on the right. It is no longer the review's
    /// "current" range — each panel has its own — but that of the file last
    /// clicked, whichever panel it came from.
    pub range: DiffRange,
    pub status: Status,
    /// The touched files, **by range**. Two panels show two lists at the same
    /// time: a single list would make one flicker every time the other reloads.
    pub files: HashMap<DiffRange, Vec<DiffFile>>,
    /// Ranges whose list has been asked for and has not come back. It is the
    /// panel that asks, and it asks at render time: without this guard, every
    /// frame would restart the command.
    pub pending_files: std::collections::HashSet<DiffRange>,
    pub selected: Option<PathBuf>,
    /// The displayed diff, with everything derived from it. An `Rc` because the
    /// render has to capture it in the virtualised list's closure, and copying
    /// several thousand of its lines per frame would cancel out the benefit of
    /// virtualisation.
    pub diff: Option<std::rc::Rc<Rendered>>,
    /// The unstaged remainder of the selected file, when it is partially
    /// staged: what the next commit would leave behind, shown under the diff
    /// so those hunks can still be added. Keyed by path because the answer
    /// arrives after a git command — the selection may have moved on.
    pub unstaged: Option<(PathBuf, std::rc::Rc<crate::git::FileDiff>)>,
    /// The lines selected in the diff: the anchor and the head, as indices into
    /// the flattened list. Two indices and not a sorted range, because it is the
    /// direction of the gesture that decides which one moves on the next
    /// extension.
    pub diff_selection: Option<(usize, usize)>,
    /// Folders collapsed in the file list.
    ///
    /// One set for both groups: collapsing `src/ui` means collapsing that
    /// folder, not "that folder among the tracked files".
    pub collapsed: std::collections::HashSet<PathBuf>,
    /// The branch review's comparison base, guessed at opening.
    pub base: Option<String>,
    /// The history and its graph, loaded on demand — opening a worktree must not
    /// pay for a `git log` nobody will look at.
    pub history: Option<std::rc::Rc<History>>,
    pub history_range: LogRange,
    /// A history read has gone out and has not come back.
    ///
    /// Without this guard, the panel would ask for a `git log` on every frame
    /// for the whole length of the command: it is what asks, and it asks at
    /// render time.
    pub history_pending: bool,
    /// The commit selected in the history, whose diff is shown.
    pub commit: Option<String>,
    /// The opened commit's full message, author and date — the block the diff
    /// paints above its files. `None` until the answer arrives, and cleared on
    /// every commit change: a block left over would caption the next commit's
    /// diff with the previous one's story.
    pub commit_detail: Option<std::rc::Rc<CommitDetail>>,
    /// The review notes taken on this worktree, all files together.
    ///
    /// They live here and not in the diff: a note survives a reload of the file,
    /// a change of range and Claudhub being closed, whereas a `Rendered` does
    /// not survive the next file write.
    ///
    /// Behind an `Rc`: the panel paints every note it shows on every frame, and
    /// it cannot borrow the state while it does — cloning the list to get
    /// around that copied every note's text, its excerpt and its path.
    pub notes: Rc<Vec<crate::ui::notes::Note>>,
    /// The next id. It cannot be derived from `notes`: a deleted note would free
    /// its number, and two notes would carry it.
    pub next_note: u64,
    /// The files marked reviewed, with the volume they had then. Like the notes,
    /// they live in the notes folder and not in the state store: it is the same
    /// review, and it reads in one go.
    pub reviewed: Vec<crate::ui::vault::Reviewed>,
    /// The worktree's free note: `NOTES.md`, as it is on disk.
    ///
    /// A string and not an `Option`: empty means "no file", and the two do not
    /// display differently — the input is there anyway, which is precisely what
    /// makes it available with no gesture.
    pub journal: String,
    /// The commit message being written, parked here while another worktree is
    /// shown: the input is unique, and without this the draft followed the
    /// window from project to project — offered, and committed, on whichever
    /// worktree one switched to.
    pub commit_draft: String,
    /// The worktree's task list, if the vault carries one.
    ///
    /// `None` means "no `TODO.md`", not "no task": the two do not display alike,
    /// and Claudhub does not lay that file down by itself — it is the agent's,
    /// and one does not sow an empty list in the vault of every worktree opened.
    pub todo: Option<crate::ui::vault::Todo>,
    /// The notes folder has answered. Until it has, there is nothing to write:
    /// doing so would erase what has not been read yet.
    pub notes_loaded: bool,
    /// This worktree already has a notes folder.
    ///
    /// Without this flag, opening a worktree would be enough to create its
    /// folder and an empty index: a vault would end up with a tree of empty
    /// folders for worktrees nobody has annotated. It stays true once the last
    /// note is deleted, otherwise the erasure would never go out.
    pub notes_on_disk: bool,
    /// The annotated lines of the displayed diff, one slot per list entry.
    ///
    /// Computed **upstream**, when the diff arrives and on every change to the
    /// notes, never inside `uniform_list`'s closure: that one runs for every
    /// visible line on every frame, wheel animation included.
    pub note_marks: std::rc::Rc<crate::ui::notes::Marks>,
    /// Notes the displayed diff can no longer place.
    ///
    /// They stay in the list, marked as such: a note lost in silence is worse
    /// than no note at all. The set only holds for the open file — it is the
    /// only one whose diff is to hand.
    pub drifted: std::collections::HashSet<u64>,
    /// Where to set the selection when the requested diff arrives.
    ///
    /// An arrow overflowing onto the neighbouring file cannot set it itself: the
    /// diff only arrives after the git command. The gesture is therefore
    /// recorded, and consumed on arrival — only for keyboard navigation, a click
    /// having to open a file without selecting anything in it.
    pub pending_jump: Option<Jump>,
    /// Note whose lines will have to be selected when the diff arrives.
    ///
    /// Same reason as `pending_jump`: clicking a note in the panel opens a file,
    /// and its diff only arrives after the git command.
    pub pending_note: Option<u64>,
    /// The file list as the panels paint it, one entry per range — see
    /// `ui::review::RowCache`. Rebuilding it costs three allocations per file
    /// plus a tree, twice over when both panels are open; nothing in it changes
    /// between two frames.
    pub row_cache: HashMap<DiffRange, crate::ui::review::RowCache>,
    /// Bumped by everything the list is derived from: the status, the file
    /// lists, the reviews, the collapses. A cache built under an older number
    /// is thrown away.
    pub rows_epoch: u64,
}

impl ReviewState {
    /// Something the file list is derived from has changed.
    ///
    /// The caches go rather than being patched: they are rebuilt on the next
    /// frame of the panel that shows them, and only that panel knows its query.
    pub fn rows_changed(&mut self) {
        self.rows_epoch += 1;
        self.row_cache.clear();
    }
}

/// Which end a file opened with the keyboard starts from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Jump {
    /// Going down: we enter by the first change.
    First,
    /// Going up: we enter by the last, where reading stops.
    Last,
}

/// What the diff's commit block says, ready to paint.
///
/// `SharedString`s because the block is rendered on every frame the diff is on
/// screen; behind an `Rc` so the render can clone the handle, not the message.
pub struct CommitDetail {
    /// The commit the detail was read for — the guard against a late answer
    /// captioning another commit's diff.
    pub id: String,
    pub short: gpui::SharedString,
    pub author: gpui::SharedString,
    pub date: gpui::SharedString,
    /// The message's first line.
    pub subject: gpui::SharedString,
    /// The rest of the message, blank separator gone. Empty for the common
    /// one-line message.
    pub body: gpui::SharedString,
}

/// The history as the view shows it.
pub struct History {
    pub commits: Vec<Commit>,
    pub graph: Vec<GraphRow>,
    /// The patch restricted to the lines asked for, one per commit — only in
    /// `LogRange::Lines`, empty everywhere else. See `Evt::History`.
    pub patches: Vec<crate::git::FileDiff>,
    /// What a row shows, ready to hand to gpui — see
    /// `ui::history_view::commit_texts`.
    pub texts: Vec<crate::ui::history_view::CommitText>,
    /// The graph's number of columns, to size its gutter.
    pub width: usize,
}

impl Default for ReviewState {
    fn default() -> Self {
        Self {
            lsp: false,
            range: DiffRange::Working,
            status: Status::default(),
            files: HashMap::new(),
            pending_files: std::collections::HashSet::new(),
            selected: None,
            diff: None,
            unstaged: None,
            diff_selection: None,
            collapsed: std::collections::HashSet::new(),
            base: None,
            history: None,
            // The current branch: what one asks a history first. The whole
            // graph is one click away.
            history_range: LogRange::Head,
            history_pending: false,
            commit: None,
            commit_detail: None,
            notes: Rc::new(Vec::new()),
            next_note: 1,
            reviewed: Vec::new(),
            journal: String::new(),
            commit_draft: String::new(),
            todo: None,
            notes_loaded: false,
            notes_on_disk: false,
            note_marks: std::rc::Rc::new(crate::ui::notes::Marks::default()),
            drifted: std::collections::HashSet::new(),
            pending_jump: None,
            pending_note: None,
            row_cache: HashMap::new(),
            rows_epoch: 0,
        }
    }
}

/// The period of the agent reading. A read of `/proc`, with no process
/// launched: short enough for an agent starting work to be seen.
const AGENT_PERIOD: std::time::Duration = std::time::Duration::from_secs(2);

/// One summary every five readings.
///
/// The summary costs **two git commands per worktree**; at that price, doing it
/// as often as the agent reading would keep a worker busy permanently across a
/// dozen worktrees. A line count ten seconds out of date fools nobody.
const SUMMARY_EVERY: u32 = 5;

/// A weak handle to the application, for the closures that only get an `App`.
///
/// The settings form declares its fields by closures receiving nothing else —
/// which is already why the settings themselves are a global — and a plugin's
/// row has buttons that act on the window. **Weak**, like the dock's panels':
/// strong, it would keep the application alive past its window.
pub struct AppHandle(pub gpui::WeakEntity<ClaudhubApp>);

impl gpui::Global for AppHandle {}

pub struct ClaudhubApp {
    pub(super) git: runtime::Handle,
    pub(super) repos: crate::ui::repos::Repos,

    /// Where the server stands: nothing to say in local mode, a message while it
    /// starts, and what is needed to relaunch it if it goes down.
    pub(super) server_state: super::server::ServerState,
    /// The first-startup question on Windows, while no distribution is chosen.
    pub(super) wsl_prompt: Option<Entity<super::server::WslPrompt>>,
    /// The server runs under WSL (as its handshake says): that is where "this
    /// path is a Windows mount" means something, and the view's machine cannot
    /// know it in its place.
    pub(super) server_wsl: bool,
    /// A newer release is published: its version and its download page. Set
    /// once the check has answered **and** the comparison says the window is
    /// behind; the status bar then shows the link.
    update: Option<(SharedString, SharedString)>,
    /// The selected worktree: the key to almost everything else.
    pub(super) active: Option<PathBuf>,
    /// The read of the file to put back has gone out.
    ///
    /// Every repository that opens asks whether the remembered file is one of
    /// its own, and the request stands until its content comes back: without
    /// this, a window with four repositories would read the same file four
    /// times.
    /// Which worktree the previous session was on, and nothing else.
    ///
    /// Everything else it was doing lives in that worktree's own entry — see
    /// `store::Place` — and is put back by `settle_place` when the selection
    /// lands there.
    pub(super) restoring: crate::ui::store::Session,
    /// A place is being put back: nothing may be filed meanwhile.
    ///
    /// The window shows one screen, one console and one set of tabs, and they
    /// are replaced one after another. A write landing between two of them —
    /// `enter_workspace` files, and it is the first step — would copy the
    /// previous project's console under the checkout one is arriving in.
    pub(super) settling: bool,
    /// The worktree whose place is the one on screen.
    ///
    /// It is what makes `settle_place` idempotent — two call sites reach it for
    /// one arrival — and what keeps `persist_session` from filing the previous
    /// project's tabs under the one just selected.
    pub(super) settled: Option<PathBuf>,
    /// The worktrees whose files have already been reopened this session.
    ///
    /// A file's panel hides itself when the editing root changes and comes back
    /// with it, so a worktree visited twice must not have its tabs read again —
    /// they never left.
    pub(super) replayed: std::collections::HashSet<PathBuf>,
    /// The files a restore is putting back, whole.
    ///
    /// Read by `take_restored_editing`: what arrives during a replay is not a
    /// gesture, and must not carry the window off to the editing screen.
    pub(super) replaying: Vec<crate::ui::store::OpenFile>,
    /// Which of them was on screen, brought forward once they have all landed.
    pub(super) replaying_focus: Option<crate::ui::store::OpenFile>,
    /// The remembered tabs still waiting to be read, in order.
    ///
    /// **One at a time**, and that is the whole reason this is a queue rather
    /// than a handful of commands sent at once: reads are served by three
    /// workers, so the answers come back in whatever order the disk gives them
    /// — and the order they come back in is the order the tabs would sit in.
    pub(super) pending_files: Vec<crate::ui::store::OpenFile>,
    /// A restore is under way: what arrives is not a gesture.
    pub(super) restoring_files: bool,
    /// The directory `claudhub` was launched from, when it is a repository's.
    ///
    /// It is what tells a launch from a mere remembered repository: `opened_at`
    /// names the deepest checkout containing the path asked for, and a
    /// remembered repository asks for its own root, so both come back with a
    /// worktree. Remotely it is the **server's** directory, which arrives with
    /// its handshake — ours names nothing over there.
    launch_dir: Option<PathBuf>,
    /// Whether `launch_dir` was **named** — an argument, a folder handed over
    /// — rather than the shell's ambient working directory. Named outranks
    /// the remembered session; ambient yields to it (`pick_worktree`).
    launch_chosen: bool,
    /// The directory named on the command line, when there was one.
    ///
    /// It outranks the directory `claudhub` was launched *from* — that is what
    /// naming one means — and it is kept rather than consumed because the
    /// remote case cannot use it yet: on Windows the handle is empty until the
    /// server answers, and it is the server's start directory that has to carry
    /// it (`server::connect_wsl`).
    pub(super) launch_arg: Option<PathBuf>,
    /// A folder handed over while the workers were out of reach.
    ///
    /// On Windows the handle is empty until the server answers, and everything
    /// sent then is dropped: a click on "Open with Claudhub" a second after
    /// launching would have opened nothing, silently. Kept as the server's
    /// path — it is translated the moment it arrives — and let go in
    /// `server_hello`.
    pending_handoff: Option<PathBuf>,
    /// How firm the current selection is, so a weaker one cannot displace it.
    ///
    /// The repositories answer in the order the workers happen to finish, and
    /// the remembered worktree must beat "the first checkout of whichever
    /// repository answered first" without ever beating the checkout `claudhub`
    /// was launched from. Ranks rather than flags because there are three of
    /// them, and the comparison is the whole rule.
    pub(super) selection_rank: u8,
    /// Whether the session's first terminal has been opened. See
    /// `ensure_session_terminal`: it happens once, on the worktree one **ends
    /// up** on.
    terminal_started: bool,
    /// How many repositories have been asked for and have not answered.
    ///
    /// It is what tells "this is the checkout one will be looking at" from "this
    /// is the stopgap filling the window while the others are still being
    /// enumerated". Only `Cmd::OpenRepo` is counted: `OpenIfRepo` answers
    /// **nothing** when the launch directory is not a repository, so counting
    /// it would leave a total that never comes back down.
    pub(super) opening_repos: usize,
    pub(super) review: HashMap<PathBuf, ReviewState>,
    /// Every open terminal, in the order they were opened, all worktrees
    /// together. Each is a dock panel per screen sharing one pty — see
    /// `terminal_view::Terminal`.
    pub(super) terminals: Vec<OpenTerminal>,
    pub(super) commit_input: Entity<TextareaState>,
    /// The comparison base's selector. It is searchable: a living repository has
    /// dozens of branches, and scrolling a list of seventy entries to find one
    /// whose name you already know is exactly what a search field saves.
    pub(super) base_select: Entity<SelectState<SearchableVec<BaseChoice>>>,
    /// The branch picker's surface, kept from one opening to the next: it holds
    /// the filter field, whose `InputState` may not be rebuilt at every frame.
    pub(super) branch_picker: Entity<crate::ui::branch_picker::BranchPicker>,
    /// The same surface as a column of the history — see
    /// `ClaudhubApp::render_history_branches`.
    ///
    /// A **second instance** and not the same one: the popover empties its
    /// filter every time it opens, which is right for a surface one calls up
    /// and wrong for a column one leaves open, and the two would otherwise
    /// fight over the step being shown.
    pub(super) branches_dock: Entity<crate::ui::branch_picker::BranchPicker>,
    /// The worktree picker's surface, kept for its filter field's sake.
    pub(super) worktree_picker: Entity<crate::ui::worktree_picker::WorktreePicker>,
    /// A note's input field. Created **once**: recreated in a `render` or when
    /// the dialog opens, it would lose the cursor, the selection and the text on
    /// the first keystroke.
    pub(super) note_input: Entity<TextareaState>,
    /// The prompt going out to the agent, before it goes.
    pub(super) prompt_input: Entity<TextareaState>,
    /// The input for a task to add to `TODO.md`, at the bottom of the list.
    pub(super) task_input: Entity<InputState>,
    /// The input for a task being edited, in its place in the list.
    ///
    /// A second field and not the bottom one: what is being typed for a new task
    /// must not disappear because a typo two lines above is being fixed.
    pub(super) task_edit_input: Entity<InputState>,
    /// The line of the task being edited, if there is one.
    pub(super) task_editing: Option<usize>,
    /// The input for the worktree's free note.
    ///
    /// A single one for every worktree, whose content follows the displayed one:
    /// one per open worktree would keep that many editing states alive, and there
    /// is only ever one in front of you.
    pub(super) journal_input: Entity<TextareaState>,
    /// A write of the free note is already scheduled.
    pub(super) journal_save: bool,
    /// The note being written: its anchoring, settled at the moment of the gesture.
    ///
    /// It is settled there and not on confirmation because the diff can change
    /// while it is being written — an agent works while it is being reviewed —
    /// and the note has to be about what was in front of you.
    pub(super) note_draft: Option<crate::ui::notes_view::NoteDraft>,
    /// The collapsed sections of the "Notes" panel, by key.
    ///
    /// In memory and not in the store: it is a reading posture, which changes
    /// several times during a review, not a preference one expects back the next
    /// day.
    pub(super) notes_collapsed: std::collections::HashSet<&'static str>,
    /// Does the notes panel show only the unhandled ones.
    pub(super) notes_only_open: bool,
    /// Each open repository's `wt.toml`, read once. `None`: it has none, and the
    /// project's gestures simply disappear from the menu.
    pub(super) wt_projects: HashMap<PathBuf, Option<crate::wt::Snapshot>>,
    /// Each visited worktree's `justfile`, read once and re-read when the file
    /// changes. `None`: there is none — or `just` is not installed — and the run
    /// button simply is not painted.
    ///
    /// Keyed by worktree and not by repository, unlike `wt.toml`: a justfile is
    /// a file of the checkout, and a branch is free to add a recipe.
    ///
    /// Behind an `Rc`: the run button's menu needs the recipes in a `'static`
    /// closure, and copying the list on every frame of the bar is what that
    /// used to cost.
    pub(super) just_recipes: HashMap<PathBuf, Option<std::rc::Rc<crate::just::Snapshot>>>,
    /// Each visited worktree's Pest suite. Keyed by worktree like the
    /// justfile, and for its reason: the tests are the checkout's files.
    pub(super) pest: HashMap<PathBuf, crate::ui::tests_view::PestState>,
    pub(super) pest_scroll: gpui::UniformListScrollHandle,
    /// The run each worktree follows — or followed: the panel keeps the last
    /// account until the next launch replaces it.
    pub(super) pest_runs: HashMap<PathBuf, crate::ui::tests_view::RunState>,
    pub(super) pest_run_scroll: gpui::UniformListScrollHandle,
    /// The send id `Evt::TestsRan` hands back — a run launched for a state
    /// since replaced must not paint the panel.
    pub(super) pest_run_seq: u64,
    /// What `wt` knows about each worktree: started or not, its static
    /// address, and the preview — options, ports, `[status.info]`.
    pub(super) wt_states: HashMap<PathBuf, crate::runtime::protocol::WtWorktree>,
    /// What `[open] source` enumerated for a worktree, asked when its "open"
    /// menu opens. `None`: asked, not answered yet — the menu shows an
    /// ellipsis. Absent: never asked.
    pub(super) wt_links: HashMap<PathBuf, Option<Vec<crate::wt::Endpoint>>>,
    /// The guided creation under way.
    pub(super) creation: Option<crate::ui::worktree_ops::WtPrompt>,
    /// The worktree whose integration has gone out, and its branch: it is on the
    /// success arriving that the cleanup is offered.
    pub(super) integrated: Option<(PathBuf, String)>,
    /// Each worktree's files and their tree.
    pub(super) explorers: HashMap<PathBuf, crate::ui::explorer::Explorer>,
    /// The file open in the built-in editor, **one per worktree**.
    ///
    /// Like the terminals, and for the same reason: a worktree is where one
    /// works, and coming back to it should give back what was open there. A
    /// single editor made switching worktrees leave the previous project's file
    /// on screen — a file that belongs to a tree the rest of the window is no
    /// longer showing.
    pub(super) editings: HashMap<PathBuf, crate::ui::explorer::Editors>,
    /// Where the editor has been, one trail per worktree. See `ui::jumps`.
    pub(super) jumps: HashMap<PathBuf, crate::ui::jumps::Jumps>,
    /// Where the caret must land in the file that is being opened.
    ///
    /// A jump to another file cannot place its caret itself: the text only
    /// arrives one round trip later. The gesture is noted here and spent when
    /// it does — the same shape as the review's `pending_jump`, and for the
    /// same reason. It carries its worktree because one browses while a file is
    /// being read, and landing a caret in somebody else's editor is worse than
    /// not landing it at all.
    pub(super) landing: Option<crate::ui::explorer::Pending>,
    /// Ticks once per file tab read, and stamps the tab that is read: what the
    /// tab limit closes on. A counter of our own and not a clock — the order is
    /// all that is asked of it, and a clock is one thing the wire does not carry.
    pub(super) tab_clock: u64,
    /// The language servers, one session per worktree. See `ui::lsp`.
    pub(super) lsp: HashMap<PathBuf, crate::ui::lsp::Session>,
    /// The requests in flight, by the id we gave them: what a provider's `Task`
    /// is waiting on. A one-shot channel each, and nothing is ever left in
    /// here — the core times a request out, and a session's death fails what it
    /// was carrying.
    pub(super) lsp_pending: HashMap<u64, async_channel::Sender<Result<String, String>>>,
    /// The last request of a method for a worktree, so the next one can cancel
    /// it: a completion is asked on one keystroke and stale on the next.
    pub(super) lsp_asking: HashMap<(PathBuf, String), u64>,
    /// Never goes back, like the SQL console's send id: the answer to a gesture
    /// that has been replaced must be recognisable as such.
    pub(super) lsp_next_id: u64,
    /// The editor has just followed a definition on its own.
    ///
    /// gpui-component handles a `Ctrl`+click on a symbol it has an answer for,
    /// and tells nobody: our own listener sits on an ancestor and runs in the
    /// same dispatch, right after. Without this it would ask again — at the
    /// caret of the file just landed in, which is somebody else's word. It is
    /// cleared in the **capture** phase of that same click, so a jump followed
    /// from the context menu cannot leave it standing for the next one.
    pub(super) followed_definition: bool,
    /// The modifier that makes a line's symbols clickable is held down.
    ///
    /// Kept rather than read at each render, and that is what makes it cheap:
    /// the window is asked for a new frame only when it **flips**, not on every
    /// `Shift` of every capital letter typed somewhere else. See
    /// `ui::follow::followable`.
    pub(super) follow_armed: bool,
    /// The `Ctrl`-hovered word, and which line's text it is in.
    ///
    /// The underline is painted from here, one frame behind the pointer as a
    /// hover always is. It is cleared by the entry the pointer moves onto and
    /// by letting go of the modifier — a word left underlined under nothing
    /// would read as a word that has been chosen.
    pub(super) follow_hover: Option<(crate::ui::follow::Spot, std::ops::Range<usize>)>,
    pub(super) files_scroll: gpui::UniformListScrollHandle,
    /// The explorer tree's focus, which gives it its arrows.
    ///
    /// A handle of its own and not the root view's: it is what tells "the arrows
    /// browse the tree" from "the arrows browse the diff", and the bindings'
    /// predicate is read on the focused node's context.
    pub(super) explorer_focus: FocusHandle,
    /// The project-wide search of the worktree on show: what was asked, what
    /// came back, and the file shown beside it. See `ui::search_view`.
    pub(super) search: crate::ui::search_view::SearchState,
    /// The other worktrees' searches, put back on returning to one.
    ///
    /// A search belongs to the project it was made in — its hits name that
    /// checkout's files — so changing worktree stashes it here and takes back
    /// what was left, the field included. See `ClaudhubApp::sync_search`.
    pub(super) searches: HashMap<PathBuf, crate::ui::search_view::SearchState>,
    /// The id of the last search sent, whatever worktree asked for it. Never
    /// goes back: see `search_view::SearchState::request`.
    pub(super) search_next_id: u64,
    /// The search field. Created **once**, like every other input: rebuilt at
    /// render time it would lose the cursor and the text on the first
    /// keystroke.
    pub(super) search_input: Entity<InputState>,
    /// The pathspecs to look in, a field of its own: narrowing a search to
    /// `*.php` must not make one retype the word being looked for.
    pub(super) search_glob_input: Entity<InputState>,
    pub(super) search_regex: bool,
    pub(super) search_whole_word: bool,
    pub(super) search_scroll: gpui::UniformListScrollHandle,
    pub(super) search_preview_scroll: gpui::UniformListScrollHandle,
    /// The result list's focus, which gives it its arrows.
    ///
    /// A handle of its own, like the project explorer's and the schema tree's:
    /// it is what tells "the arrows walk the results" from "the arrows walk the
    /// diff", and two sets of bindings on one key would not be settled.
    pub(super) search_focus: FocusHandle,
    /// The preview read that has gone out, and the line to reveal when it comes
    /// back.
    ///
    /// One walks the results while a file is being read, and painting the
    /// answer of a row one has already left is worse than painting nothing —
    /// the same shape as the editor's `landing`, and for the same reason.
    pub(super) pending_preview: Option<(PathBuf, PathBuf, u32)>,
    /// The plugins, in manifest order. Each holds its script, its state and the
    /// tree it last produced; the panels only paint what is in here.
    pub(super) plugins: Vec<crate::plugin::Plugin>,
    /// The capability calls still out, and the moment each stops being waited
    /// for. **A request never stays pending**: the background sweep is what
    /// fails the ones nobody answered, rather than a timer per request.
    pub(super) plugin_deadlines: Vec<(String, u64, std::time::Instant)>,
    /// The folded sections of the plugins' panels. In memory, like the notes
    /// panel's: a reading posture, not a preference.
    pub(super) plugin_folded: crate::ui::plugin_view::Folds,
    /// The scroll handles of the plugins' lists, one per panel and list id.
    ///
    /// The window's and not the tree's, for the reason a field's `InputState`
    /// is: a tree is rebuilt on every gesture, and a handle created there would
    /// put the list back at the top each time.
    pub(super) plugin_lists: std::collections::HashMap<String, gpui::UniformListScrollHandle>,
    /// The colouring of the excerpts a plugin draws, kept by content.
    ///
    /// An excerpt is a fragment with no file around it — a stack frame's ten
    /// lines — so it is parsed on its own, which is what `HitHighlights`
    /// already does for a search's result list. Parsing costs milliseconds, and
    /// a panel repaints on every frame, hence the cache; it is keyed by the
    /// text itself, so a tree rebuilt identically finds it again.
    pub(super) plugin_code: crate::ui::plugin_view::CodeStyles,
    /// The lane a plugin's capabilities leave by. Kept so the list of plugins
    /// can be rebuilt — after an installation — without spawning a second
    /// drain task for every one of them.
    pub(super) plugin_outbox: Option<async_channel::Sender<crate::plugin::host::Request>>,
    /// What the editing screen shows when it is not a worktree's file.
    ///
    /// The editor keys what it holds by a **root**, which is a worktree in the
    /// ordinary case and a plugin's directory when one edits a script: a folder
    /// outside any repository is simply another root, and that is what makes
    /// editing one cost no new plumbing. Cleared by choosing a worktree, whose
    /// file is then what one expects to find.
    pub(super) editing_root: Option<PathBuf>,
    /// The databases tree: connections, schemas, tables, columns.
    pub(super) db: crate::ui::db::DbState,
    pub(super) db_scroll: gpui::UniformListScrollHandle,
    /// The databases tree's focus, which gives it its arrows.
    ///
    /// A handle of its own, like the project explorer's: it is what tells "the
    /// arrows browse the schema" from "the arrows browse the diff".
    pub(super) db_focus: FocusHandle,
    /// Every open SQL console, in the order they were opened, all worktrees
    /// together — the terminals' list, for the same reason: a panel per console,
    /// and the ones of the worktree one is not looking at stay in the dock,
    /// invisible.
    pub(super) consoles: Vec<crate::ui::db_query::Console>,
    /// The console a keystroke is about: the last one to have held the focus.
    ///
    /// The gestures that come from a console's own bar carry their id; the ones
    /// that come from the keyboard — `Ctrl+Enter`, `Ctrl+C` on the grid — have
    /// only the focus to go on, and the focus is what the dock moves when one
    /// changes tab.
    pub(super) active_console: Option<crate::ui::db_query::ConsoleId>,
    /// What names the next console. Never reused, unlike the rank the tab says.
    pub(super) console_seq: u64,
    /// What identifies a query's answer, counted for the **window** and not for
    /// a console: two consoles counting from one would each take the other's
    /// rows, and the number is the only thing the answer carries back.
    pub(super) db_request_seq: u64,
    /// What is known of each repository's tags, by main repository: tags live
    /// in the shared `.git` and are the same seen from every worktree.
    pub(super) tags: HashMap<PathBuf, crate::ui::tags::TagsState>,
    pub(super) tags_scroll: gpui::UniformListScrollHandle,
    /// What is known of each repository's stashes, by main repository:
    /// `refs/stash` lives in the shared `.git` and is the same seen from every
    /// worktree.
    pub(super) stashes: HashMap<PathBuf, crate::ui::stashes::StashesState>,
    pub(super) stashes_scroll: gpui::UniformListScrollHandle,
    /// Every query run, per worktree, and the reach the panel is showing.
    ///
    /// In the application and not in a global: only the history panel reads it,
    /// and a global is what the settings form needs, not what a panel needs.
    pub(super) sql_history: crate::ui::sql_history::History,
    pub(super) sql_history_reach: crate::ui::sql_history::Reach,
    /// The history list's scroll handle. `VirtualListScrollHandle` because its
    /// rows are not all the same height — a day's heading is one line, a query
    /// two — and that is the one case where walking a vector of sizes is worth
    /// its price.
    pub(super) sql_history_scroll: gpui_component::VirtualListScrollHandle,
    /// The history list as the panel last prepared it — the entries the filter
    /// kept, their rows and their heights. Rebuilt when what it depends on
    /// moves, `History::version` included; see `refresh_sql_history_list`.
    pub(super) sql_history_list: Option<crate::ui::sql_history_view::HistoryList>,
    /// A worktree being waited for, and the prompt to deliver in it once created.
    ///
    /// `wt`'s creation runs hooks that take minutes; nothing but the arrival of
    /// the worktree list says it has finished.
    pub(super) awaiting_agent: Option<(PathBuf, String)>,
    /// What is waiting for a frame to become a balloon.
    ///
    /// **A queue and not a call**: `push_notification` wants a `&mut Window`,
    /// and most of the places with something to say hold none — a copy, an
    /// editor's refusal, a note that could not be replaced, an answer that came
    /// back in a spawned task. They are drained at the top of the root's
    /// render, the first place a window exists again after `cx.notify()`.
    pub(super) pending_notes: Vec<(SharedString, crate::ui::notify::Level)>,
    /// A view the home screen has to bring forward, waiting for a frame.
    ///
    /// The same lane the notifications take, and for the same reason: selecting
    /// a tab is `DockArea::move_panel`, which wants a `&mut Window`, and the
    /// gestures that ask for one have none — opening a diff is `open_file`,
    /// which takes a `cx` and nothing else, having had no use for a window in
    /// its life. Filed here, drained at the top of the root's render.
    ///
    /// One and not a queue: two of them in the same frame would be two tabs
    /// asked for at once, and only the last can be on screen.
    pub(super) pending_reveal: Option<&'static str>,
    /// Worktrees whose status read has already gone out.
    ///
    /// The file watcher can produce several waves before an answer comes back;
    /// without this guard rail, a build touching a thousand files piles up a
    /// thousand identical `git status`es, and everything after — diffs included
    /// — waits behind them.
    pending_status: std::collections::HashSet<PathBuf>,
    /// When the last automatic fetch was started.
    last_auto_fetch: Option<std::time::Instant>,
    /// The worktree a commit message is being waited for, if there is one.
    ///
    /// One only, and its path rather than a boolean: the agent takes several
    /// seconds, one changes worktree in the meantime, and it is what lets the
    /// answer find the field it belongs to again — as it lets the button know
    /// which of the two panels is spinning.
    pub(super) suggesting_message: Option<PathBuf>,
    /// The window's dock: three tool zones around a centre of documents.
    ///
    /// One and not nine. A screen used to be an area of its own, which is why a
    /// terminal needed one panel per screen and a file two; there is one place
    /// for each of them now. See `ui::dock_layout`.
    pub(super) dock: Entity<DockArea>,
    /// The dock's skin, kept alive: it is what draws the tabs, and it is through
    /// it that the presentation settings pass.
    #[allow(dead_code)]
    dock_skin: std::rc::Rc<DockSkin>,
    /// True when a deferred write of the layout is already scheduled.
    layout_save_scheduled: bool,
    /// The settings form, built once and outliving its dialog.
    ///
    /// **An entity and not a closure**: `open_dialog` keeps a `Fn` called back
    /// from the root's render, where reading the root entity is a panic. See
    /// `settings_view::SettingsForm`.
    pub(super) settings_form: Entity<crate::ui::settings_view::SettingsForm>,
    /// The edges the zen fold put away, to be given back on the way out.
    ///
    /// Empty when one is not in zen. It is not persisted: a window reopening in
    /// zen with no memory of what it hid would be a window one has to rebuild.
    pub(super) zen_folded: Vec<crate::ui::rails::Side>,
    /// The terminals show every worktree's, not only the one being looked at.
    ///
    /// What the multiplexer used to be a screen for. It is not a preference but
    /// where one stands — "which of the agents I left running has finished" —
    /// so it lives in the store beside the rest of it. The panels are not
    /// rebuilt when it turns over: their visibility is observed, and the tab
    /// says which project it belongs to for as long as it holds.
    pub(super) terminals_everywhere: bool,
    /// The views the user has hidden, by panel name.
    ///
    /// A set and not a flag per panel: it is `Panel::visible` that makes a view
    /// disappear — a collapsible dock zone would forbid moving the last panel
    /// left in it — and the mechanism is the same for the terminals as for the
    /// review.
    ///
    /// Here and not in the settings alone: the panels observe it, and
    /// `Settings::update_global` notifies nobody. The settings keep the copy that
    /// survives closing.
    /// The views taken off their rail: no button, no tab. A decision made once
    /// — I do not use Sentry — and the "Views" menu is where it is undone.
    pub(super) off_panels: std::collections::HashSet<String>,
    /// The views folded away: not on screen, but one press of their button from
    /// coming back. What a press on a lit button does.
    pub(super) folded_panels: std::collections::HashSet<String>,

    /// What each worktree has in progress, including those not opened: it is the
    /// question one asks while scanning the list.
    pub(super) summaries: HashMap<PathBuf, crate::git::Summary>,
    /// The agents found, by worktree, and whether they are working. The tracker
    /// holds the previous reading: "working" is a difference between two of
    /// them, not something a single one says.
    pub(super) agents: crate::agent::Tracker,
    /// True between the press and the release of the button in the diff view:
    /// it is what tells a selection drag from a plain hover, and what prevents a
    /// drag started elsewhere — in the sidebar, on a resize handle — from
    /// extending the selection while passing over the code.
    pub(super) diff_dragging: bool,

    /// The diff's virtualised list scrolling. It lives on the view and is never
    /// rebuilt: recreating it per frame would put the diff back at the top on
    /// every frame.
    pub(super) diff_scroll: gpui::UniformListScrollHandle,
    /// The conflicted file open in the three-pane merge, if there is one. It
    /// stands **in place of** the diff at the centre of the Git screen: opening
    /// any other file is what puts the diff back.
    pub(super) merging: Option<crate::ui::merge_view::MergeState>,
    /// The merge view's list scrolling. Its own handle, never rebuilt, for the
    /// reason every other one is: a fresh handle per frame scrolls back to the
    /// top on every frame.
    pub(super) merge_scroll: gpui_component::VirtualListScrollHandle,
    /// The measured width of the diff view on the previous frame.
    ///
    /// A view just opened has no bounds: they are only worth something once the
    /// first layout has been done. Taking them for the real width wraps the file
    /// at eight columns — and since nothing redraws while nothing happens, the
    /// display stays wrong until the next event, that is, the background sweep
    /// two seconds later. So we keep the last known width, which holds for every
    /// following diff.
    pub(super) diff_width: gpui::Pixels,
    /// Frames asked for while waiting for that first measurement.
    ///
    /// Bounded: a panel shrunk to zero would never be measured, and asking for a
    /// frame on every frame would run the interface flat out for a view nobody
    /// sees.
    pub(super) diff_measures: u8,
    /// The width the diff's own frame had at the last **layout**, which is not
    /// the same reading as `diff_width`.
    ///
    /// `diff_width` is read at render time and therefore describes the frame
    /// **before**: a panel that has just been zoomed paints one frame at the
    /// size it no longer has, and nothing asks for another — the window only
    /// redraws on an event, so the wrong width stayed on screen until the
    /// background sweep two seconds later. A canvas measures the frame after
    /// layout, and a width that has moved since the last one is what asks for
    /// the frame that repaints it right.
    pub(super) diff_laid_out: gpui::Pixels,
    /// The history list's measured width, by the same mechanism and for a
    /// reason of its own: the panel lives in a resizable column, and the row's
    /// fixed columns — hash, author, date — are dropped one by one when the
    /// width no longer holds them beside the summary, which is what a history
    /// is read by.
    pub(super) history_laid_out: gpui::Pixels,
    /// The history panel's own shape, measured the same way and read for one
    /// question only: whether the graph sits beside the chosen commit's files
    /// or above them — see `history_view::side_by_side`. Not the width above,
    /// which is the room left *beside the graph's gutter*.
    pub(super) history_shape: gpui::Size<gpui::Pixels>,
    /// The handle of the wrapped two-column view.
    ///
    /// A second handle and not the same one: the entries there no longer have
    /// the same height, `v_virtual_list` paints them, and it can only scroll with
    /// its own. The two are never shown at the same time.
    pub(super) diff_wrap_scroll: gpui_component::VirtualListScrollHandle,
    pub(super) history_scroll: gpui::UniformListScrollHandle,
    /// In a line history, whether a commit shows the patch restricted to those
    /// lines or the whole commit.
    ///
    /// A reading preference and not a worktree's state: it is the same answer
    /// one wants from one selection to the next.
    pub(super) history_lines_only: bool,
    /// The file lists' scrolling, **one per range**: "Review" and "Changes" are
    /// shown at the same time, and a single handle would scroll them together.
    file_scroll: HashMap<DiffRange, gpui::UniformListScrollHandle>,
    /// Each panel's search, created on its first opening — **one per worktree
    /// and per panel**, as the editors and the trail are. See `find::Key`.
    pub(super) finders: HashMap<crate::ui::find::Key, crate::ui::find::Finder>,
    /// The panel the last click happened in: that is what `Ctrl+F` aims at.
    ///
    /// The click and not the focus. gpui-component's dock puts focus on the
    /// active tab of each zone — three of them are shown at once — and nothing
    /// says which one the user is looking at; the last button pressed does.
    pub(super) pane: crate::ui::find::Pane,
    /// The hits found in the displayed diff.
    pub(super) diff_search: crate::ui::find::DiffSearch,
    /// Scroll handles for the panels that are **not** virtualised — the notes,
    /// the conflicts, Sentry, the sidebar. A map rather than one field per
    /// panel: they are all the same thing, and they only serve to give the
    /// scrollbar a position. Created here and not at render time, otherwise the
    /// list would go back to the top on every frame.
    scrolls: HashMap<&'static str, gpui::ScrollHandle>,
    /// The wheel smoothing, per panel, created on its first use. The key is the
    /// scrollbar's — see `ui::scroll`.
    pub(super) motions: HashMap<gpui::SharedString, crate::ui::motion::ScrollMotion>,
    /// The split between the graph and the chosen commit's file list, stacked.
    pub(super) history_split: Entity<gpui_component::resizable::ResizableState>,
    /// The split between the branches and everything else, in a zone wide
    /// enough for three columns.
    ///
    /// **Nested and not a third slot** of the split below: dragging the
    /// branches wider would otherwise move the graph's divider with it, and
    /// each state stays a pair one can reason about.
    pub(super) history_split_branches: Entity<gpui_component::resizable::ResizableState>,
    /// And the same split laid across, in a zone wide enough for it.
    ///
    /// **A second state and not the same one**: what a resizable keeps is the
    /// sizes of its slots, and a height carried into a width would open the
    /// side-by-side arrangement on whatever the stacked one was last dragged
    /// to.
    pub(super) history_split_side: Entity<gpui_component::resizable::ResizableState>,
    /// The write operations in flight — what the buttons carrying a spinner
    /// read, and what the status bar names.
    ///
    /// **The key is exactly what comes back.** Every write answers with an
    /// `Evt::Done` or an `Evt::Failed` carrying the same worktree and the same
    /// action, and both go through one place: an entry can therefore not be left
    /// behind, which is the only failure mode of a spinner.
    pub(super) flight: crate::ui::inflight::InFlight,
    /// The settings page the screen must land on, and the identity of the form
    /// that shows it. See `settings_view::open_settings_at`.
    pub(super) settings_page: crate::ui::settings_view::Page,
    pub(super) settings_epoch: usize,
    /// What the system told the settings form — installed fonts, `/etc/shells`.
    /// Read once: the form's declaration is rebuilt on every frame.
    pub(super) settings_env: Option<std::rc::Rc<crate::ui::settings_view::Environment>>,
    /// The lowest level the logs page shows. A posture of reading, which changes
    /// several times while chasing something down, not a preference one expects
    /// to find again tomorrow — hence here and not in the settings file.
    pub(super) logs_level: log::LevelFilter,
    /// The log records, laid out as the page paints them, copied from the ring
    /// only when it has moved.
    pub(super) logs: std::rc::Rc<Vec<crate::ui::settings_view::LogRow>>,
    pub(super) logs_seen: u64,
    /// The root's focus, and what a gesture hands the keyboard back to.
    ///
    /// A focus handle nobody renders any more resolves no binding: whatever
    /// removes a focused element from the tree — hiding the terminals, closing
    /// one — has to say where the focus goes, or every shortcut stays dead
    /// until a click lands on a live node.
    pub(super) focus: FocusHandle,
}

/// The window's dock, and what its building found out.
struct Docks {
    dock: Entity<DockArea>,
    dock_skin: std::rc::Rc<DockSkin>,
    /// Whether a layout had to be built from scratch, and therefore has to be
    /// written at once: without that, the file keeps an earlier version's until
    /// the first move.
    needs_save: bool,
}

impl ClaudhubApp {
    /// The startup workers: this process's, or none.
    ///
    /// None when they will live elsewhere — on Windows, or when
    /// `CLAUDHUB_SERVER_CMD` names a server. The connection happens afterwards,
    /// in a task: waking a distribution takes seconds, and a gpui entity's
    /// constructor cannot pay for them.
    fn spawn_backend() -> (runtime::Handle, async_channel::Receiver<Evt>, bool) {
        if runtime::remote::command_from_env().is_some() || cfg!(windows) {
            // The handle stays empty until the server answers. Falling back to
            // local workers would make `git.exe` work on Windows paths, silently
            // and wide of the mark.
            let (_closed, events) = async_channel::unbounded();
            return (runtime::Handle::pending(), events, true);
        }
        let (git, events) = runtime::spawn();
        (git, events, false)
    }

    pub fn new(
        folder: Option<PathBuf>,
        handoffs: async_channel::Receiver<Option<PathBuf>>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let (git, events, remote) = Self::spawn_backend();

        let commit_input =
            cx.new(|cx| TextareaState::new(window, cx).placeholder(tr!("commit-placeholder")));

        // `auto_grow` rather than a fixed height: a review remark is two lines
        // or ten, and a frozen area forces scrolling what is being written.
        let note_input = cx.new(|cx| {
            TextareaState::new(window, cx)
                .auto_grow(2, 8)
                .placeholder(tr!("note-placeholder"))
        });

        // Taller than a note's: what is read back here is a whole message, with
        // the quoted code, and eight lines of context are the minimum to judge
        // what is going out.
        let prompt_input = cx.new(|cx| TextareaState::new(window, cx).auto_grow(8, 20));

        let task_input =
            cx.new(|cx| InputState::new(window, cx).placeholder(tr!("todo-add-placeholder")));
        let task_edit_input = cx.new(|cx| InputState::new(window, cx));

        // The free note: it grows with what is written in it, within the limits
        // the section can give without pushing the rest out of sight.
        let journal_input = cx.new(|cx| {
            TextareaState::new(window, cx)
                .auto_grow(3, 14)
                .placeholder(tr!("journal-placeholder"))
        });

        let base_select = cx.new(|cx| {
            SelectState::new(
                SearchableVec::new(Vec::<BaseChoice>::new()),
                None,
                window,
                cx,
            )
            .searchable(true)
        });
        // Subscribed once, in the constructor: a subscription set up during a
        // render would pile up on every frame.
        cx.subscribe(&base_select, |this, _, event, cx| {
            let SelectEvent::Confirm(Some(base)) = event else {
                return;
            };
            this.set_base(base.to_string(), cx);
        })
        .detach();

        // The dock and its panels. The panels carry no state: they delegate to
        // this entity, of which they only keep a weak reference so as not to
        // form a cycle.
        crate::ui::panels::register(&cx.entity(), cx);
        let Docks {
            dock,
            dock_skin,
            needs_save: app_needs_layout_save,
        } = Self::build_dock(window, cx);

        let branch_picker = crate::ui::branch_picker::BranchPicker::new(
            crate::ui::branch_picker::Mode::Popover,
            window,
            cx,
        );
        let branches_dock = crate::ui::branch_picker::BranchPicker::new(
            crate::ui::branch_picker::Mode::Docked,
            window,
            cx,
        );
        let worktree_picker = crate::ui::worktree_picker::WorktreePicker::new(window, cx);

        let search_inputs = crate::ui::search_view::SearchInputs::new(window, cx);
        let settings_form = {
            let app = cx.entity();
            cx.new(|cx| crate::ui::settings_view::SettingsForm::new(&app, cx))
        };

        let mut app = Self {
            git,
            repos: crate::ui::repos::Repos::default(),
            server_state: super::server::ServerState::default(),
            wsl_prompt: None,
            server_wsl: false,
            update: None,
            active: None,
            restoring: crate::ui::store::Store::global(cx).session.clone(),
            settled: None,
            settling: false,
            replayed: std::collections::HashSet::new(),
            replaying: Vec::new(),
            replaying_focus: None,
            pending_files: Vec::new(),
            restoring_files: false,
            launch_dir: None,
            launch_chosen: false,
            launch_arg: folder,
            pending_handoff: None,
            selection_rank: crate::ui::session::SELECTION_NONE,
            terminal_started: false,
            opening_repos: 0,
            review: HashMap::new(),
            terminals: Vec::new(),
            commit_input,
            base_select,
            branch_picker,
            branches_dock,
            worktree_picker,
            note_input,
            prompt_input,
            task_input,
            task_edit_input,
            task_editing: None,
            journal_input,
            journal_save: false,
            notes_collapsed: std::collections::HashSet::new(),
            note_draft: None,
            notes_only_open: false,
            wt_projects: HashMap::new(),
            just_recipes: HashMap::new(),
            lsp: HashMap::new(),
            lsp_pending: HashMap::new(),
            lsp_asking: HashMap::new(),
            lsp_next_id: 1,
            wt_states: HashMap::new(),
            wt_links: HashMap::new(),
            creation: None,
            integrated: None,
            explorers: HashMap::new(),
            editings: HashMap::new(),
            jumps: HashMap::new(),
            landing: None,
            tab_clock: 0,
            followed_definition: false,
            follow_armed: false,
            follow_hover: None,
            files_scroll: gpui::UniformListScrollHandle::new(),
            search: Default::default(),
            searches: HashMap::new(),
            search_next_id: 0,
            search_input: search_inputs.text,
            search_glob_input: search_inputs.glob,
            search_regex: false,
            search_whole_word: false,
            search_scroll: gpui::UniformListScrollHandle::new(),
            search_preview_scroll: gpui::UniformListScrollHandle::new(),
            search_focus: search_inputs.focus,
            pending_preview: None,
            plugins: Vec::new(),
            plugin_deadlines: Vec::new(),
            plugin_folded: Default::default(),
            plugin_lists: Default::default(),
            plugin_code: Default::default(),
            plugin_outbox: None,
            editing_root: None,
            db: Default::default(),
            db_scroll: gpui::UniformListScrollHandle::new(),
            db_focus: cx.focus_handle(),
            consoles: Vec::new(),
            active_console: None,
            console_seq: 0,
            db_request_seq: 0,
            // Read once, at startup, like the state store: it is a file of our
            // own, a few hundred kilobytes at most, and nothing else writes it.
            tags: HashMap::new(),
            tags_scroll: gpui::UniformListScrollHandle::new(),
            pest: HashMap::new(),
            pest_scroll: gpui::UniformListScrollHandle::new(),
            pest_runs: HashMap::new(),
            pest_run_scroll: gpui::UniformListScrollHandle::new(),
            pest_run_seq: 0,
            stashes: HashMap::new(),
            stashes_scroll: gpui::UniformListScrollHandle::new(),
            sql_history: crate::ui::sql_history::History::load(),
            sql_history_reach: Default::default(),
            sql_history_scroll: gpui_component::VirtualListScrollHandle::new(),
            sql_history_list: None,
            awaiting_agent: None,
            pending_notes: Vec::new(),
            pending_reveal: None,
            pending_status: std::collections::HashSet::new(),
            last_auto_fetch: None,
            suggesting_message: None,
            dock,
            dock_skin,
            layout_save_scheduled: false,
            terminals_everywhere: false,
            zen_folded: Vec::new(),
            settings_form,
            off_panels: Settings::global(cx).hidden_panels.iter().cloned().collect(),
            folded_panels: Settings::global(cx).folded_panels.iter().cloned().collect(),
            summaries: HashMap::new(),
            agents: crate::agent::Tracker::default(),
            diff_dragging: false,
            diff_scroll: gpui::UniformListScrollHandle::new(),
            diff_width: gpui::px(0.),
            diff_measures: 0,
            merging: None,
            merge_scroll: gpui_component::VirtualListScrollHandle::new(),
            diff_laid_out: gpui::px(0.),
            history_laid_out: gpui::px(0.),
            history_shape: gpui::size(gpui::px(0.), gpui::px(0.)),
            diff_wrap_scroll: gpui_component::VirtualListScrollHandle::new(),
            history_scroll: gpui::UniformListScrollHandle::new(),
            history_lines_only: true,
            file_scroll: HashMap::new(),
            scrolls: HashMap::new(),
            motions: HashMap::new(),
            explorer_focus: cx.focus_handle(),
            finders: HashMap::new(),
            // The diff by default: it is the panel one looks at on arriving, and
            // the only one that is always there.
            pane: crate::ui::find::Pane::Diff,
            diff_search: crate::ui::find::DiffSearch::default(),
            history_split: cx.new(|_| gpui_component::resizable::ResizableState::default()),
            history_split_side: cx.new(|_| gpui_component::resizable::ResizableState::default()),
            history_split_branches: cx
                .new(|_| gpui_component::resizable::ResizableState::default()),
            flight: crate::ui::inflight::InFlight::default(),
            settings_page: Default::default(),
            settings_epoch: 0,
            settings_env: None,
            // Everything the console kept: the ring only holds what
            // `CLAUDHUB_LOG` let through in the first place, and opening on a
            // narrowed view would hide records nothing else says exist.
            logs_level: log::LevelFilter::Trace,
            logs: Default::default(),
            logs_seen: 0,
            focus: cx.focus_handle(),
        };

        if app_needs_layout_save {
            app.schedule_layout_save(cx);
        }
        cx.set_global(AppHandle(cx.entity().downgrade()));
        app.pump_events(events, window, cx);
        app.pump_handoffs(handoffs, window, cx);
        // Before the sweep: the sweep is also what expires a plugin's requests,
        // and it has to find a list to look at rather than build one.
        app.start_plugins(cx);
        app.start_scanning(cx);
        app.watch_vault_inputs(window, cx);

        app.open_remembered_repositories(remote, cx);
        // Starting the server waits for the window to be mounted: a dialog needs
        // `Root`'s layers, which are only installed on the first render.
        cx.spawn_in(window, async move |this, cx| {
            this.update_in(cx, |app, window, cx| app.start_backend(window, cx))
                .ok();
        })
        .detach();
        // The window opens with the focus nowhere, and a focus handle nobody
        // holds puts no context on the stack: **every** shortcut was dead until
        // the first click landed on a live node. It is not a courtesy to the
        // keyboard, it is what makes the bindings resolve at all — same defect
        // as hiding a focused terminal, one step earlier.
        window.focus(&app.focus, cx);
        // The window manager's close — `Alt+F4`, its cross — is the one gesture
        // the platform takes without asking the view; this is where it is told
        // to ask. The callback is `Fn`, hence the weak handle: the window
        // outlives nothing here, but the signature does not know it.
        let this = cx.weak_entity();
        window.on_window_should_close(cx, move |window, cx| {
            this.update(cx, |app, cx| app.quit_or_ask(window, cx))
                .unwrap_or(true)
        });
        app
    }

    /// The previous session's repositories, then the directory named on the
    /// command line — or, failing that, the current one — if it is a
    /// repository: that is what somebody launching `claudhub` from their
    /// project expects.
    ///
    /// The check goes to the worker: `is_repo` costs a git subprocess, which the
    /// constructor is not allowed to pay for.
    fn open_remembered_repositories(&mut self, remote: bool, cx: &mut Context<Self>) {
        let remembered = Settings::global(cx).repositories.clone();
        for path in remembered {
            self.opening_repos += 1;
            self.git.send(Cmd::OpenRepo(path));
        }
        // Remotely, the directory that counts is the **server's**: it arrives
        // with `Evt::ServerHello`, and ours names nothing over there.
        if !remote {
            // Named on the command line, or merely the shell's working
            // directory: the first is a choice and outranks the session, the
            // second is ambient and yields to it.
            self.launch_chosen = self.launch_arg.is_some();
            let launched_in = self
                .launch_arg
                .clone()
                .or_else(|| std::env::current_dir().ok());
            if let Some(dir) = launched_in {
                self.launch_dir = Some(dir.clone());
                self.git.send(Cmd::OpenIfRepo(dir));
            }
        }
        // Once per launch, behind the repositories: on the network queue it
        // costs nobody anything, and the answer paints the status bar's
        // download link when this window is behind the published release.
        // Remotely this send is dropped with the rest — `backend_ready`
        // resends it with the repositories, and for the same reason.
        self.git.send(Cmd::ReleaseCheck);
    }

    /// One dock per screen, restored or built, and the one that was being looked
    /// at when the window was closed.
    ///
    /// The four are built at startup and not on first visit: a dock is built
    /// with `window`, and doing it at render time would amount to creating
    /// entities in the middle of a frame.
    fn build_dock(window: &mut Window, cx: &mut Context<Self>) -> Docks {
        // A saved layout takes precedence over the default one. The whole file
        // is discarded if its version differs: the panels may have changed name,
        // and rebuilding from unknown names would give a window full of empty
        // frames.
        let saved = load_layouts();
        // **Through `DockSkin` and not through `DockArea::new`.** Since the
        // layout engine moved into `gpui-base`, an area built without a skin
        // docks, drags and persists perfectly well — but draws **no chrome** at
        // all: no tab bar, no title, no frame. The panels then stack bare, which
        // reads as a broken window without a single error being reported.
        let (area, skin) = DockSkin::dock_area(
            crate::ui::dock_layout::DOCK_ID,
            Some(LAYOUT_VERSION),
            window,
            cx,
        );
        // `Segmented`: the rounded pill in a rail, in place of the bordered
        // rectangle whose radius is a hard-coded zero. It is our commit on the
        // fork that exposes this setting.
        skin.set_tab_variant(gpui_component::tab::TabVariant::Segmented, cx);
        // A tab bar everywhere, including on groups with a single panel: the
        // default (`Auto`) renders a flat title there, and "Branches" or
        // "Terminals" would not have the same band as their neighbours — two
        // chromes for one window.
        skin.set_panel_style(gpui_component::dock::PanelStyle::TabBar, cx);
        // **The two edges carry a title, not a strip of tabs.** The rails
        // already say what each zone holds and which of it is in front, one
        // button per tool window; a tab bar beside them says it a second time,
        // and the two disagree the moment one is truncated. The title stays a
        // drag source, so a tool window can still be taken out by hand.
        //
        // The centre keeps its tabs — documents are chosen there and nowhere
        // else — and so does the bottom, where several terminals live side by
        // side and their tabs carry the name, the `+` and the cross.
        for side in [
            gpui_component::dock::DockPlacement::Left,
            gpui_component::dock::DockPlacement::Right,
        ] {
            skin.set_panel_style_at(side, gpui_component::dock::PanelStyle::Title, cx);
        }
        // **No collapse affordance in the tab bars.** The dock draws, in the tab
        // bar of the group nearest each edge, a chevron that folds the
        // neighbouring zone away. The zones fold from the rails now, which is
        // where one looks for them: a control that goes away with the zone it
        // controls is a control one cannot come back through.
        skin.set_toggle_button_visible(false, cx);

        let restored = saved
            .area
            .and_then(|mut state| {
                // **Anything the dock cannot build**, and not a list of known
                // culprits: a plugin uninstalled between two sessions leaves its
                // name behind, and so does a panel of ours that has gone. Left
                // in, the registry prints "panel type is not registered" across
                // the window, at every start.
                //
                // The four regions, and not the centre alone — see `prune_dock`.
                let doomed = |name: &str| !crate::ui::panels::is_registered(name);
                let empty = prune(&mut state.center, &doomed);
                prune_dock(&mut state.left_dock, &doomed);
                prune_dock(&mut state.right_dock, &doomed);
                prune_dock(&mut state.bottom_dock, &doomed);
                // The window opens on "Changes", whatever tab was in front of
                // it when it last closed — see `open_on`. It sits in the left
                // zone now, so the zones are asked as well as the centre.
                let changes = crate::ui::panels::ChangesPanel::NAME;
                open_on(&mut state.center, changes);
                open_dock_on(&mut state.left_dock, changes);
                open_dock_on(&mut state.right_dock, changes);
                open_dock_on(&mut state.bottom_dock, changes);
                // Nothing left of the centre: what was saved says only that the
                // window used to hold panels that have gone, and the honest
                // answer is the default layout rather than empty frames kept for
                // their names.
                (!empty).then_some(state)
            })
            .and_then(|state| area.update(cx, |a, cx| a.load(state, window, cx)).ok())
            .is_some();
        if !restored {
            area.update(cx, |a, cx| {
                crate::ui::dock_layout::install_default_layout(a, window, cx);
            });
        }
        // The dock notifies on every move, resize or tab change: it is the save
        // signal, deferred so a drag does not write one file per pixel.
        cx.observe(&area, |this, _, cx| this.schedule_layout_save(cx))
            .detach();
        Docks {
            dock: area,
            dock_skin: skin,
            // The initial layout is written at once: without that, the file
            // keeps an earlier version's until the first move, and that is what
            // would be read back on the next startup.
            needs_save: !restored,
        }
    }

    /// Periodically polls the state of every open worktree.
    ///
    /// The file watcher only covers the displayed worktree; the others — those
    /// where an agent works while one reviews elsewhere — report nothing. A
    /// regular sweep is the only way to see them move, and that is precisely
    /// where what one wants to see happens.
    fn start_scanning(&mut self, cx: &mut Context<Self>) {
        cx.spawn(async move |this, cx| {
            let mut tick: u32 = 0;
            loop {
                let alive = this
                    .update(cx, |this, cx| {
                        this.scan_now(tick.is_multiple_of(SUMMARY_EVERY), cx);
                        this.auto_fetch_now(cx);
                        // The plugins ride the same clock. Expiring a request
                        // wants no finer grain than two seconds against a
                        // ninety-second ceiling, and re-reading the settings
                        // here is what lets a token corrected in the form take
                        // effect without recompiling the script that uses it.
                        this.expire_plugin_calls();
                        this.configure_plugins(cx);
                    })
                    .is_ok();
                if !alive {
                    return;
                }
                tick = tick.wrapping_add(1);
                cx.background_executor().timer(AGENT_PERIOD).await;
            }
        })
        .detach();
    }

    fn scan_now(&mut self, with_summaries: bool, cx: &mut Context<Self>) {
        let worktrees: Vec<PathBuf> = self
            .repos
            .iter()
            .flat_map(|repo| repo.worktrees.iter().map(|w| w.path.clone()))
            .collect();
        if worktrees.is_empty() {
            return;
        }
        if with_summaries {
            self.git.send(Cmd::LoadSummaries {
                worktrees: worktrees.clone(),
            });
            // The `wt` reading follows the summaries': these are shell commands
            // declared by the project, one per worktree, and there is no reason
            // to run them more often than a `git status`.
            self.scan_wt();
        }
        let programs = Settings::global(cx).terminal.agent_programs();
        self.git.send(Cmd::ScanAgents {
            worktrees,
            programs,
        });
    }

    /// Goes to fetch what is new in the open repositories, at the configured period.
    ///
    /// A timestamp rather than a tick count: the period is set in minutes and the
    /// sweep beats every two seconds, so a tick counter would bear no readable
    /// relation to the setting. It is also what makes changing the setting take
    /// effect at once.
    ///
    /// The first pass happens as soon as the first repository is open: what one
    /// wants to know on arriving is precisely what changed while one was away.
    /// While no repository is open, nothing is recorded — otherwise the opening
    /// would wait for a whole period.
    fn auto_fetch_now(&mut self, cx: &mut Context<Self>) {
        let Some(period) = Settings::global(cx).auto_fetch_period() else {
            return;
        };
        if self.repos.is_empty() {
            return;
        }
        if self
            .last_auto_fetch
            .is_some_and(|last| last.elapsed() < period)
        {
            return;
        }
        self.last_auto_fetch = Some(std::time::Instant::now());
        for main in self
            .repos
            .iter()
            .map(|repo| repo.main.clone())
            .collect::<Vec<_>>()
        {
            self.git.send(Cmd::AutoFetch { main });
        }
    }

    /// Saves the layout, once things have settled.
    ///
    /// The state is re-read at write time and not at call time: opening a dock
    /// zone is deferred by one frame, and capturing it at once would save the
    /// state from before the gesture.
    fn schedule_layout_save(&mut self, cx: &mut Context<Self>) {
        if self.layout_save_scheduled {
            return;
        }
        self.layout_save_scheduled = true;
        cx.spawn(async move |this, cx| {
            cx.background_executor()
                .timer(std::time::Duration::from_millis(700))
                .await;
            let _ = this.update(cx, |this, cx| {
                this.layout_save_scheduled = false;
                let mut state = this.dock.read(cx).dump(cx);
                // The terminals are taken out before writing: a terminal is a
                // **process**, and a layout is read long after that process has
                // died. Rebuilding one from its name would be a tab showing
                // nothing.
                //
                // A file's tab goes the same way, and for a reason one step
                // removed: its content is a **text read off the disk**, which a
                // builder cannot fetch without a round trip — a tab read back
                // would be an empty frame. What the previous session had open is
                // remembered where the rest of the place one was is: in the
                // store, see `ui::session`.
                //
                // The four regions, not the centre alone: the terminals dock
                // into a side zone, which is where an unpruned one would come
                // back as a name nothing can build.
                // A console's tab goes with them, and for the file's reason:
                // its content is a query one was writing, several of them at a
                // time, each on its own connection. What the previous session
                // had open comes back from the store, beside the files.
                let doomed = |name: &str| {
                    name == super::panels::TerminalPanel::NAME
                        || name == super::panels::FilePanel::NAME
                        || name == super::panels::QueryPanel::NAME
                };
                prune(&mut state.center, &doomed);
                prune_dock(&mut state.left_dock, &doomed);
                prune_dock(&mut state.right_dock, &doomed);
                prune_dock(&mut state.bottom_dock, &doomed);
                let layouts = Layouts {
                    version: Some(LAYOUT_VERSION),
                    area: Some(state),
                };
                save_layouts(&layouts);
            });
        })
        .detach();
    }

    /// Drains the workers' events in batches.
    ///
    /// In batches because one `update_in` per event forces a gpui effect cycle
    /// every time: opening a repository, which produces a dozen of them, would
    /// cost ten renders instead of one.
    pub(super) fn pump_events(
        &mut self,
        events: async_channel::Receiver<Evt>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        const BATCH: usize = 64;
        cx.spawn_in(window, async move |this, cx| {
            while let Ok(evt) = events.recv().await {
                let mut batch = vec![evt];
                while batch.len() < BATCH {
                    let Ok(next) = events.try_recv() else { break };
                    batch.push(next);
                }
                let alive = this
                    .update_in(cx, |app, window, cx| {
                        for evt in batch {
                            app.handle_event(evt, window, cx);
                        }
                    })
                    .is_ok();
                if !alive {
                    break; // the window is closed
                }
            }
        })
        .detach();
    }

    /// The folders later launches hand over (`crate::instance`).
    ///
    /// No batching, unlike the events: these arrive at the speed of a hand on a
    /// context menu, and each one is a gesture whose answer is a window coming
    /// forward.
    fn pump_handoffs(
        &mut self,
        handoffs: async_channel::Receiver<Option<PathBuf>>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        cx.spawn_in(window, async move |this, cx| {
            while let Ok(folder) = handoffs.recv().await {
                let alive = this
                    .update_in(cx, |app, window, cx| app.handed_folder(folder, window, cx))
                    .is_ok();
                if !alive {
                    break; // the window is closed
                }
            }
        })
        .detach();
    }

    /// A second launch gave us its folder, or asked for nothing but the window.
    ///
    /// The selection rank is **put back to nothing**: the folder is a choice
    /// made by hand, and `pick_worktree` grades a launch directory
    /// `SELECTION_CHOSEN` — which would not displace the `SELECTION_CHOSEN`
    /// already there, so the answer would arrive and change nothing.
    fn handed_folder(
        &mut self,
        folder: Option<PathBuf>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        // First, and whatever the folder turns out to be: what was asked for is
        // this window, and a click that raises nothing looks like a click that
        // did nothing. The other process handed us the right to do it
        // (`instance::allow_foreground`). Under Wayland the compositor keeps
        // the last word — raising a window one did not click is exactly what it
        // is there to refuse — and the request may come out as a highlight in
        // the dock rather than a window in front. Nothing to fix here: it is
        // the platform's answer, and it is the same one every editor gets.
        window.activate_window();
        let Some(folder) = folder else {
            return;
        };
        // Translated before it is remembered: `launch_dir` is compared against
        // the worktrees git enumerates, which are the **server's** paths.
        match self.repo_path_for_server(folder, cx) {
            // Nothing reaches a server that is starting up or down, and a
            // dropped command is a click that did nothing: it waits for the
            // handshake instead.
            Ok(folder) => match self.server_state {
                super::server::ServerState::Local | super::server::ServerState::Up => {
                    self.open_handed(folder)
                }
                _ => self.pending_handoff = Some(folder),
            },
            Err(message) => self.announce(message, cx),
        }
        cx.notify();
    }

    /// Opens a folder somebody handed over, on the server's terms.
    fn open_handed(&mut self, folder: PathBuf) {
        self.launch_dir = Some(folder.clone());
        // Handed over is named: « Ouvrir avec Claudhub » on a folder is a
        // choice, and it must beat the remembered session.
        self.launch_chosen = true;
        self.selection_rank = crate::ui::session::SELECTION_NONE;
        // `OpenRepo` and not `OpenIfRepo`, as in the folder picker: a folder
        // named by hand deserves an answer, including "this is not a
        // repository".
        self.git.send(Cmd::OpenRepo(folder));
    }

    // — A worktree's free note ————————————————————————————————————

    /// Schedules the write of the free note on every keystroke.
    ///
    /// Deferred by a second, like the settings and for the same reason: an input
    /// emits a value per keystroke, and a vault gets synced — writing on every
    /// character would keep the sync busy permanently. No "save" button: it is a
    /// scratchpad, and having to confirm it would be the best way to lose three
    /// sentences in it.
    fn watch_vault_inputs(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        use gpui_component::input::InputEvent;
        cx.subscribe(&self.journal_input.clone(), |this, _, event, cx| {
            if matches!(event, InputEvent::Change) {
                this.schedule_journal_save(cx);
            }
        })
        .detach();
        // `subscribe_in` and not `subscribe`: clearing a field needs a window,
        // which the event alone does not carry.
        //
        // Enter adds and leaves the field ready for the next: a list is filled
        // in one go, and picking the mouse back up between two tasks is exactly
        // the gesture this was meant to remove.
        cx.subscribe_in(
            &self.task_input.clone(),
            window,
            |this, input, event, window, cx| {
                if !matches!(event, InputEvent::PressEnter { .. }) {
                    return;
                }
                let label = input.read(cx).value().to_string();
                this.add_task(&label, cx);
                input.update(cx, |input, cx| input.set_value("", window, cx));
            },
        )
        .detach();
        // Enter confirms, and so does losing the focus: `InputState` has no
        // escape event, and abandoning a correction because one clicked
        // elsewhere would be the worse of the two flaws.
        cx.subscribe_in(
            &self.task_edit_input.clone(),
            window,
            |this, input, event, _window, cx| {
                if !matches!(event, InputEvent::PressEnter { .. } | InputEvent::Blur) {
                    return;
                }
                let label = input.read(cx).value().to_string();
                this.commit_task_edit(&label, cx);
            },
        )
        .detach();
    }

    fn schedule_journal_save(&mut self, cx: &mut Context<Self>) {
        if self.journal_save {
            return;
        }
        self.journal_save = true;
        cx.spawn(async move |this, cx| {
            cx.background_executor()
                .timer(std::time::Duration::from_secs(1))
                .await;
            let _ = this.update(cx, |this, cx| {
                this.journal_save = false;
                this.persist_journal(cx);
            });
        })
        .detach();
    }

    /// Writes the displayed worktree's free note, or erases it if it is empty.
    fn persist_journal(&mut self, cx: &mut Context<Self>) {
        let Some(worktree) = self.active.clone() else {
            return;
        };
        let Some(dir) = self.notes_dir(&worktree, cx) else {
            return;
        };
        let text = self.journal_input.read(cx).value().to_string();
        let Some(state) = self.review.get_mut(&worktree) else {
            return;
        };
        // Nothing while the folder has not answered: what is held in memory at
        // startup is a blank page, and writing it would erase the note that has
        // not been read yet.
        if !state.notes_loaded || state.journal == text {
            return;
        }
        let expect = (!state.journal.is_empty()).then(|| crate::files::digest(&state.journal));
        state.journal = text.clone();
        self.git.send(Cmd::WriteVaultFile {
            worktree,
            path: crate::wslpath::join(&dir, crate::ui::vault::NOTES),
            text,
            expect,
        });
    }

    /// Puts the displayed worktree's note back into the input.
    ///
    /// Never while it is being written in: the vault is re-read on every write —
    /// ours included — and putting the disk's text back under one's fingers
    /// would move the cursor into the middle of a sentence. What arrives
    /// meanwhile will therefore wait for the next load, and that is the right
    /// trade: two hands on the same paragraph have no merge.
    fn sync_journal_input(&mut self, worktree: &Path, window: &mut Window, cx: &mut Context<Self>) {
        if self.active.as_deref() != Some(worktree) {
            return;
        }
        if self
            .journal_input
            .read(cx)
            .focus_handle(cx)
            .is_focused(window)
        {
            return;
        }
        let Some(text) = self.review.get(worktree).map(|state| state.journal.clone()) else {
            return;
        };
        if self.journal_input.read(cx).value() == text.as_str() {
            return;
        }
        self.journal_input
            .update(cx, |input, cx| input.set_value(text, window, cx));
    }

    /// The two directories `file_changed` weighs a path against.
    ///
    /// Resolved once for a batch of paths: both are joins and lookups, and a
    /// single write in a busy worktree brings dozens of paths at a time.
    fn watched_dirs(&self, cx: &App) -> (Option<PathBuf>, Option<PathBuf>) {
        let plugins = crate::ui::plugin_view::plugins_dir();
        let vault = self
            .active
            .clone()
            .and_then(|active| self.notes_dir(&active, cx));
        (plugins, vault)
    }

    fn file_changed(
        &mut self,
        path: &Path,
        dirs: &(Option<PathBuf>, Option<PathBuf>),
        cx: &mut Context<Self>,
    ) {
        let (plugins, vault) = dirs;
        // Before the worktree guard: a plugin's directory is not in a worktree,
        // and its script has to be picked up whether or not one is open.
        if let Some(dir) = plugins {
            if path.starts_with(dir) {
                self.reload_plugins(cx);
                return;
            }
        }
        let Some(active) = self.active.clone() else {
            return;
        };
        // A project's `wt.toml`, re-read like the justfile below: a task or a
        // question added while Claudhub is open has to reach the menu. Before
        // the worktree guard because the file belongs to the **main**
        // repository, and only the watched checkout reports it — which is to
        // say it is seen when the main is the checkout on screen; edited from
        // a linked worktree, it is read again the next time the main is shown.
        if path.file_name().is_some_and(|name| name == "wt.toml") {
            let main = self
                .repos
                .iter()
                .map(|repo| repo.main.clone())
                .find(|main| crate::wslpath::join(main, "wt.toml") == path);
            if let Some(main) = main {
                self.reload_wt_project(&main);
            }
        }
        // The vault is not inside the worktree, and what changes there is not
        // read with `git status`: it is the folder itself that has to be re-read.
        if let Some(vault) = vault.clone() {
            if path.starts_with(&vault) {
                self.git.send(Cmd::ReadNotes {
                    worktree: active,
                    dir: vault,
                });
                return;
            }
        }
        // A linked worktree lives inside another in some arrangements; reacting
        // only for the displayed checkout avoids a double refresh and a
        // flickering list.
        if !path.starts_with(&active) {
            return;
        }
        // A recipe added while Claudhub is open has to show up in the menu: the
        // justfile is a file one edits during the very session it drives.
        if path
            .file_name()
            .and_then(|name| name.to_str())
            .is_some_and(crate::just::is_justfile)
        {
            self.reload_just(&active);
        }
        // A test added while Claudhub is open has to show up in the panel: the
        // suite is what one edits during the very session that runs it.
        if crate::ui::tests_view::reloads(&active, path) {
            self.reload_pest(&active, cx);
        }
        self.request_status(active);
        cx.notify();
    }

    /// **One `match` and one arm per variant**, each of the fat ones delegating
    /// to a named method: the exhaustiveness check is what tells whoever adds an
    /// `Evt` that they have a view to update, and grouping the arms behind
    /// anything else would trade that guarantee for shorter functions.
    fn handle_event(&mut self, evt: Evt, window: &mut Window, cx: &mut Context<Self>) {
        match evt {
            // — Repositories and worktrees ————————————————————————
            Evt::RepoOpened {
                main,
                name,
                worktrees,
                opened_at,
            } => self.repo_opened(main, name, worktrees, opened_at, window, cx),
            Evt::RepoUnavailable { path, message } => {
                self.opening_repos = self.opening_repos.saturating_sub(1);
                self.repo_unavailable(path, message, cx)
            }
            Evt::Worktrees { main, worktrees } => {
                self.worktrees_arrived(main, worktrees, window, cx)
            }
            Evt::Summaries { summaries } => {
                self.summaries.extend(summaries);
            }
            Evt::Agents { agents } => self.agents_scanned(agents),

            // — Review ————————————————————————————————————————————
            Evt::Status { worktree, status } => self.status_arrived(worktree, status, cx),
            Evt::DiffFiles {
                worktree,
                range,
                files,
            } => self.diff_files_arrived(worktree, range, files, cx),
            Evt::FileDiff {
                worktree,
                path,
                diff,
            } => self.file_diff_arrived(worktree, path, diff, cx),
            Evt::UnstagedDiff {
                worktree,
                path,
                diff,
            } => self.unstaged_arrived(worktree, path, diff, cx),
            Evt::CommitDetail {
                worktree,
                id,
                author,
                date,
                message,
            } => self.commit_detail_arrived(worktree, id, author, date, message, cx),
            Evt::History {
                worktree,
                range,
                commits,
                graph,
                patches,
            } => self.history_arrived(worktree, range, commits, graph, patches, cx),
            Evt::Branches {
                main,
                branches,
                default_base,
            } => self.branches_arrived(main, branches, default_base, window, cx),
            Evt::Tags { main, tags } => self.tags_arrived(main, tags, cx),
            Evt::RemoteTags { main, names } => self.remote_tags_arrived(main, names, cx),
            Evt::Stashes { main, stashes } => self.stashes_arrived(main, stashes, cx),

            // — Writes ————————————————————————————————————————————
            Evt::Done {
                worktree,
                action,
                output,
            } => self.action_done(worktree, action, output, window, cx),
            Evt::Failed {
                worktree,
                action,
                message,
            } => self.action_failed(worktree, action, message, window, cx),
            Evt::Fetched { main } => self.fetched(main),
            Evt::ReleaseChecked { version, url } => {
                // Compared against the window's own version, not the
                // fetcher's: in remote mode the server did the round trip,
                // and the window is what gets updated.
                if crate::release::is_newer(&version, env!("CARGO_PKG_VERSION")) {
                    self.update = Some((SharedString::from(version), SharedString::from(url)));
                    cx.notify();
                }
            }
            Evt::CommitMessage { worktree, message } => {
                self.commit_message_arrived(worktree, message, window, cx)
            }

            // — `wt` ——————————————————————————————————————————————
            Evt::WtProject { main, project } => {
                self.wt_projects.insert(main, project);
            }
            Evt::JustRecipes { worktree, recipes } => {
                self.just_recipes
                    .insert(worktree, recipes.map(std::rc::Rc::new));
            }
            Evt::TestsFound { worktree, report } => self.pest_arrived(worktree, report, cx),
            Evt::TestsLine { worktree, id, line } => self.pest_line(worktree, id, line, cx),
            Evt::TestsRan { worktree, id, run } => self.pest_ran(worktree, id, run, cx),
            Evt::WtQuestions {
                main,
                slug,
                answers,
                questions,
                // The phase and the task came back so a transport could route
                // them; the view knows them from the dialog it opened.
                phase: _,
                task: _,
                round,
            } => self.wt_questions_arrived(
                crate::ui::worktree_ops::WtRound {
                    main,
                    slug,
                    answers,
                    questions,
                    round,
                },
                window,
                cx,
            ),
            Evt::WtTask {
                worktree,
                task,
                launch,
            } => self.wt_task_ready(worktree, task, launch, window, cx),
            Evt::WtStates { states } => {
                self.wt_states.extend(states);
            }
            Evt::WtLinks {
                worktree,
                endpoints,
            } => {
                self.wt_links.insert(worktree, Some(endpoints));
            }
            Evt::WtProgress {
                main,
                slug,
                op,
                line,
                warning,
            } => self.wt_progress(&main, &slug, op, line, warning, cx),

            // — Sentry ————————————————————————————————————————————

            // — Files and vault ———————————————————————————————————
            Evt::ProjectFiles {
                worktree,
                files,
                ignored,
                dirs,
            } => self.project_files_arrived(worktree, files, ignored, dirs),
            Evt::DirListed {
                worktree,
                dir,
                result,
            } => self.dir_listed(worktree, dir, result),
            Evt::FileContent {
                worktree,
                path,
                content,
            } => self.file_content_arrived(worktree, path, content, window, cx),
            Evt::ImageContent {
                worktree,
                path,
                image,
            } => self.image_content_arrived(worktree, path, image, window, cx),
            Evt::FileBase {
                worktree,
                path,
                text,
            } => self.file_base_arrived(worktree, path, text, cx),
            Evt::MergeStages {
                worktree,
                path,
                result,
            } => self.merge_arrived(worktree, path, result, cx),
            Evt::SearchDone {
                worktree,
                request,
                result,
            } => {
                // An answer about a worktree one has left: kept all the same,
                // the panel saying which checkout its results belong to. What
                // would be wrong is showing them as this worktree's.
                let _ = worktree;
                self.search_done(request, result, window, cx)
            }
            Evt::Preview {
                worktree,
                path,
                content,
            } => self.preview_arrived(worktree, path, content, cx),
            Evt::FilesChanged { paths } => {
                let dirs = self.watched_dirs(cx);
                for path in paths {
                    self.file_changed(&path, &dirs, cx);
                }
            }
            // The vault has been written: it exists, so it can be watched.
            // Re-reading it straight away is not a luxury — the disk is the
            // authority, and a refused write (the agent had touched the file
            // meanwhile) has to give the view back what is really there rather
            // than what we thought we had put in it.
            Evt::VaultWritten { worktree } => {
                self.watch_vault(&worktree, cx);
                if let Some(dir) = self.notes_dir(&worktree, cx) {
                    self.git.send(Cmd::ReadNotes { worktree, dir });
                }
            }
            Evt::NotesRead { worktree, files } => self.notes_read(worktree, files, window, cx),

            // — Plugins ————————————————————————————————————————————
            Evt::PluginResult {
                plugin,
                call,
                result,
            } => self.plugin_result(plugin, call, result),
            Evt::PluginManaged { dir, op, result } => {
                self.plugin_managed(dir, op, result, window, cx)
            }

            // — The language server ————————————————————————————————
            Evt::LspReady {
                worktree,
                name,
                capabilities,
            } => self.lsp_ready(worktree, name, capabilities, window, cx),
            Evt::LspStopped { worktree, reason } => self.lsp_stopped(worktree, reason, cx),
            Evt::LspAnswer { id, result, .. } => self.lsp_answer(id, result),
            Evt::LspDiagnostics {
                worktree,
                path,
                diagnostics,
            } => self.lsp_diagnostics(worktree, path, diagnostics, cx),
            Evt::LspBusy { worktree, message } => self.lsp_busy(worktree, message, cx),
            Evt::LspApplyEdit { worktree, id, edit } => {
                self.lsp_apply_edit(worktree, id, edit, window, cx)
            }

            // — Databases —————————————————————————————————————————
            Evt::DbDatabases { key, databases } => self.db_databases_arrived(key, databases, cx),
            Evt::DbTables {
                key,
                database,
                tables,
            } => self.db_tables_arrived(key, database, tables, cx),
            Evt::DbColumns {
                key,
                database,
                table,
                columns,
            } => self.db_columns_arrived(key, database, table, columns, cx),
            Evt::DbAllColumns {
                key,
                database,
                columns,
            } => self.db_all_columns_arrived(key, database, columns, cx),
            Evt::DbRows {
                request,
                rows,
                elapsed_ms,
            } => self.db_rows_arrived(request, rows, elapsed_ms, cx),
            Evt::DbExported { path, rows } => self.db_exported(path, rows, cx),

            // — Transport —————————————————————————————————————————
            Evt::ServerHello {
                build,
                cwd,
                running_under_wsl,
                shells,
            } => self.server_hello(build, cwd, running_under_wsl, shells),
            Evt::ServerLost { message } => {
                log::warn!("remote server lost: {message}");
                self.server_state = super::server::ServerState::Down(message);
                // Whatever was under way died with it, and nothing will ever
                // answer for it. A spinner that turns for ever says less than
                // one that never turned — the status bar carries the reason.
                self.clear_running();
            }
        }
        cx.notify();
    }

    // — Repositories and worktrees ————————————————————————————————

    fn repo_opened(
        &mut self,
        main: PathBuf,
        name: String,
        worktrees: Vec<Worktree>,
        opened_at: Option<PathBuf>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        // Which checkout to show, and how firmly. The decision is outside the
        // view and tested: the repositories answer in whatever order the
        // workers finish, and a rule that gets it wrong shows a worktree
        // nobody asked for.
        let paths: Vec<PathBuf> = worktrees.iter().map(|w| w.path.clone()).collect();
        let candidate = crate::ui::session::pick_worktree(
            opened_at,
            &paths,
            self.launch_dir.as_deref(),
            self.launch_chosen,
            self.restoring.worktree.as_deref(),
        );
        // `open` says no when the repository is already there: reopening must
        // not duplicate its row. A folder that is back — remounted, recloned —
        // stops being missing at the same time.
        //
        // The selection below runs **either way**, and that is not a detail:
        // the repository one launches from is usually a remembered one too, so
        // it is already open by the time `OpenIfRepo` answers — and that
        // answer is the only one that names the deep checkout one is standing
        // in rather than the main repository.
        if self.repos.open(main.clone(), name, worktrees) {
            Settings::update_global(cx, |s| s.remember_repository(&main));
            self.forget_missing_worktrees(&main, cx);
            self.ensure_wt_project(&main);
            self.git.send(Cmd::LoadBranches { main });
        }
        // A weaker candidate never displaces a firmer one, and re-selecting is
        // free: `select_worktree` returns at once when the path is already the
        // active one.
        if let Some((rank, path)) = candidate.filter(|(rank, _)| *rank > self.selection_rank) {
            // The rank is **given** rather than lowered after the fact:
            // choosing by hand and filling the window while the repositories
            // answer are the same code, and what tells them apart is exactly
            // this number — which the first terminal reads before opening
            // anywhere.
            self.select_worktree_ranked(path, rank, window, cx);
        }
        // Answered, so it no longer stands in the way of the session's
        // terminal. **After** the selection above, which is what may have just
        // made this repository the one being looked at.
        self.opening_repos = self.opening_repos.saturating_sub(1);
        self.ensure_session_terminal(window, cx);
        // And where the work stood there, for the same reason and under the
        // same rule: a stopgap that nothing displaces *becomes* the answer, and
        // it is only once nothing is owed that it may open anything.
        self.settle_place(window, cx);
    }

    fn repo_unavailable(&mut self, path: PathBuf, message: String, cx: &mut Context<Self>) {
        log::warn!("{} does not open: {message}", path.display());
        // Remembered: it stays on screen, in error, with what is needed to
        // remove it. Named just now in a folder picker: there is nothing to
        // keep, only a reason to give.
        if Settings::global(cx)
            .repositories
            .iter()
            .any(|remembered| remembered == &path)
        {
            self.repos.mark_missing(path, message);
        } else {
            self.announce_error(SharedString::from(message), cx);
        }
    }

    fn worktrees_arrived(
        &mut self,
        main: PathBuf,
        worktrees: Vec<Worktree>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.repos.set_worktrees(&main, worktrees);
        // git has just enumerated: it is the only moment the list is certain, so
        // the only one where forgetting an entry is safe.
        self.forget_missing_worktrees(&main, cx);
        // A worktree created for an issue is waiting for its prompt: it is the
        // only signal that says `wt` has finished its hooks.
        self.deliver_awaited_agent(window, cx);
        // The active worktree may have been removed under our feet.
        if let Some(active) = self.active.clone() {
            if !self.worktree_exists(&active) {
                self.active = None;
                self.review.remove(&active);
                self.close_terminals_of(&active, window, cx);
                self.close_consoles_of(&active, window, cx);
                // The editors of a worktree that is gone go with it, as its
                // terminals do: the files they hold are on a disk that no
                // longer has them, and their tabs would stay in the dock.
                self.close_files_of(&active, window, cx);
                self.jumps.remove(&active);
                self.forget_finders(&active);
                if let Some(first) = self.first_worktree() {
                    self.select_worktree(first, window, cx);
                }
            }
        }
    }

    /// "Working" means **has burnt processor since the previous reading**.
    fn agents_scanned(&mut self, agents: crate::agent::Agents) {
        self.agents.update(agents);
    }

    // — Review ————————————————————————————————————————————————————

    fn status_arrived(&mut self, worktree: PathBuf, status: Status, cx: &mut Context<Self>) {
        self.pending_status.remove(&worktree);
        let base = self.default_base_for(&worktree);
        self.ensure_review(&worktree, cx);
        // `ensure_review` has just put it there, so no key is copied to look it
        // up.
        let Some(state) = self.review.get_mut(&worktree) else {
            return;
        };
        // The branch the status reports is the one that is checked out **now**:
        // a checkout rereads the status, and nothing rereads the worktree list,
        // so this is where the title bar's branch stops being the one one has
        // just left.
        let branch = status.branch.clone();
        state.status = status;
        state.rows_changed();
        self.repos.set_branch(&worktree, branch.as_deref());
        if state.base.is_none() {
            state.base = base;
        }
        // A merge open on a file git no longer reports as unmerged is a merge
        // that has been settled elsewhere — the two buttons of the conflicts
        // row, an abort, a `git checkout` in a terminal next door. What it
        // shows would be the index as it was, and every button on it would act
        // on a conflict that is over.
        let settled = self.merging.as_ref().is_some_and(|merge| {
            merge.worktree == worktree
                && self
                    .review
                    .get(&worktree)
                    .is_some_and(|state| !unmerged(state, &merge.path))
        });
        if settled {
            self.merging = None;
        }
        // The tree's rows read the status by path: filed once here rather than
        // at every frame of the panel.
        self.index_file_status(&worktree);
        let state = self.review.entry(worktree.clone()).or_default();
        // The lists depend on the status: a file just staged must not stay
        // displayed on the wrong side. We **ask for them again** without
        // emptying them: clearing what is on screen before having something to
        // replace it makes the list flicker on every refresh, and one arrives
        // per file write.
        let stale: Vec<DiffRange> = state.files.keys().cloned().collect();
        for range in stale {
            if state.pending_files.insert(range.clone()) {
                self.git.send(Cmd::LoadDiffFiles {
                    worktree: worktree.clone(),
                    range,
                });
            }
        }
        // The remainder panel follows the index: a hunk just added from it
        // must leave it, and a stage done in a terminal next door must make
        // it appear.
        if matches!(state.range, DiffRange::Working) {
            if let Some(path) = state.selected.clone() {
                if partially_staged(&state.status, &path) {
                    self.git.send(Cmd::LoadUnstagedDiff {
                        worktree,
                        path,
                        context: Settings::global(cx).context_lines(),
                    });
                } else {
                    state.unstaged = None;
                }
            }
        }
    }

    fn diff_files_arrived(
        &mut self,
        worktree: PathBuf,
        range: DiffRange,
        files: Vec<DiffFile>,
        cx: &mut Context<Self>,
    ) {
        let Some(state) = self.review.get_mut(&worktree) else {
            return;
        };
        state.pending_files.remove(&range);
        // Did the displayed file come from this range, and is it still there? If
        // it has gone — staged, discarded, committed — leaving its diff on
        // screen would mean reviewing a state that no longer exists.
        let gone = state.range == range
            && state
                .selected
                .as_ref()
                .is_some_and(|path| !files.iter().any(|f| &f.path == path));
        // A stale review goes away with the list that disproves it: the file has
        // gone from the range, or it has changed again since it was ticked.
        // Keeping it would say "reviewed" of content nobody has read.
        let before = state.reviewed.len();
        state.reviewed.retain(|item| {
            item.range != range
                || files.iter().any(|file| {
                    file.path == item.path
                        && file.added == item.added
                        && file.removed == item.removed
                })
        });
        let pruned = state.reviewed.len() != before;
        // A commit just opened goes straight to its diff: the click on the
        // history, a tag or a stash promised reading, and waiting for a second
        // click on the file list left the centre saying "pick a file". Only
        // while nothing is selected — a selection is the user's, and the lines
        // shortcut of `open_commit` sets one before this list arrives — and
        // only on the worktree being looked at: a late answer must not pull
        // the screen towards a worktree one has left.
        let first = (matches!(range, DiffRange::Commit { .. })
            && state.range == range
            && state.selected.is_none()
            && self.active.as_deref() == Some(worktree.as_path()))
        .then(|| files.first().map(|file| file.path.clone()))
        .flatten();
        state.files.insert(range.clone(), files);
        state.rows_changed();
        if gone {
            state.selected = None;
            state.diff = None;
            state.unstaged = None;
            state.diff_selection = None;
        }
        if pruned {
            self.persist_notes(&worktree, cx);
        }
        if let Some(path) = first {
            self.open_file(worktree, path, range, cx);
        }
    }

    /// The unstaged remainder of a partially staged file, for the panel under
    /// the diff. A late answer for a file no longer shown is dropped.
    fn unstaged_arrived(
        &mut self,
        worktree: PathBuf,
        path: PathBuf,
        diff: crate::git::FileDiff,
        cx: &mut Context<Self>,
    ) {
        let Some(state) = self.review.get_mut(&worktree) else {
            return;
        };
        if state.selected.as_deref() != Some(path.as_path())
            || !matches!(state.range, DiffRange::Working)
        {
            return;
        }
        state.unstaged = Some((path, std::rc::Rc::new(diff)));
        cx.notify();
    }

    fn file_diff_arrived(
        &mut self,
        worktree: PathBuf,
        path: PathBuf,
        diff: crate::git::FileDiff,
        cx: &mut Context<Self>,
    ) {
        // The theme is read before the mutable borrow of the state: the
        // highlighting depends on it, and `cx.theme()` borrows `cx`.
        let theme = cx.theme().highlight_theme.clone();
        let (split, whole_file) = {
            let settings = Settings::global(cx);
            (settings.diff_split, settings.diff_whole_file)
        };
        let mut jumped = None;
        let mut note = None;
        if let Some(state) = self.review.get_mut(&worktree) {
            if state.selected.as_deref() == Some(path.as_path()) {
                let rendered = std::rc::Rc::new(Rendered::new(&path, diff, &theme));
                // The arrow that opened this file expects a change, not the top
                // of the file.
                if let Some(jump) = state.pending_jump.take() {
                    let stops = rendered.blocks(split, whole_file);
                    jumped = match jump {
                        Jump::First => stops.first(),
                        Jump::Last => stops.last(),
                    }
                    .map(|(start, _)| *start);
                    state.diff_selection = jumped.map(|row| (row, row));
                }
                note = state.pending_note.take();
                state.diff = Some(rendered);
                // The hits carry offsets into a text that has just been
                // replaced.
                self.diff_search.valid = false;
            }
        }
        // The annotated lines follow from the diff that has just arrived: it is
        // the only moment the computation happens, and certainly not in the
        // list's render.
        self.refresh_note_marks(&worktree);
        if let Some(id) = note {
            self.select_note_rows(id, cx);
        } else if let Some(row) = jumped {
            // The same placement as a hunk step, which is what this is the tail
            // of: entering the neighbouring file by the end one came from brings
            // that hunk under the eye, not wherever the list happened to be.
            self.reveal_diff_hunk(row, cx);
        }
    }

    /// The opened commit's message has arrived. Filed only while the commit is
    /// still the one on screen: the answer follows a git command, and the
    /// selection may have moved on during the read.
    fn commit_detail_arrived(
        &mut self,
        worktree: PathBuf,
        id: String,
        author: String,
        date: String,
        message: String,
        cx: &mut Context<Self>,
    ) {
        let Some(state) = self.review.get_mut(&worktree) else {
            return;
        };
        if state.commit.as_deref() != Some(id.as_str()) {
            return;
        }
        // The abbreviated hash the history computed, when the commit came from
        // there — a tag's or a stash's commit did not, and a plain truncation
        // captions those well enough.
        let short = state
            .history
            .as_ref()
            .and_then(|history| history.commits.iter().find(|c| c.id == id))
            .map(|c| c.short.clone())
            .unwrap_or_else(|| id.chars().take(10).collect());
        let (subject, body) = match message.split_once('\n') {
            Some((subject, body)) => (subject.to_string(), body.trim_start().to_string()),
            None => (message, String::new()),
        };
        state.commit_detail = Some(std::rc::Rc::new(CommitDetail {
            id,
            short: short.into(),
            author: author.into(),
            date: date.into(),
            subject: subject.into(),
            body: body.into(),
        }));
        cx.notify();
    }

    fn history_arrived(
        &mut self,
        worktree: PathBuf,
        range: LogRange,
        commits: Vec<Commit>,
        graph: Vec<GraphRow>,
        patches: Vec<crate::git::FileDiff>,
        cx: &mut Context<Self>,
    ) {
        let first = matches!(range, LogRange::Lines { .. }) && !commits.is_empty();
        let Some(state) = self.review.get_mut(&worktree) else {
            return;
        };
        state.history_pending = false;
        // A late answer, for a range no longer being looked at, would replace
        // the history with the wrong one.
        if state.history_range == range {
            let width = crate::git::history::width(&graph);
            // The row's texts are built here, once, and not in the list's
            // closure: that one runs for every visible row of every frame, and
            // it was four strings copied per row.
            let texts = crate::ui::history_view::commit_texts(&commits);
            state.history = Some(std::rc::Rc::new(History {
                commits,
                graph,
                patches,
                texts,
                width,
            }));
        }
        // A line history is asked for by a gesture on a selection, and it is
        // that selection's story one wants to read: showing the list without
        // opening anything would ask for a second click to say what was already
        // said.
        if first && self.active.as_deref() == Some(worktree.as_path()) {
            self.open_commit(0, cx);
        }
    }

    fn branches_arrived(
        &mut self,
        main: PathBuf,
        branches: Vec<Branch>,
        default_base: Option<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(repo) = self.repos.iter_mut().find(|r| r.main == main) {
            repo.branches = branches;
            repo.default_base = default_base;
        }
        // The reviews already open may have been waiting for this base: the
        // status arrives before the branches, and nothing will refresh them a
        // second time.
        let bases: Vec<(PathBuf, Option<String>)> = self
            .review
            .keys()
            .map(|worktree| (worktree.clone(), self.default_base_for(worktree)))
            .collect();
        for (worktree, base) in bases {
            if let Some(state) = self.review.get_mut(&worktree) {
                if state.base.is_none() {
                    state.base = base;
                }
            }
        }
        // After the propagation, not before: the selector has to show the base
        // kept, and it has just been decided.
        self.refresh_base_choices(window, cx);
    }

    // — Writes ————————————————————————————————————————————————————

    fn action_done(
        &mut self,
        worktree: Option<PathBuf>,
        action: Action,
        output: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.finish(&worktree, action);
        // A git write can have moved `HEAD` under the files being edited — a
        // commit is the everyday case, and it is the one where the gutter's
        // marks have to go quiet. Hooked on a completed gesture and not on a
        // status refresh: one of those arrives on every file the watcher sees
        // change, and this asks git a question per open tab.
        self.refresh_editor_bases(worktree.as_deref());
        // The committed message is spent — the parked draft too: the commit
        // may finish after one has moved on, and the input then holds another
        // worktree's draft, which a blind clear would erase. `CommitPush` as
        // well: it is the same message, sent by the other button.
        if matches!(action, Action::Commit | Action::CommitPush) {
            if let Some(state) = worktree.as_deref().and_then(|w| self.review.get_mut(w)) {
                state.commit_draft.clear();
            }
            if worktree.is_some() && worktree == self.active {
                self.commit_input.update(cx, |input, cx| {
                    input.set_value("", window, cx);
                });
            }
        }
        self.report(
            action,
            &output,
            crate::ui::notify::Level::Success,
            window,
            cx,
        );
        // A `wt` console closes on success; the balloon above keeps the result.
        self.wt_operation_ended(action, true, window, cx);
        // The integration has succeeded: what is left is to decide the fate of
        // the worktree and its branch, which `wt` deliberately keeps.
        if action == Action::Integrate {
            self.offer_cleanup(window, cx);
        }
        // An operation that moved HEAD also changes the branches.
        if matches!(
            action,
            Action::Commit | Action::Fetch | Action::Pull | Action::Push | Action::Checkout
        ) {
            if let Some(main) = worktree.as_deref().and_then(|w| self.main_of(w)) {
                self.git.send(Cmd::LoadBranches { main });
            }
        }
        // And one that moved the refs — commits, branches or tags — has made
        // the history panel stale: what it shows is the graph and its
        // decorations, both read from the refs.
        if matches!(
            action,
            Action::Commit
                | Action::CommitPush
                | Action::Fetch
                | Action::Pull
                | Action::Push
                | Action::Checkout
                | Action::Branch
                | Action::Tag
                | Action::PushTag
                | Action::Merge
                | Action::Integrate
                | Action::Rebase
                | Action::Abort
                | Action::Resume
        ) {
            if let Some(worktree) = worktree.as_deref() {
                self.refresh_repo_history(worktree, cx);
            }
        }
    }

    fn action_failed(
        &mut self,
        worktree: Option<PathBuf>,
        action: Action,
        message: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.finish(&worktree, action);
        // Without this, a status that fails once — repository briefly locked,
        // disk busy — would block every later refresh of that worktree for good.
        if let Some(worktree) = worktree.as_ref() {
            self.pending_status.remove(worktree);
        }
        // An armed integration flag would survive the failure and would offer
        // the cleanup on the next success, whatever it was.
        if action == Action::Integrate {
            self.integrated = None;
        }
        if action == Action::SuggestMessage {
            self.suggesting_message = None;
        }
        // Same reason as the status above: without this, one `ls-files` that
        // fails leaves the explorer waiting for an answer that will never come,
        // and its tree stays empty for the rest of the session.
        if action == Action::Read {
            if let Some(worktree) = worktree.as_deref() {
                self.project_files_failed(worktree);
            }
        }
        // A push the remote rejected because it moved on, or a pull the
        // fast-forward rule refuses, is not an error to read: it is a question
        // — merge or rebase? — and the dialog asks it, the way PhpStorm does.
        // The balloon is skipped, the dialog being the report; cancelling it is
        // answering "leave things as they are". Only for a failure that names
        // its worktree: `UpdateBranch` answers with none, and reconciling would
        // then run on whatever branch the main repository has checked out.
        if matches!(action, Action::Push | Action::Pull | Action::CommitPush)
            && crate::git::repo::diverged(&message)
        {
            if let Some(worktree) = worktree {
                self.offer_reconcile(worktree, action != Action::Pull, window, cx);
                return;
            }
        }
        // A failure always gets a balloon: the bar is overwritten by the next
        // message that goes through it, and the balloon is what one comes back
        // to read.
        self.report(
            action,
            &message,
            crate::ui::notify::Level::Error,
            window,
            cx,
        );
        // A `wt` console stays on failure, with the steps that led to the error.
        self.wt_operation_ended(action, false, window, cx);
    }

    /// The divergence dialog: merge, rebase, or leave it.
    ///
    /// Enter merges — the answer that rewrites nothing — the middle button
    /// rebases, and both send the one command that pulls and, when a push is
    /// what brought us here, pushes again behind it.
    fn offer_reconcile(
        &mut self,
        worktree: PathBuf,
        push: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let entity = cx.entity();
        let body = if push {
            tr!("diverge-body-push")
        } else {
            tr!("diverge-body-pull")
        };
        let rebasing = {
            let (entity, worktree) = (entity.clone(), worktree.clone());
            move |_: &mut Window, cx: &mut App| {
                let worktree = worktree.clone();
                entity.update(cx, |this, cx| this.reconcile(worktree, true, push, cx));
            }
        };
        window.open_dialog(cx, move |dialog, _window, _cx| {
            let (entity, worktree, rebasing) = (entity.clone(), worktree.clone(), rebasing.clone());
            dialog
                .title(tr!("diverge-title"))
                .overlay_closable(false)
                .close_button(false)
                .child(div().text_sm().child(body.clone()))
                .footer(super::dialogs::choose(
                    tr!("diverge-merge"),
                    tr!("diverge-rebase"),
                    rebasing,
                ))
                .on_ok(move |_, _window, cx| {
                    let worktree = worktree.clone();
                    entity.update(cx, |this, cx| this.reconcile(worktree, false, push, cx));
                    true
                })
        });
    }

    /// Sends the reconciliation, under the action the failed gesture had worn:
    /// it is the same button that spins again, and the same key the worker's
    /// answer will take back.
    fn reconcile(&mut self, worktree: PathBuf, rebase: bool, push: bool, cx: &mut Context<Self>) {
        let action = if push { Action::Push } else { Action::Pull };
        let cmd = Cmd::Reconcile {
            worktree: worktree.clone(),
            rebase,
            push,
        };
        self.start(Some(worktree), action, cmd, cx);
    }

    /// An outcome, of either sign, as a balloon.
    ///
    /// The fallback is `success_key()` on both paths — it was so before this
    /// was one function — which reads as the name of what was attempted; the
    /// level is what says how it went.
    fn report(
        &mut self,
        action: Action,
        output: &str,
        level: crate::ui::notify::Level,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.notify(crate::ui::notify::notice(action, output, level), window, cx);
    }

    /// Puts an outcome on screen.
    ///
    /// What it says is `ui::notify`'s decision and is tested there; what is
    /// left here is the gpui-component call.
    pub(super) fn notify(
        &mut self,
        notice: crate::ui::notify::Notice,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.balloon(
            Some(tr!(notice.title)),
            SharedString::from(notice.body),
            notice.level,
            window,
            cx,
        );
    }

    /// One balloon, top right.
    ///
    /// `Root` already re-emits the notification layer at the end of the root
    /// view's render — see "Conventions gpui" — so nothing else is needed to
    /// make it appear. A message with no title of its own carries none: the
    /// balloon then reads as the sentence it is, which is what a copy or a
    /// refusal has to say.
    fn balloon(
        &mut self,
        title: Option<SharedString>,
        message: SharedString,
        level: crate::ui::notify::Level,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let kind = match level {
            crate::ui::notify::Level::Success => {
                gpui_component::notification::NotificationType::Success
            }
            crate::ui::notify::Level::Error => {
                gpui_component::notification::NotificationType::Error
            }
        };
        window.push_notification(
            gpui_component::notification::Notification::new()
                .with_type(kind)
                .when_some(title, |note, title| note.title(title))
                .message(message)
                // A failure stays until it is dismissed: it is the one thing
                // here that is read late, and a balloon that fades on its own
                // is a message one finds gone on coming back to it.
                .autohide(level == crate::ui::notify::Level::Success),
            cx,
        );
    }

    // — Operations in flight ——————————————————————————————————————

    /// Sends a write, and remembers it is under way.
    ///
    /// The key is the pair the worker will answer with, and that is the whole
    /// point: `write_then_refresh` echoes the worktree and the action it was
    /// given, so what is put down here is exactly what `finish` will find.
    pub(super) fn start(
        &mut self,
        worktree: Option<PathBuf>,
        action: Action,
        cmd: Cmd,
        cx: &mut Context<Self>,
    ) {
        self.flight.start(worktree, action);
        self.git.send(cmd);
        cx.notify();
    }

    /// Forgets everything under way.
    ///
    /// For the two moments when what was in flight will never answer: the server
    /// dying, and a new one taking its place.
    fn clear_running(&mut self) {
        self.flight.clear();
    }

    fn finish(&mut self, worktree: &Option<PathBuf>, action: Action) {
        self.flight.finish(worktree, action);
    }

    /// Is this operation under way? What a button reads to turn.
    pub(super) fn is_running(&self, worktree: Option<&Path>, action: Action) -> bool {
        self.flight.is_running(worktree, action)
    }

    /// The same, for the worktree being looked at.
    pub(super) fn active_running(&self, action: Action) -> bool {
        self.is_running(self.active.as_deref(), action)
    }

    fn fetched(&mut self, main: PathBuf) {
        // The fetch has moved the remote references: the displayed worktree's
        // ahead and behind counts no longer hold, and they are what the buttons
        // carry. The status of the displayed worktree only — the background
        // sweep takes care of the others, and re-reading ten statuses every ten
        // minutes would cost more than it shows.
        if let Some(worktree) = self.active.clone() {
            if self.main_of(&worktree).as_deref() == Some(main.as_path()) {
                self.request_status(worktree);
            }
        }
        self.git.send(Cmd::LoadBranches { main });
    }

    fn commit_message_arrived(
        &mut self,
        worktree: PathBuf,
        message: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        // The message arrives several seconds after the request: it only lands
        // in the field of the worktree that asked for it. The flag, for its
        // part, falls whatever happens — otherwise the button would keep
        // spinning on a worktree one has left.
        if self.suggesting_message.as_deref() == Some(worktree.as_path()) {
            self.suggesting_message = None;
        }
        if self.active.as_deref() == Some(worktree.as_path()) {
            self.commit_input.update(cx, |input, cx| {
                input.set_value(message, window, cx);
            });
        } else {
            // Parked in its worktree's draft rather than dropped: it will be
            // in the field when one comes back.
            if let Some(state) = self.review.get_mut(&worktree) {
                state.commit_draft = message;
            }
            self.announce(tr!("commit-suggest-elsewhere"), cx);
        }
    }

    // — Vault —————————————————————————————————————————————————————

    /// The notes folder has answered: it is the source, and what was held in
    /// memory was only a wait.
    fn notes_read(
        &mut self,
        worktree: PathBuf,
        files: Vec<(String, String)>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let mut notes = Vec::new();
        let mut reviewed: Vec<crate::ui::vault::Reviewed> = Vec::new();
        let mut todo = None;
        let mut journal = String::new();
        let on_disk = !files.is_empty();
        for (name, text) in files {
            if name == crate::ui::vault::INDEX || name == crate::ui::vault::LEGACY_INDEX {
                reviewed = crate::ui::vault::parse_index(&text);
            } else if name == crate::ui::vault::TODO {
                todo = Some(crate::ui::vault::parse_todo(&text));
            } else if name == crate::ui::vault::NOTES {
                journal = text;
            } else if let Some(note) = crate::ui::vault::parse_note(&text) {
                notes.push(note);
            }
        }
        notes.sort_by_key(|note| note.id);
        let migrating = self.migrate_legacy_notes(&worktree, &mut notes, cx);
        self.ensure_review(&worktree, cx);
        if let Some(state) = self.review.get_mut(&worktree) {
            // An id already taken by a note from the folder would make two notes
            // with the same number, and the prompt would name one for the other.
            let highest = notes.iter().map(|note| note.id).max().unwrap_or(0);
            state.next_note = state.next_note.max(highest + 1);
            state.notes = Rc::new(notes);
            // Sorted here, and kept sorted by `set_reviewed`: the panel showed
            // them in order, and sorting a copy on every frame is what that
            // cost.
            reviewed.sort_by(|a, b| a.path.cmp(&b.path));
            state.reviewed = reviewed;
            state.rows_changed();
            state.todo = todo;
            state.journal = journal;
            state.notes_loaded = true;
            state.notes_on_disk = on_disk;
        }
        self.refresh_note_marks(&worktree);
        self.sync_journal_input(&worktree, window, cx);
        if migrating {
            self.persist_notes(&worktree, cx);
        }
    }

    /// Pours the notes of an earlier `state.json` into the folder's, and empties
    /// the store. Answers whether there was anything to pour.
    ///
    /// Picking up the old store goes down the same path as a fresh install, like
    /// `migrate_agents`: an earlier state file carries its notes, and they have
    /// nobody else to write them into the folder. Once only — the store is
    /// emptied straight after, and the ids already taken are respected.
    fn migrate_legacy_notes(
        &mut self,
        worktree: &Path,
        notes: &mut Vec<crate::ui::notes::Note>,
        cx: &mut Context<Self>,
    ) -> bool {
        let legacy = Store::global(cx)
            .worktree(worktree)
            .map(|saved| saved.notes.clone())
            .unwrap_or_default();
        if legacy.is_empty() {
            return false;
        }
        let known: std::collections::HashSet<u64> = notes.iter().map(|note| note.id).collect();
        notes.extend(legacy.into_iter().filter(|note| !known.contains(&note.id)));
        notes.sort_by_key(|note| note.id);
        if let Some(main) = self.main_of(worktree) {
            Store::update_global(cx, |store| {
                store.worktree_mut(worktree, &main).notes = Vec::new();
            });
        }
        true
    }

    // — Transport —————————————————————————————————————————————————

    fn server_hello(
        &mut self,
        build: String,
        cwd: PathBuf,
        running_under_wsl: bool,
        shells: Vec<String>,
    ) {
        log::info!("remote server ready (build {build})");
        self.server_state = super::server::ServerState::Up;
        // The commands sent before the handle was live were dropped, and a
        // fresh server has nothing of ours in flight: what is left in the set is
        // waiting for an answer that will not come.
        self.clear_running();
        self.server_wsl = running_under_wsl;
        // It is the server that will launch the shells: it is its own the form
        // has to offer, not this machine's.
        crate::ui::settings::set_server_shells(shells);
        // The form's copy names this machine's shells, which are not the ones
        // that will be launched.
        self.forget_settings_environment();
        // Remote mode's "launched from its project": it is the server's
        // directory that says so, not ours — and it was **named** exactly
        // when a command-line argument put it there (`server::connect_wsl`).
        self.launch_chosen = self.launch_arg.is_some();
        self.launch_dir = Some(cwd.clone());
        self.git.send(Cmd::OpenIfRepo(cwd));
        // And what was handed over while there was nobody to hear it. **After**
        // the line above, whose `launch_dir` it must displace: a folder somebody
        // clicked outranks the directory the server happens to have started in.
        if let Some(folder) = self.pending_handoff.take() {
            self.open_handed(folder);
        }
    }

    // — Selection ————————————————————————————————————————————————

    /// Asks for a status, unless one is already in flight for this worktree.
    pub(super) fn request_status(&mut self, worktree: PathBuf) {
        if self.pending_status.insert(worktree.clone()) {
            self.git.send(Cmd::RefreshStatus { worktree });
        }
    }

    pub(super) fn select_worktree(
        &mut self,
        path: PathBuf,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.select_worktree_ranked(path, crate::ui::session::SELECTION_CHOSEN, window, cx);
    }

    /// The same, saying how firm the choice is.
    ///
    /// Every gesture is `SELECTION_CHOSEN`; the weaker ranks are for the
    /// selections Claudhub makes for itself while the repositories answer —
    /// see `session::pick_worktree`.
    pub(super) fn select_worktree_ranked(
        &mut self,
        path: PathBuf,
        rank: u8,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.active.as_deref() == Some(path.as_path()) {
            return;
        }
        // The vault is watched like the worktree, and for the same reason: the
        // work happens elsewhere in it. An agent ticking a task in `TODO.md` or
        // answering in a note has to be seen at once, otherwise one would have
        // to change worktree and come back.
        let previous_vault = self
            .active
            .as_deref()
            .and_then(|previous| self.notes_dir(previous, cx));
        let vault = self.notes_dir(&path, cx);
        if let Some(previous) = self.active.clone() {
            self.git.send(Cmd::Unwatch { worktree: previous });
        }
        if let Some(previous) = previous_vault {
            self.git.send(Cmd::UnwatchDir { dir: previous });
        }
        self.git.send(Cmd::Watch {
            worktree: path.clone(),
        });
        if let Some(vault) = vault {
            self.git.send(Cmd::WatchDir { dir: vault });
        }
        // The commit message belongs to its worktree: the draft written for
        // one project is parked in its state and this worktree's is put back —
        // the input is unique, and the text would otherwise follow the window.
        if let Some(previous) = self.active.clone() {
            let draft = self.commit_input.read(cx).value().to_string();
            if let Some(state) = self.review.get_mut(&previous) {
                state.commit_draft = draft;
            }
        }
        self.active = Some(path.clone());
        self.ensure_review(&path, cx);
        let draft = self
            .review
            .get(&path)
            .map(|state| state.commit_draft.clone())
            .unwrap_or_default();
        if self.commit_input.read(cx).value() != draft.as_str() {
            self.commit_input
                .update(cx, |input, cx| input.set_value(draft, window, cx));
        }
        // What this checkout's justfile offers, for the run button. Asked here
        // and not while drawing the bar: it is a subprocess.
        self.ensure_just(&path);
        // And its Pest suite, for the tests panel — here and not at a first
        // paint, because the tab only exists once the answer says there is one.
        self.ensure_pest(&path, cx);
        // A plugin's panel speaks about the worktree the window shows, like
        // every other panel: changing it is starting over, not refreshing.
        self.plugins_follow_worktree(cx);
        // And the editing screen goes back to this worktree's file: one had
        // gone off to edit a plugin's script, which belongs to no worktree, and
        // leaving it there would show a file the rest of the window denies.
        self.editing_root = None;
        // The free note follows the displayed worktree: the input is unique, and
        // keeping the previous one's text would write it here.
        self.sync_journal_input(&path, window, cx);
        // And so does the search, field included, for the same reason.
        self.sync_search(&path, window, cx);
        self.request_status(path);
        // Every worktree has its base: the selector has to show this one, not
        // the one of the worktree just left.
        self.refresh_base_choices(window, cx);
        // The database scope names the checkout (`db::scope::Vars`), so the
        // rows it keeps and the count of what it hides both belong to the
        // worktree being shown — and both are read from the last rebuild.
        self.db_rebuild(cx);
        self.selection_rank = rank;
        self.ensure_session_terminal(window, cx);
        // Where the work stood here: the screen, the console, and — on the
        // first arrival — the files. **Before** the write below, which would
        // otherwise file the previous project's tabs under this worktree.
        self.settle_place(window, cx);
        self.persist_session(cx);
        cx.notify();
    }

    /// The session's first terminal, on the worktree one **ends up** on.
    ///
    /// It is what one talks to while reviewing, and the first gesture of every
    /// session was `Ctrl+T` — a shell that has to be asked for is a shell one
    /// asks for every time. Only the first: opening one per worktree visited
    /// would leave a dozen shells behind an afternoon's browsing, and the
    /// terminals one has closed must stay closed.
    ///
    /// **Never on a stopgap selection**, and that is the whole of this
    /// function. While the repositories are still being enumerated the window
    /// shows the first checkout of whichever answered first
    /// (`SELECTION_FALLBACK`); opening the terminal there put a shell in a
    /// project the session never asked for — and, the flag being set, denied
    /// the remembered worktree the one that was meant for it. So a rank below
    /// the session's waits for the repositories still owed an answer; when none
    /// is left, the stopgap **is** the answer and it opens there.
    fn ensure_session_terminal(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.terminal_started {
            return;
        }
        if !crate::ui::session::session_terminal_due(self.selection_rank, self.opening_repos) {
            return;
        }
        let Some(worktree) = self.active.clone() else {
            return;
        };
        self.terminal_started = true;
        self.ensure_terminal(&worktree, window, cx);
    }

    /// Opens a file in the diff view.
    ///
    /// The range comes from the panel the click starts in: "Changes" and "Branch
    /// review" show the same file two ways, and it is the list clicked that
    /// decides which.
    pub(super) fn open_file(
        &mut self,
        worktree: PathBuf,
        path: PathBuf,
        range: DiffRange,
        cx: &mut Context<Self>,
    ) {
        let Some(state) = self.review.get_mut(&worktree) else {
            return;
        };
        state.selected = Some(path.clone());
        // **And the diff comes back**, the tab having a cross now: closing it
        // takes the panel out of the tree, and a click in the list that showed
        // nothing would make the cross a gesture one cannot undo. Deferred
        // because this has no window — see `reveal_panel_later`.
        self.pending_reveal = Some(super::panels::DiffPanel::NAME);
        let Some(state) = self.review.get_mut(&worktree) else {
            return;
        };
        // The previous diff is cleared at once: keeping another file's for the
        // length of the read would give the impression that the click did
        // nothing, and then that the content changes by itself.
        state.diff = None;
        state.unstaged = None;
        state.diff_selection = None;
        // A jump armed by an arrow does not survive another gesture: opening a
        // file with the mouse has to open it at the top.
        state.pending_jump = None;
        state.range = range.clone();
        let untracked = state
            .status
            .files
            .iter()
            .any(|f| f.path == path && f.is_untracked());
        let partial = matches!(range, DiffRange::Working) && partially_staged(&state.status, &path);
        // Opening a file is leaving the merge: the centre shows one or the
        // other, and what one has just clicked is what one wants to see. After
        // the state above and not before, so that one lookup serves for all of
        // it — nothing between the two reads either.
        self.merging = None;
        self.git.send(Cmd::LoadFileDiff {
            worktree: worktree.clone(),
            range: range.clone(),
            path: path.clone(),
            context: Settings::global(cx).context_lines(),
            untracked,
        });
        // The remainder: only a partially staged file has an "other part",
        // and only the working range commits.
        if partial {
            self.git.send(Cmd::LoadUnstagedDiff {
                worktree,
                path: path.clone(),
                context: Settings::global(cx).context_lines(),
            });
        }
        // The list follows the open file: an arrow that changes file would
        // otherwise leave it out of view, and one would review without knowing
        // where one is. The scrolling is non-strict — an already-visible file
        // does not make the list jump under one's eyes.
        self.reveal_file(&range, &path, cx);
        // And on the home screen, the diff is one tab of a group that may be
        // showing the console: clicking a file has to bring it forward, or the
        // click reads as having done nothing. Deferred, this being the one
        // opening gesture that never had a window.
        self.reveal_panel_later(crate::ui::panels::DiffPanel::NAME);
        cx.notify();
    }

    /// A range's list scroll handle, created on demand.
    ///
    /// Never rebuilt: a fresh handle per frame would put the list back at the
    /// top on every frame.
    pub(super) fn file_scroll(&mut self, range: &DiffRange) -> gpui::UniformListScrollHandle {
        self.file_scroll.entry(range.clone()).or_default().clone()
    }

    /// A non-virtualised panel's scroll handle.
    pub(super) fn scroll_of(&mut self, key: &'static str) -> gpui::ScrollHandle {
        self.scrolls.entry(key).or_default().clone()
    }

    /// Asks for the displayed file's diff again.
    ///
    /// What it contains depends on settings that change mid-review — the
    /// context, and "the whole file": it then has to be re-read, git alone
    /// knowing what the elided lines contained.
    pub(super) fn reload_diff(&mut self, cx: &mut Context<Self>) {
        let Some(worktree) = self.active.clone() else {
            return;
        };
        let Some(state) = self.review.get(&worktree) else {
            return;
        };
        let (Some(path), range) = (state.selected.clone(), state.range.clone()) else {
            return;
        };
        self.open_file(worktree, path, range, cx);
    }

    /// Asks for a range's file list, if it is missing.
    ///
    /// Called when the panel showing it renders: it is what knows what it shows,
    /// and loading it in advance would cost a command for a tab nobody will open.
    pub(super) fn ensure_files(&mut self, range: DiffRange, cx: &mut Context<Self>) {
        let Some(worktree) = self.active.clone() else {
            return;
        };
        let Some(state) = self.review.get_mut(&worktree) else {
            return;
        };
        if state.files.contains_key(&range) || !state.pending_files.insert(range.clone()) {
            return;
        }
        self.git.send(Cmd::LoadDiffFiles { worktree, range });
        cx.notify();
    }

    pub(super) fn refresh_active(&mut self, cx: &mut Context<Self>) {
        let Some(worktree) = self.active.clone() else {
            return;
        };
        if let Some(main) = self.main_of(&worktree) {
            self.git.send(Cmd::RefreshRepo { main: main.clone() });
            self.git.send(Cmd::LoadBranches { main });
        }
        self.request_status(worktree);
        cx.notify();
    }

    // — Access to the state ——————————————————————————————————————

    /// Fills the base selector with the current repository's branches.
    ///
    /// The locals first, then the remotes: it is the order one looks for them
    /// in, and `branch::list` already returns them that way.
    fn refresh_base_choices(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(worktree) = self.active.clone() else {
            return;
        };
        let Some(repo) = self.repo_of(&worktree) else {
            return;
        };
        let choices: Vec<BaseChoice> = repo
            .branches
            .iter()
            .map(|branch| BaseChoice::of(branch, &worktree))
            .collect();
        let current = self
            .review
            .get(&worktree)
            .and_then(|state| state.base.clone())
            .map(SharedString::from);

        self.base_select.update(cx, |select, cx| {
            select.set_items(SearchableVec::new(choices), window, cx);
            if let Some(current) = current {
                select.set_selected_value(&current, window, cx);
            }
        });
    }

    /// Changes the current worktree's comparison base.
    ///
    /// The choice belongs to the worktree: comparing one agent worktree against
    /// `dev` and another against the branch it started from is the normal case,
    /// not the exception.
    pub(super) fn set_base(&mut self, base: String, cx: &mut Context<Self>) {
        let Some(worktree) = self.active.clone() else {
            return;
        };
        let Some(state) = self.review.get_mut(&worktree) else {
            return;
        };
        if state.base.as_deref() == Some(base.as_str()) {
            return;
        }
        state.base = Some(base.clone());
        // The branch lists were about the old base: forgetting them makes them
        // be asked for again on the panel's next render.
        state
            .files
            .retain(|range, _| !matches!(range, DiffRange::Branch { .. }));
        state
            .pending_files
            .retain(|range| !matches!(range, DiffRange::Branch { .. }));
        // The branch history depends on the same base.
        state.history = None;
        self.persist_review(&worktree, cx);
        cx.notify();
    }

    /// Creates a worktree's state, putting back into it what the store had kept.
    ///
    /// The base read back **wins** over the one git guesses: it is a choice of
    /// the user's, and guessing it again on every launch was exactly the gap the
    /// store fills. The folder collapses come from the same place, for the same
    /// reason.
    fn ensure_review(&mut self, worktree: &Path, cx: &App) {
        if self.review.contains_key(worktree) {
            return;
        }
        let saved = Store::global(cx).worktree(worktree).cloned();
        let mut state = ReviewState::default();
        if let Some(saved) = saved {
            state.base = saved.base;
            state.lsp = saved.lsp;
            state.collapsed = saved.collapsed.into_iter().collect();
            // A file written before this field existed carries zero, and a note
            // with a null id would be confused with the absence of a note.
            state.next_note = saved.next_note.max(1);
        }
        self.review.insert(worktree.to_path_buf(), state);
        // The notes live in a folder, and a folder is read in a worker: a vault
        // on a slow disk would freeze the window for the length of a `read_dir`.
        if let Some(dir) = self.notes_dir(worktree, cx) {
            self.git.send(Cmd::ReadNotes {
                worktree: worktree.to_path_buf(),
                dir,
            });
        }
    }

    /// A worktree's notes folder, under the settings' root.
    pub(super) fn notes_dir(&self, worktree: &Path, cx: &App) -> Option<PathBuf> {
        let main = self.main_of(worktree)?;
        let root = Settings::global(cx).notes_root()?;
        // The vault is read and written by the workers, and the agent receives it
        // in its environment: it therefore has to be a path **of the server's**,
        // never one of the world the setting comes from. On Windows,
        // `<config>/notes` becomes a path in the distribution; a vault already
        // pointed at `/home/…` passes through unchanged.
        let root = if cfg!(windows) {
            crate::wslpath::for_server(&root)?
        } else {
            root
        };
        Some(crate::ui::vault::dir_for(&root, &main, worktree))
    }

    /// Writes into the store what the current worktree has that persists.
    ///
    /// A single write site rather than one per field: the three fit in a few
    /// kilobytes, and keeping them up to date separately would multiply the
    /// chances of forgetting one.
    pub(super) fn persist_review(&mut self, worktree: &Path, cx: &mut App) {
        let Some(main) = self.main_of(worktree) else {
            return;
        };
        let Some(state) = self.review.get(worktree) else {
            return;
        };
        // Sorted: a `HashSet` serialised in a different order on every write
        // would make a file that changes with nothing having changed.
        let mut collapsed: Vec<PathBuf> = state.collapsed.iter().cloned().collect();
        collapsed.sort();
        let (base, next_note, lsp) = (state.base.clone(), state.next_note, state.lsp);
        Store::update_global(cx, |store| {
            let saved = store.worktree_mut(worktree, &main);
            saved.base = base;
            saved.collapsed = collapsed;
            saved.next_note = next_note;
            saved.lsp = lsp;
        });
        self.persist_notes(worktree, cx);
    }

    /// Brings the worktree's notes folder into line with what is held in memory.
    ///
    /// Without a timer, unlike the settings and the store: what is sent is an
    /// order to a worker, not a write, and a note is confirmed in a dialog where
    /// a settings field emits a value per keystroke. The worker, for its part,
    /// does not rewrite a file whose content has not moved.
    pub(super) fn persist_notes(&mut self, worktree: &Path, cx: &App) {
        let Some(dir) = self.notes_dir(worktree, cx) else {
            return;
        };
        let Some(state) = self.review.get_mut(worktree) else {
            return;
        };
        // Nothing while the folder has not answered: writing an empty list would
        // erase notes that have not been read yet.
        if !state.notes_loaded {
            return;
        }
        let something = !state.notes.is_empty() || !state.reviewed.is_empty();
        if !something && !state.notes_on_disk {
            return;
        }
        state.notes_on_disk |= something;
        let mut files: Vec<(String, String)> = state
            .notes
            .iter()
            .map(|note| {
                (
                    crate::ui::vault::note_file(note),
                    crate::ui::vault::render_note(note),
                )
            })
            .collect();
        files.push((
            crate::ui::vault::INDEX.to_string(),
            crate::ui::vault::render_index(worktree, &state.reviewed),
        ));
        self.git.send(Cmd::WriteNotes {
            worktree: worktree.to_path_buf(),
            dir,
            files,
        });
    }

    /// (Re)installs the watch on the displayed worktree's vault.
    ///
    /// No effect if it is already watched, and no effect if the folder does not
    /// exist yet — the order simply has to be sent again after creating it.
    fn watch_vault(&mut self, worktree: &Path, cx: &App) {
        if self.active.as_deref() != Some(worktree) {
            return;
        }
        let Some(vault) = self.notes_dir(worktree, cx) else {
            return;
        };
        self.git.send(Cmd::WatchDir { dir: vault });
    }

    /// Is the two-column view wrapped?
    ///
    /// The question decides **which list is displayed**, and therefore which
    /// handle carries the scrolling: the two are never painted at the same time,
    /// and aiming at the wrong one would scroll a list that is not there.
    pub(super) fn diff_wrapped(&self, cx: &App) -> bool {
        let settings = Settings::global(cx);
        settings.diff_split && settings.diff_wrap
    }

    /// The handle gpui really animates for the displayed diff.
    pub(super) fn diff_base_handle(&self, cx: &App) -> gpui::ScrollHandle {
        use crate::ui::scroll::Scrollable;
        if self.diff_wrapped(cx) {
            self.diff_wrap_scroll.base()
        } else {
            self.diff_scroll.base()
        }
    }

    /// Brings a diff entry into view, without moving if it is already there.
    ///
    /// That is what an arrow moving one line at a time wants: a line already on
    /// screen must not make the view jump under the eye.
    pub(super) fn reveal_diff_row(&self, row: usize, strategy: gpui::ScrollStrategy, cx: &App) {
        if self.diff_wrapped(cx) {
            self.diff_wrap_scroll.scroll_to_item(row, strategy);
        } else {
            self.diff_scroll.scroll_to_item(row, strategy);
        }
    }

    /// Brings a diff entry into view, visible or not.
    ///
    /// Going from one hunk to the next is not going one line further: gpui's
    /// `scroll_to_item` does nothing when the target is already on screen, so
    /// the key moved the selection by four hundred lines and the view not at
    /// all — which reads as a key that does nothing, the more so as nothing
    /// said which hunk one was on. A hunk step therefore **always** moves.
    ///
    /// The wrapped list has no strict variant (`scroll_strict` is hard-coded in
    /// gpui-component) — except for `Center`, which it applies unconditionally
    /// and which therefore needs nothing. `Top` is the one that has to be
    /// forced, and its non-strict path is **not** a top at all: an entry below
    /// the viewport is brought to its **bottom** edge, which is where the hunk
    /// header ended up — one line high, at the very bottom of the screen. The
    /// list is therefore first scrolled **past the end**, so the target lies
    /// above the viewport and the only branch left is the one that pins an entry
    /// to the top. The overshoot is clamped in the same prepaint, before
    /// anything is painted, so nothing flashes.
    pub(super) fn reveal_diff_row_strict(
        &self,
        row: usize,
        strategy: gpui::ScrollStrategy,
        cx: &App,
    ) {
        use crate::ui::scroll::Scrollable;
        use gpui_component::scroll::ScrollbarHandle;
        if self.diff_wrapped(cx) {
            if !matches!(strategy, gpui::ScrollStrategy::Center) {
                let base = self.diff_wrap_scroll.base();
                let offset = base.offset();
                base.set_offset(gpui::point(offset.x, -base.content_size().height));
            }
            self.diff_wrap_scroll.scroll_to_item(row, strategy);
        } else {
            self.diff_scroll.scroll_to_item_strict(row, strategy);
        }
    }

    /// Puts a hunk where it is read from, and moves the view whatever happens.
    ///
    /// **In the middle of the view**, which is where the eye already is and
    /// which shows what comes before the change as well as what follows it —
    /// reading a hunk is reading it in its context. The exception is a hunk
    /// **taller than the view**: centring it would push its first lines off the
    /// top, where they cannot be recovered by reading downwards. Such a hunk
    /// goes to the top, and is read like a page.
    pub(super) fn reveal_diff_hunk(&self, row: usize, cx: &App) {
        let strategy = if self.hunk_fits_the_view(row, cx) {
            gpui::ScrollStrategy::Center
        } else {
            gpui::ScrollStrategy::Top
        };
        self.reveal_diff_row_strict(row, strategy, cx);
    }

    /// Marks files reviewed, or hands their review back.
    ///
    /// The volume is recorded at the moment of the click: it is what expires the
    /// tick when an agent rewrites the file.
    pub(super) fn set_reviewed(
        &mut self,
        worktree: PathBuf,
        range: DiffRange,
        paths: Vec<PathBuf>,
        reviewed: bool,
        cx: &mut Context<Self>,
    ) {
        let Some(state) = self.review.get_mut(&worktree) else {
            return;
        };
        let volumes: HashMap<&PathBuf, (usize, usize)> = state
            .files
            .get(&range)
            .map(Vec::as_slice)
            .unwrap_or(&[])
            .iter()
            .map(|file| (&file.path, (file.added, file.removed)))
            .collect();
        for path in &paths {
            state
                .reviewed
                .retain(|item| item.range != range || item.path != *path);
            if reviewed {
                let (added, removed) = volumes.get(path).copied().unwrap_or((0, 0));
                state.reviewed.push(crate::ui::vault::Reviewed {
                    range: range.clone(),
                    path: path.clone(),
                    added,
                    removed,
                });
            }
        }
        // Kept sorted: it is the order the notes panel lists them in.
        state.reviewed.sort_by(|a, b| a.path.cmp(&b.path));
        state.rows_changed();
        self.persist_review(&worktree, cx);
        cx.notify();
    }

    /// Forgets what was kept of worktrees git no longer lists.
    fn forget_missing_worktrees(&mut self, main: &Path, cx: &mut App) {
        let alive = self.repos.worktree_paths(main);
        if alive.is_empty() {
            return;
        }
        let main = main.to_path_buf();
        Store::update_global(cx, |store| store.forget_missing(&main, &alive));
    }

    /// Closes the diff: the file it was showing stops being the picked one.
    ///
    /// The tab is conditional, so clearing the selection is the whole of
    /// closing it — and picking another file brings it back, which is what
    /// makes the cross a gesture one can undo without looking for how.
    pub(super) fn close_diff(&mut self, cx: &mut Context<Self>) {
        self.merging = None;
        if let Some(state) = self.active_review_mut() {
            state.selected = None;
            state.diff = None;
            state.unstaged = None;
            state.diff_selection = None;
        }
        cx.notify();
    }

    /// Is there a diff to read — a file picked in one of the two lists, or a
    /// merge taking that place.
    pub(super) fn diff_on_screen(&self) -> bool {
        self.merging.is_some() || self.active_review().is_some_and(|s| s.selected.is_some())
    }

    /// Is there a file under the search's cursor to show beside the hits.
    pub(super) fn search_preview_open(&self) -> bool {
        self.search.preview.is_some()
    }

    /// The cross on the preview's tab: done reading this hit.
    ///
    /// The preview comes back with the next hit one moves on to, which is what
    /// makes closing it cheap — it is "give me the room", not "forget where I
    /// was".
    pub(super) fn close_search_preview(&mut self, cx: &mut Context<Self>) {
        self.search.preview = None;
        cx.notify();
    }

    pub(super) fn active_review(&self) -> Option<&ReviewState> {
        self.active.as_ref().and_then(|p| self.review.get(p))
    }

    pub(super) fn active_review_mut(&mut self) -> Option<&mut ReviewState> {
        let path = self.active.clone()?;
        self.review.get_mut(&path)
    }

    /// Says something in the status bar. For the gestures that succeed without
    /// changing anything on screen — copying, for instance — it is the only
    /// acknowledgement one can give.
    pub(super) fn announce(&mut self, text: SharedString, cx: &mut Context<Self>) {
        self.pending_notes
            .push((text, crate::ui::notify::Level::Success));
        cx.notify();
    }

    /// The same, for something that did not work.
    ///
    /// It differs by one thing and it is the one that matters: a failure does
    /// **not** fade. It is the message read late — one comes back to the window
    /// to find out why nothing happened — and a balloon that goes on its own is
    /// a message found gone.
    pub(super) fn announce_error(&mut self, text: SharedString, cx: &mut Context<Self>) {
        self.pending_notes
            .push((text, crate::ui::notify::Level::Error));
        cx.notify();
    }

    /// Asks for a view to come forward at the next frame, from a gesture with
    /// no window in hand.
    pub(super) fn reveal_panel_later(&mut self, name: &'static str) {
        self.pending_reveal = Some(name);
    }

    /// Brings forward what a windowless gesture asked for.
    ///
    /// Beside `flush_notes` and for its reason: the root is not leased at the
    /// top of its own render, so the dock may be edited from here and nowhere
    /// inside a closure the root calls back.
    fn flush_reveal(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(name) = self.pending_reveal.take() else {
            return;
        };
        // **Quietly**: this runs between two frames, and a tab that has just
        // grown content is not a reason to take the caret out of whatever is
        // being typed in.
        self.reveal_quietly(name, window, cx);
    }

    /// Turns what was said between two frames into balloons.
    ///
    /// Called at the top of the root's render: `Root` is not leased then — its
    /// own render returned before ours was asked for, which is the rule the
    /// dock's panels live by — so pushing into it is safe here and nowhere
    /// inside a closure the root itself calls back.
    fn flush_notes(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        for (text, level) in std::mem::take(&mut self.pending_notes) {
            self.balloon(None, text, level, window, cx);
        }
    }

    pub(super) fn main_of(&self, worktree: &Path) -> Option<PathBuf> {
        self.repos.main_of(worktree)
    }

    pub(super) fn repo_of(&self, worktree: &Path) -> Option<&crate::ui::repos::RepoState> {
        self.repos.repo_of(worktree)
    }

    pub(super) fn active_worktree(&self) -> Option<&Worktree> {
        self.repos.worktree(self.active.as_deref()?)
    }

    pub(super) fn worktree_exists(&self, path: &Path) -> bool {
        self.repos.contains_worktree(path)
    }

    pub(super) fn first_worktree(&self) -> Option<PathBuf> {
        self.repos.first_worktree()
    }

    fn default_base_for(&self, worktree: &Path) -> Option<String> {
        self.repos.default_base_for(worktree)
    }

    // — Rendering ——————————————————————————————————————————————————

    /// The operations under way, in the status bar.
    ///
    /// Named and not merely counted: "3 operations" says nothing, where
    /// "Pushing…" says which of the gestures just made is the one still going.
    /// Sorted, because a `HashSet` iterates in a different order every frame and
    /// the words would dance.
    fn render_running(&self, cx: &mut Context<Self>) -> Option<impl IntoElement> {
        if self.flight.is_empty() {
            return None;
        }
        let names: Vec<SharedString> = self
            .flight
            .announcements()
            .into_iter()
            .map(|key| tr!(key))
            .collect();
        Some(
            h_flex()
                .gap_1()
                .items_center()
                .text_color(cx.theme().warning)
                .child(icon("loader-circle").xsmall())
                .child(names.join(" · "))
                .child(Divider::vertical().h(px(12.))),
        )
    }

    /// The active checkout's agent — the badge, then a rule, or nothing at
    /// all: a bar that says "no agent" all day long says nothing.
    fn render_agent_indicator(&self, cx: &gpui::App) -> Option<impl IntoElement> {
        let agent = self.agents.get(self.active.as_deref()?)?;
        Some(
            gpui_component::h_flex()
                .flex_none()
                .gap_2()
                .items_center()
                .child(crate::ui::topbar::agent_badge(agent, cx))
                .child(Divider::vertical().h(px(12.))),
        )
    }

    fn render_status_bar(&mut self, cx: &mut Context<Self>) -> impl IntoElement {
        // On a Windows drive mounted by WSL, watching reports nothing: saying so
        // is the only way to tell "nothing has changed" from "Claudhub no longer
        // sees anything". The computation is redone on every frame because it
        // costs only a comparison of path components, membership of WSL being
        // recorded once and for all.
        let unwatched = self.active.as_deref().is_some_and(|path| {
            crate::runtime::watch::on_windows_filesystem(path)
                // Remotely, it is the **server's** machine that knows whether it
                // is under WSL: its handshake said so, and the mount is
                // recognised from the path alone.
                || (self.server_wsl && crate::runtime::watch::is_windows_mount(path))
        });
        let muted = cx.theme().muted_foreground;

        h_flex()
            // A toolbar and not a list row: it now carries buttons, and
            // twenty-two pixels would make them spill over the bottom of the
            // window.
            .h(super::theme::toolbar_height(cx))
            .w_full()
            .px_2()
            .items_center()
            .gap_2()
            .border_t_1()
            .border_color(cx.theme().border)
            .bg(cx.theme().title_bar)
            .text_xs()
            .text_color(muted)
            // The bottom edge's tool windows open the bar, where the screen
            // picker used to be — see `render_bottom_tools` for why they are
            // here rather than in a strip of their own.
            .child(self.render_bottom_tools(cx))
            // The active checkout's agent: the dot
            // says a Claude works (or waits) in what the window is looking at,
            // read out of the corner of the eye. The rest of the fleet stays
            // in the worktree picker and on the pins.
            .children(self.render_agent_indicator(cx))
            // What is running right now, before what has finished: a menu entry
            // closes on the click, so an integration, a rebase or a `wt rm` has
            // no button of its own to turn, and this line is the only thing
            // between the gesture and the toast that comes minutes later.
            .children(self.render_running(cx))
            // A half-finished operation comes before everything else: while it
            // lasts, what the review shows is not what one thinks.
            .children(self.render_pending_bar(cx))
            // The server's state comes before the ordinary warnings: while it is
            // missing, nothing the window shows moves any more, and every gesture
            // goes into the void.
            .children(self.render_server_status(cx))
            .when(unwatched, |el| {
                el.child(
                    h_flex()
                        .gap_1()
                        .text_color(cx.theme().warning)
                        .child(icon("triangle-alert").xsmall())
                        .child(tr!("watch-windows-filesystem")),
                )
                .child(Divider::vertical().h(px(12.)))
            })
            // A newer release is out: the one message this bar still carries
            // that asks for a click — the link opens the release's page,
            // where the AppImage and the installer are.
            .when_some(self.update.clone(), |el, (version, url)| {
                el.child(
                    h_flex()
                        .id("release-update")
                        .gap_1()
                        .items_center()
                        .text_color(cx.theme().info)
                        .cursor_pointer()
                        .hover(|s| s.underline())
                        .child(icon("download").xsmall())
                        .child(tr!("update-available", { version: version }))
                        .on_click(move |_, _window, cx| {
                            cx.open_url(&url);
                        }),
                )
                .child(Divider::vertical().h(px(12.)))
            })
            // What the bar used to say has gone to the balloons, top right:
            // the last message of a window is read where one is looking, and
            // the bottom edge is where one is not. What is left here is the
            // space that pushes the terminals to the other end.
            .child(div().flex_1().min_w_0())
            // The terminals close the bar, at the bottom right — the corner of
            // the window they open on. They were at the top, at the other end of
            // the screen from the panel they show and beside a menu that speaks
            // of the application rather than of the work.
            //
            // The toggle carries the **same `+`** as the last terminal tab's,
            // and after it as the `+` comes after the tabs: with the terminals
            // hidden there is no tab bar left to open one from, and showing them
            // first only to ask is a gesture too many.
            .child(
                h_flex()
                    .flex_shrink_0()
                    .gap_1()
                    .child(
                        Button::new("terminal")
                            .xsmall()
                            .compact()
                            .icon(icon("square-terminal"))
                            // The name is written and not hovered, as at both ends of
                            // this bar: an icon alone is a rebus, and a tooltip is a
                            // name one has to ask for.
                            .label(tr!("panel-terminal"))
                            // Solid when they are showing, outline when they are not —
                            // the screen picker's polarity, at the other end of the same
                            // line: `ghost` plus `selected` is a background a few
                            // percent away from the bar's own, invisible on half the
                            // themes.
                            .map(|button| {
                                if self.terminals_on_screen(cx) {
                                    button.primary()
                                } else {
                                    button.outline()
                                }
                            })
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.toggle_terminal_panel(window, cx);
                            })),
                    )
                    // "Which of the agents I left running has finished" is the
                    // one question that crosses the worktrees, and it used to
                    // be a screen. It is a state of the terminals now, and its
                    // switch belongs beside them.
                    //
                    // Here and not in the dock's tab bar: a control there would
                    // follow the panel and repeat itself in every tab, which is
                    // what the `+` had to do. This corner is the terminals'
                    // whatever zone holds them.
                    .child(
                        Button::new("terminals-everywhere")
                            .xsmall()
                            .compact()
                            .icon(icon("grid-3x3"))
                            .tooltip(tr!("terminal-every-worktree"))
                            .map(|button| {
                                if self.terminals_everywhere {
                                    button.primary()
                                } else {
                                    button.outline()
                                }
                            })
                            .on_click(cx.listener(|this, _, _window, cx| {
                                this.terminals_everywhere = !this.terminals_everywhere;
                                cx.notify();
                            })),
                    )
                    .child(crate::ui::panels::new_terminal_button(
                        &cx.entity().downgrade(),
                        None,
                    )),
            )
    }
}

impl Focusable for ClaudhubApp {
    fn focus_handle(&self, _cx: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl Render for ClaudhubApp {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // What was said between two frames, before anything is painted: see
        // `flush_notes`.
        self.flush_notes(window, cx);
        self.flush_reveal(window, cx);
        v_flex()
            // Vim mode is read at render time and not at construction: the
            // context is what turns its bindings on, and the setting changes
            // along the way.
            .key_context(super::shortcuts::context(Settings::global(cx).vim_mode))
            .track_focus(&self.focus)
            // **On the root, and it has to be.** A modifier change is a
            // keyboard event: it walks the path from whatever holds the focus
            // up to here, so a listener on the diff itself would be silent
            // whenever the hand is in a terminal or in the tree — which is
            // exactly when one reaches for `Ctrl` and the mouse. Nothing is
            // repainted unless the flag turns over: `Shift` on every capital
            // letter would otherwise cost a frame each.
            .on_modifiers_changed(cx.listener(
                |this, event: &gpui::ModifiersChangedEvent, _, cx| {
                    let armed = event.modifiers.secondary();
                    if this.follow_armed != armed {
                        this.follow_armed = armed;
                        // Let go of it and nothing is followable any more: the
                        // underline goes with the hand cursor.
                        this.follow_hover = None;
                        // The result grid is told rather than asked: its cells
                        // are painted by a delegate, which has no place to read
                        // the application once per frame.
                        this.arm_db_follow(armed, cx);
                        cx.notify();
                    }
                },
            ))
            .on_action(cx.listener(super::shortcuts::refresh))
            .on_action(cx.listener(super::shortcuts::show_line_history))
            .on_action(cx.listener(super::shortcuts::new_terminal))
            .on_action(cx.listener(super::shortcuts::close_terminal))
            .on_action(cx.listener(super::shortcuts::toggle_terminal))
            .on_action(cx.listener(super::shortcuts::next_terminal))
            .on_action(cx.listener(super::shortcuts::commit))
            .on_action(cx.listener(super::shortcuts::open_settings))
            .on_action(cx.listener(super::shortcuts::zoom_in))
            .on_action(cx.listener(super::shortcuts::zoom_out))
            .on_action(cx.listener(super::shortcuts::zoom_reset))
            .on_action(cx.listener(super::shortcuts::copy_diff))
            .on_action(cx.listener(super::shortcuts::copy_diff_patch))
            .on_action(cx.listener(super::shortcuts::select_whole_diff))
            .on_action(cx.listener(super::shortcuts::previous_line))
            .on_action(cx.listener(super::shortcuts::next_line))
            .on_action(cx.listener(super::shortcuts::extend_up))
            .on_action(cx.listener(super::shortcuts::extend_down))
            .on_action(cx.listener(super::shortcuts::previous_hunk))
            .on_action(cx.listener(super::shortcuts::next_hunk))
            .on_action(cx.listener(super::shortcuts::previous_file))
            .on_action(cx.listener(super::shortcuts::next_file))
            .on_action(cx.listener(super::shortcuts::toggle_diff_split))
            .on_action(cx.listener(super::shortcuts::toggle_whole_file))
            .on_action(cx.listener(super::shortcuts::annotate_selection))
            .on_action(cx.listener(super::shortcuts::ask_agent))
            .on_action(cx.listener(super::shortcuts::send_notes))
            .on_action(cx.listener(super::shortcuts::save_file))
            .on_action(cx.listener(super::shortcuts::find))
            .on_action(cx.listener(super::shortcuts::find_file))
            .on_action(cx.listener(super::shortcuts::close_find))
            .on_action(cx.listener(super::shortcuts::find_next))
            .on_action(cx.listener(super::shortcuts::find_previous))
            .on_action(cx.listener(super::shortcuts::search_project))
            .on_action(cx.listener(super::shortcuts::search_up))
            .on_action(cx.listener(super::shortcuts::search_down))
            .on_action(cx.listener(super::shortcuts::search_open))
            .on_action(cx.listener(super::shortcuts::explorer_up))
            .on_action(cx.listener(super::shortcuts::explorer_down))
            .on_action(cx.listener(super::shortcuts::explorer_left))
            .on_action(cx.listener(super::shortcuts::explorer_right))
            .on_action(cx.listener(super::shortcuts::explorer_open))
            .on_action(cx.listener(super::shortcuts::goto_definition))
            .on_action(cx.listener(super::shortcuts::jump_back))
            .on_action(cx.listener(super::shortcuts::jump_forward))
            .on_action(cx.listener(super::shortcuts::explorer_home))
            .on_action(cx.listener(super::shortcuts::explorer_end))
            .on_action(cx.listener(super::shortcuts::db_up))
            .on_action(cx.listener(super::shortcuts::db_down))
            .on_action(cx.listener(super::shortcuts::db_left))
            .on_action(cx.listener(super::shortcuts::db_right))
            .on_action(cx.listener(super::shortcuts::db_open))
            .on_action(cx.listener(super::shortcuts::run_db_query))
            .on_action(cx.listener(super::shortcuts::toggle_tool))
            .on_action(cx.listener(super::shortcuts::new_db_console))
            .on_action(cx.listener(super::shortcuts::copy_db_result))
            .on_action(cx.listener(super::shortcuts::select_whole_result))
            .on_action(cx.listener(super::shortcuts::export_db_csv))
            .on_action(cx.listener(super::shortcuts::show_shortcuts))
            .on_action(cx.listener(super::shortcuts::toggle_sidebar))
            .on_action(cx.listener(super::shortcuts::toggle_zen))
            .on_action(cx.listener(super::shortcuts::previous_terminal))
            .on_action(cx.listener(super::shortcuts::select_worktree))
            .on_action(cx.listener(super::shortcuts::fetch))
            .on_action(cx.listener(super::shortcuts::pull))
            .on_action(cx.listener(super::shortcuts::push))
            .on_action(cx.listener(super::shortcuts::toggle_stage))
            .on_action(cx.listener(super::shortcuts::toggle_review_tree))
            .on_action(cx.listener(super::shortcuts::diff_start))
            .on_action(cx.listener(super::shortcuts::diff_end))
            .on_action(cx.listener(super::shortcuts::diff_page_up))
            .on_action(cx.listener(super::shortcuts::diff_page_down))
            .on_action(cx.listener(super::shortcuts::close_editor))
            // The thumb buttons, which no keymap carries: gpui gives them as
            // `Navigate`, X11 from buttons 8 and 9 and Wayland from `BTN_SIDE`
            // and `BTN_EXTRA`. On the root and in the bubble phase, which is
            // enough — nothing below listens for them, the terminal included,
            // where only the left button is handed to the program.
            .on_mouse_down(
                gpui::MouseButton::Navigate(gpui::NavigationDirection::Back),
                cx.listener(|this, _, window, cx| this.jump_back(window, cx)),
            )
            .on_mouse_down(
                gpui::MouseButton::Navigate(gpui::NavigationDirection::Forward),
                cx.listener(|this, _, window, cx| this.jump_forward(window, cx)),
            )
            .size_full()
            // The gutter and not the background: what shows between two panels —
            // the resize handles, a collapsed zone — is the plane the cards sit
            // on, not the surface of a card.
            .bg(super::theme::gutter(cx))
            .text_color(cx.theme().foreground)
            .child(self.render_topbar(window, cx))
            // The same breathing room the fork puts between the cards, around
            // them: without this padding, the zones touch the window's edges, the
            // top bar and the status bar, and the cards only breathe from the
            // inside.
            //
            // **Wider on the sides than above and below**, which is a ruler
            // disagreeing with the eye rather than an inconsistency: the top bar
            // and the status bar each close with a border, so four pixels there
            // read as a margin, while the same four against the bare window edge
            // read as nothing at all. Equal numbers looked equal nowhere.
            .child(self.render_workspace(cx))
            .child(self.render_status_bar(cx))
            // gpui-component's layers have to be re-emitted by the root view,
            // otherwise dialogs and notifications appear nowhere.
            .children(Root::render_dialog_layer(window, cx))
            .children(Root::render_notification_layer(window, cx))
    }
}

impl ClaudhubApp {
    /// Opens a dialog with a single input line.
    ///
    /// The `InputState` is created here and captured by the closure: an entity
    /// recreated on every frame would lose the cursor, the selection and the text
    /// on the first character.
    pub(super) fn open_text_dialog(
        &mut self,
        title: SharedString,
        placeholder: SharedString,
        window: &mut Window,
        cx: &mut Context<Self>,
        on_ok: impl Fn(&mut Self, String, &mut Window, &mut Context<Self>) + 'static,
    ) {
        self.open_text_dialog_with(title, placeholder, "", window, cx, on_ok)
    }

    /// The same thing, with the field already filled in.
    ///
    /// "New file here" needs the folder under the cursor: retyping it every time
    /// is what makes the gesture go unused.
    pub(super) fn open_text_dialog_with(
        &mut self,
        title: SharedString,
        placeholder: SharedString,
        value: impl Into<SharedString>,
        window: &mut Window,
        cx: &mut Context<Self>,
        on_ok: impl Fn(&mut Self, String, &mut Window, &mut Context<Self>) + 'static,
    ) {
        let value = value.into();
        let input = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder(placeholder)
                .default_value(value)
        });
        let entity = cx.entity();
        let field = input.clone();
        let on_ok = std::rc::Rc::new(on_ok);
        window.open_dialog(cx, move |dialog, _window, _cx| {
            let (input, entity, on_ok) = (input.clone(), entity.clone(), on_ok.clone());
            dialog
                .title(title.clone())
                .overlay_closable(false)
                .close_button(false)
                .footer(super::dialogs::confirm())
                .child(gpui_component::input::Input::new(&input))
                // The window is passed to the closure: what is launched next —
                // opening a terminal, delivering a text into it — needs it, and
                // taking it back afterwards would mean one frame's gap with the
                // gesture.
                .on_ok(move |_, window, cx| {
                    let value = input.read(cx).value().to_string();
                    entity.update(cx, |this, cx| on_ok(this, value, window, cx));
                    true
                })
        });
        // The field takes the focus, so that the dialog is typed in and answered
        // with Enter without a click first.
        super::dialogs::focus_field(&field, window, cx);
    }
}

impl ClaudhubApp {
    pub(super) fn active_path(&self) -> Option<PathBuf> {
        self.active.clone()
    }

    /// Gives the window back its initial arrangement.
    ///
    /// The escape hatch of a system where everything can be moved: a panel
    /// dragged out of sight has no other way back. It rebuilds the whole window
    /// now rather than one screen out of nine, which is what makes it worth
    /// reaching for — and folding a zone is not what it repairs, the rails
    /// putting a folded zone one press from its return.
    pub(super) fn reset_layout(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.dock.update(cx, |area, cx| {
            crate::ui::dock_layout::install_default_layout(area, window, cx);
        });
        self.schedule_layout_save(cx);
        cx.notify();
    }

    /// Where each panel sits, as the dock's tree says it — see
    /// `dock_layout::seats`.
    pub(super) fn seats(&self, cx: &App) -> Vec<crate::ui::rails::Seat> {
        crate::ui::dock_layout::seats(self.dock.read(cx), cx)
    }

    /// Brings a view out, wherever the dock holds it: visible, its zone
    /// unfolded, its tab in front.
    ///
    /// **The single funnel of every "show me that".** There used to be four —
    /// `reveal`, `travel_reveal`, `show_panel` and `revealed_at_home` — because
    /// the answer depended on whether one stood on the home screen, where the
    /// other screens' centres were tabs rather than places. With one workspace
    /// the question does not arise: showing a view is bringing its tab forward,
    /// always, and the only thing left to decide is whether the tab exists.
    pub(super) fn reveal_panel(
        &mut self,
        name: &'static str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.reveal_quietly(name, window, cx);
        // The focus follows a **document** and not a tool window: a view
        // brought out beside what one is reading is not what one has come to
        // type in, and taking the caret out of the editor to put it nowhere is
        // exactly what the old `focus_workspace` was careful not to do.
        if crate::ui::rails::side_of(name, &self.seats(cx)).is_none() {
            self.focus_panel(name, window, cx);
        }
    }

    /// The same, leaving the focus where it is — see `flush_reveal`.
    fn reveal_quietly(&mut self, name: &'static str, window: &mut Window, cx: &mut Context<Self>) {
        self.show_panel(name, cx);
        // **The editor is reached by the file being read, and not by its
        // placeholder.** `EditorPanel` hides itself as soon as a file is open,
        // and selecting a hidden tab shows nothing at all: the group falls back
        // to its first visible panel, which is whatever was there before — the
        // diff, most often. The tab arrived and the window did not move.
        //
        // It is the rule `revealed_at_home` carried, and it outlived the
        // screens that made it necessary.
        if name == super::panels::EditorPanel::NAME {
            if let Some(panel) = self.editing().map(|editing| editing.panel.clone()) {
                super::panels::FilePanel::activate(&panel, window, cx);
                cx.notify();
                return;
            }
        }
        let seats = self.seats(cx);
        // Out of the tree — hidden and pruned, or dragged out of everything —
        // and it has to be put back before it can be shown.
        if !seats.iter().any(|seat| seat.panel == name) {
            let dock = self.dock.clone();
            match crate::ui::rails::tool(name) {
                // **Back to its own half**, and the split made if that half has
                // gone. Put back by its region alone it landed in the region's
                // first group — the top — so a tool of the bottom half came
                // back as one more tab beside what it was meant to sit under.
                // `move_to` is the path that knows the difference.
                Some(tool) => crate::ui::dock_layout::move_to(&dock, name, tool.home, window, cx),
                // A document: the centre, among the tabs it shares. The area
                // splits an empty region's root to take the first one.
                None => dock.update(cx, |area, cx| {
                    let Some(handle) = crate::ui::dock_layout::build(name, window, cx) else {
                        return;
                    };
                    crate::ui::panels::dock_panel_at(
                        area,
                        handle,
                        gpui_component::dock::DockPlacement::Center,
                        None,
                        |_| None,
                        window,
                        cx,
                    );
                }),
            }
        }
        // Its zone, if it is in one and it is folded: a tab selected behind a
        // folded edge is a gesture that did nothing.
        if let Some(side) = crate::ui::rails::side_of(name, &self.seats(cx)) {
            self.unfold(side, window, cx);
        }
        let dock = self.dock.clone();
        crate::ui::panels::select_panel_named(&dock, name, window, cx);
        cx.notify();
    }

    /// The same, writing the step down — see `travel_to_panel`'s callers.
    ///
    /// A step is a **document** and nothing else. Unfolding a tool window is
    /// not a movement: showing the history beside what one is reading does not
    /// change where one is, and a back arrow that folded a zone would be one
    /// nobody could predict.
    pub(super) fn travel_to_panel(
        &mut self,
        name: &'static str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let a_document = crate::ui::rails::side_of(name, &self.seats(cx)).is_none();
        if a_document {
            let from = self.here(cx);
            self.record_step(from, crate::ui::jumps::Place::Panel(name), cx);
        }
        self.reveal_panel(name, window, cx);
    }

    /// Carries out what pressing a rail button means — see `rails::press`.
    ///
    /// The decision is made in the pure module and only carried out here, which
    /// is what makes "pressing what is already in front puts the zone away, and
    /// pressing a tab behind it selects it" a thing to test rather than a thing
    /// to watch.
    pub(super) fn press_tool(
        &mut self,
        panel: &'static str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        use crate::ui::rails::Press;
        // The terminals are the one tool window nothing can build back: their
        // content is a process. "Show me a terminal" is therefore "open one if
        // there is none", which is the gesture `Ctrl+T` already makes.
        if panel == super::panels::TerminalPanel::NAME {
            self.toggle_terminal_panel(window, cx);
            return;
        }
        let seats = self.seats(cx);
        match crate::ui::rails::press(panel, &seats) {
            // The half and not its zone: one left with nothing visible stops
            // being drawn on its own, and the other takes the room. Putting
            // away the view alone left the next tab of the half showing in its
            // place, which is not what a press on a lit button asks for.
            Press::Hide { anchor } => {
                for name in crate::ui::rails::showing_in(anchor, &seats) {
                    self.set_panel_folded(name, true, cx);
                }
            }
            Press::Reveal { panel, .. } | Press::Restore { panel, .. } => {
                self.reveal_panel(panel, window, cx)
            }
        }
    }

    /// Sends a tool window to another place, and shows it there.
    ///
    /// The gesture a rail button's right click offers. Dragging does it too and
    /// is what one reaches for with a panel already on screen; this is for the
    /// one folded away, which has no title to take hold of — moving it would
    /// otherwise mean opening it, dragging it, and folding it again.
    pub(super) fn move_tool(
        &mut self,
        panel: &'static str,
        anchor: crate::ui::rails::Anchor,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.show_panel(panel, cx);
        crate::ui::dock_layout::move_to(&self.dock.clone(), panel, anchor, window, cx);
        // A tool sent to a folded edge is a gesture that did nothing.
        self.unfold(anchor.side, window, cx);
        let dock = self.dock.clone();
        crate::ui::panels::select_panel_named(&dock, panel, window, cx);
        self.schedule_layout_save(cx);
        cx.notify();
    }

    /// Moves a tool window one place along its own rail.
    ///
    /// The rail reads the tree, so this reorders the tabs of the half it is in
    /// — there is no list of ours to permute. Wanting the files before the
    /// changes is a thing one should be able to do without editing the source,
    /// and this is where.
    pub(super) fn shift_tool(
        &mut self,
        panel: &'static str,
        delta: isize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        crate::ui::dock_layout::shift(&self.dock.clone(), panel, delta, window, cx);
        self.schedule_layout_save(cx);
        cx.notify();
    }

    /// Unfolds one edge, if it is folded.
    ///
    /// `toggle_dock` notifies the inner dock and not the area, and it is the
    /// area we watch in order to save: without the `notify`, the window would
    /// reopen with the zone in the state from before the gesture.
    pub(super) fn unfold(
        &mut self,
        side: crate::ui::rails::Side,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let placement = crate::ui::dock_layout::placement_of(side);
        self.dock.update(cx, |area, cx| {
            if !area.is_dock_open(placement) {
                area.toggle_dock(placement, window, cx);
                cx.notify();
            }
        });
    }

    /// Writes one step into the trail of the worktree in hand.
    ///
    /// The single place the trail is written outside of an opening — which has
    /// its own funnel, the content arriving a round trip after the gesture. A
    /// step from a place to itself is not one: it would cost a press to undo a
    /// movement that never happened.
    pub(super) fn record_step(
        &mut self,
        from: crate::ui::jumps::Place,
        to: crate::ui::jumps::Place,
        cx: &mut Context<Self>,
    ) {
        if from == to {
            return;
        }
        let Some(worktree) = self.active.clone() else {
            return;
        };
        self.jumps.entry(worktree).or_default().jump(from, to);
        cx.notify();
    }

    /// Whether the trail of the worktree in hand has anywhere to go. What the
    /// two arrows ask, wherever they are painted.
    pub(super) fn can_travel(&self) -> (bool, bool) {
        match self.active.as_ref().and_then(|w| self.jumps.get(w)) {
            Some(trail) => (trail.can_back(), trail.can_forward()),
            None => (false, false),
        }
    }

    /// Gives the focus to what a view is entered for.
    ///
    /// Three views hold a text field, and they are named one by one; everywhere
    /// else the root handle is the right answer, being the one the review's keys
    /// and the window's shortcuts are resolved against.
    ///
    /// A field one cannot see is never focused: the panel may have been hidden,
    /// and there may be no file open nor any console at all.
    fn focus_panel(&mut self, name: &str, window: &mut Window, cx: &mut Context<Self>) {
        use crate::ui::panels::*;
        if name == EditorPanel::NAME {
            if let Some(editing) = self.editing() {
                let handle = gpui::Focusable::focus_handle(&editing.input, cx);
                handle.focus(window, cx);
                return;
            }
            // No file open: the tree is what there is to work in, and its arrows
            // are the ones one reaches for.
            if self.panel_visible(FilesPanel::NAME) {
                self.explorer_focus.clone().focus(window, cx);
                return;
            }
        }
        if name == SearchPanel::NAME && self.panel_visible(SearchPanel::NAME) {
            let handle = gpui::Focusable::focus_handle(&self.search_input, cx);
            handle.focus(window, cx);
            return;
        }
        window.focus(&self.focus, cx);
    }

    pub(super) fn terminal_visible(&self, _cx: &App) -> bool {
        self.panel_visible(super::panels::TerminalPanel::NAME)
    }

    /// Whether there is a terminal on screen right now — what the status bar's
    /// button lights up for.
    ///
    /// Not the same reading as `terminal_visible`, which is the **flag**: it
    /// says the terminals are not hidden, and it stays true when the last tab
    /// has been closed by hand. A lit button over an empty corner says the one
    /// thing a toggle must never say, that what it names is on screen when it is
    /// not. Nothing is written here: the flag describes an intent, and the next
    /// press reads it — pressed while empty, it opens one, which is what
    /// `toggle_terminal_panel` already does.
    pub(super) fn terminals_on_screen(&self, cx: &App) -> bool {
        if !self.terminal_visible(cx) {
            return false;
        }
        // Showing every worktree's is what the multiplexer used to be a screen
        // for: counting only the one being looked at would call the corner
        // empty in front of a dozen live shells.
        if self.terminals_everywhere {
            return !self.terminals.is_empty();
        }
        self.active
            .as_deref()
            .is_some_and(|worktree| self.terminals_of(worktree).next().is_some())
    }

    /// Whether *this* worktree's terminals are on screen.
    ///
    /// Two conditions, and the second is what makes a terminal keep its place:
    /// the terminals of the worktrees one is not looking at stay in the dock's
    /// tree, invisible. A `TabGroup` renders no tab for an invisible panel and
    /// closes itself when none is left, so a zone empties and fills again with
    /// nothing moved — which is why a terminal dragged into a split is still
    /// there after a round trip through another worktree.
    pub(super) fn terminal_shown(&self, worktree: &Path, cx: &App) -> bool {
        self.terminal_visible(cx)
            && (self.terminals_everywhere || self.active.as_deref() == Some(worktree))
    }

    pub(super) fn show_terminal_panel(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.show_panel(super::panels::TerminalPanel::NAME, cx);
        if let Some(worktree) = self.active.clone() {
            self.ensure_terminal(&worktree, window, cx);
        }
    }

    /// A set of panel names as the settings file takes them.
    ///
    /// Sorted: a `HashSet` serialised in a different order every time would
    /// make a file that changes with nothing having changed.
    fn sorted_names(set: &std::collections::HashSet<String>) -> Vec<String> {
        let mut names: Vec<String> = set.iter().cloned().collect();
        names.sort();
        names
    }

    /// A view is drawn unless it is folded away or taken off its rail.
    ///
    /// **Two states and not one.** They were the same set, and it made one
    /// gesture undo the other: pressing a lit button to put a view away marked
    /// it exactly as "I do not use Sentry" did, so the button one had just
    /// pressed was the button that should have gone. Folding is a moment —
    /// give me the room back — and taking a view off is a decision one makes
    /// once.
    ///
    /// The question is asked in the negative: a panel just added shows up
    /// without anything having to declare it, and a name unknown to the
    /// settings file — a renamed panel — no longer hides anything.
    pub(super) fn panel_visible(&self, name: &str) -> bool {
        !self.folded_panels.contains(name) && !self.off_panels.contains(name)
    }

    /// Whether a view has been taken off its rail: no button, no tab.
    ///
    /// What the "Views" menu ticks, and the one thing its tick can mean — a
    /// folded view is still there, one press away.
    pub(super) fn panel_off(&self, name: &str) -> bool {
        self.off_panels.contains(name)
    }

    /// Folds a view away, or gives it back. Its button stays either way.
    pub(super) fn set_panel_folded(&mut self, name: &str, folded: bool, cx: &mut Context<Self>) {
        let changed = if folded {
            self.folded_panels.insert(name.to_string())
        } else {
            self.folded_panels.remove(name)
        };
        if !changed {
            return;
        }
        let folded = Self::sorted_names(&self.folded_panels);
        Settings::update_global(cx, |s| s.folded_panels = folded);
        cx.notify();
    }

    /// Takes a view off its rail, or puts it back.
    ///
    /// Off is off: no button, no tab, and the "Views" menu of the title bar is
    /// where it comes back from. A view taken off is also unfolded, so that
    /// putting it back is one gesture and not two.
    pub(super) fn set_panel_off(&mut self, name: &str, off: bool, cx: &mut Context<Self>) {
        let changed = if off {
            self.off_panels.insert(name.to_string())
        } else {
            self.off_panels.remove(name) | self.folded_panels.remove(name)
        };
        if !changed {
            return;
        }
        // Sorted before being written, like the store's collapses: a set
        // serialised in a different order every time would make a settings file
        // that changes with nothing having changed.
        let (off, folded) = (
            Self::sorted_names(&self.off_panels),
            Self::sorted_names(&self.folded_panels),
        );
        Settings::update_global(cx, |s| {
            s.hidden_panels = off;
            s.folded_panels = folded;
        });
        cx.notify();
    }

    pub(super) fn toggle_panel_off(&mut self, name: &str, cx: &mut Context<Self>) {
        let off = self.panel_off(name);
        self.set_panel_off(name, !off, cx);
    }

    /// Unfolds a view, and puts it back on its rail if it had been taken off.
    ///
    /// "Show me this" answers both states at once: a gesture that reached a
    /// view taken off and only unfolded it would do nothing at all.
    pub(super) fn show_panel(&mut self, name: &str, cx: &mut Context<Self>) {
        self.set_panel_off(name, false, cx);
        self.set_panel_folded(name, false, cx);
    }

    /// The area keyboard zoom aims at.
    ///
    /// The focus decides: a terminal being looked at enlarges itself, and
    /// everywhere else it is the reviewed code one wants larger. Asking the user
    /// to name an area before zooming would be one more gesture for an intention
    /// that is never ambiguous.
    fn zoom_zone(&self, window: &Window, cx: &App) -> crate::ui::settings::Zoom {
        if self.terminal_focused(window, cx) {
            crate::ui::settings::Zoom::Terminal
        } else {
            crate::ui::settings::Zoom::Diff
        }
    }

    pub(super) fn zoom(&mut self, steps: f32, window: &mut Window, cx: &mut Context<Self>) {
        let zone = self.zoom_zone(window, cx);
        Settings::update_global(cx, |s| {
            s.zoom(zone, steps);
        });
        cx.notify();
    }

    pub(super) fn reset_zoom(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let zone = self.zoom_zone(window, cx);
        Settings::update_global(cx, |s| {
            s.reset_zoom(zone);
        });
        cx.notify();
    }

    pub(super) fn toggle_terminal_panel(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let worktree = self.active.clone();
        // Nothing open yet: the button **shows**, whatever the flag said. The
        // flag is on by default and there is no terminal at startup, so a plain
        // toggle would spend the first click hiding an emptiness — a button
        // that does nothing, which is how one concludes it is broken.
        let empty = worktree
            .as_deref()
            .is_some_and(|worktree| self.terminals_of(worktree).next().is_none());
        let terminals = super::panels::TerminalPanel::NAME;
        if empty {
            self.show_panel(terminals, cx);
        } else {
            let folded = !self.panel_visible(terminals);
            self.show_panel(terminals, cx);
            if !folded {
                self.set_panel_folded(terminals, true, cx);
            }
        }
        // The first one is opened on demand, never when a worktree is: a shell
        // nobody asked for is a process nobody asked for.
        if self.terminal_visible(cx) {
            if let Some(worktree) = worktree {
                self.ensure_terminal(&worktree, window, cx);
                // And the focus goes with it: the gesture is "I want a terminal
                // now", and one that has to be clicked before it takes a
                // keystroke is a gesture left half done.
                self.focus_terminal(&worktree, window, cx);
            }
        } else {
            // Hiding takes the focus back to the root, and it is not a courtesy.
            // What was focused was the terminal this gesture just removed from
            // the tree; a focus handle nobody renders any more resolves no
            // binding, so `Ctrl+T` did nothing at all until a click somewhere
            // put the focus back on a live node.
            window.focus(&self.focus, cx);
        }
    }

    /// Folds one edge away, or brings it back.
    ///
    /// `toggle_dock` only notifies the inner dock, and it is the area we observe
    /// to save the layout: without this `notify`, the window would reopen with
    /// the zone in the state from before the gesture.
    pub(super) fn toggle_zone(
        &mut self,
        side: crate::ui::rails::Side,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let placement = crate::ui::dock_layout::placement_of(side);
        self.dock.update(cx, |area, cx| {
            area.toggle_dock(placement, window, cx);
            cx.notify();
        });
    }

    /// Shows or hides the left zone — what one picks from.
    pub(super) fn toggle_sidebar(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.toggle_zone(crate::ui::rails::Side::Left, window, cx);
    }

    /// The worktrees in the order the sidebar shows them.
    ///
    /// Collapses change nothing there: `Ctrl+3` has to name the same worktree
    /// whether its repository is collapsed or not, otherwise the shortcut would
    /// only be memorable in one state of the list.
    fn worktrees_in_order(&self) -> Vec<PathBuf> {
        self.repos.worktrees_in_order()
    }

    pub(super) fn select_worktree_at(
        &mut self,
        index: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(path) = self.worktrees_in_order().into_iter().nth(index) {
            self.select_worktree(path, window, cx);
        }
    }

    pub(super) fn fetch(&mut self, cx: &mut Context<Self>) {
        if let Some(worktree) = self.active.clone() {
            self.git.send(Cmd::Fetch { worktree });
            cx.notify();
        }
    }

    pub(super) fn pull(&mut self, cx: &mut Context<Self>) {
        if let Some(worktree) = self.active.clone() {
            self.git.send(Cmd::Pull { worktree });
            cx.notify();
        }
    }

    pub(super) fn push(&mut self, cx: &mut Context<Self>) {
        if let Some(worktree) = self.active.clone() {
            self.git.send(Cmd::Push {
                worktree,
                force_with_lease: false,
            });
            cx.notify();
        }
    }

    /// Ticks or unticks the open file, like a click on its box.
    ///
    /// The status is the only source that tells the index from the working tree:
    /// a file absent from its list is not stageable — it is a commit's file, not
    /// a change in progress.
    pub(super) fn toggle_stage_of_open_file(&mut self, cx: &mut Context<Self>) {
        let Some(worktree) = self.active.clone() else {
            return;
        };
        let Some(state) = self.active_review() else {
            return;
        };
        let Some(path) = state.selected.clone() else {
            return;
        };
        let Some(file) = state.status.files.iter().find(|file| file.path == path) else {
            return;
        };
        let staged = file.is_staged();
        self.set_staged(worktree, vec![path], !staged, cx);
    }
}

/// Whether git still reports this file as unmerged.
fn unmerged(state: &ReviewState, path: &Path) -> bool {
    state.status.conflicted().any(|file| file.path == path)
}

/// Part of the file is in the index and part is not — what git writes `MM`.
/// The remainder panel under the diff only exists for those.
pub(super) fn partially_staged(status: &Status, path: &Path) -> bool {
    status
        .files
        .iter()
        .any(|f| f.path == path && f.is_staged() && f.is_unstaged() && !f.is_untracked())
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui_component::dock::{PanelInfo, PanelState};

    fn leaf(name: &str) -> PanelState {
        PanelState::new(name)
    }

    fn stack(sizes: Vec<gpui::Pixels>, children: Vec<PanelState>) -> PanelState {
        let mut state = PanelState::new("StackPanel");
        state.info = PanelInfo::stack(sizes, gpui::Axis::Vertical);
        state.children = children;
        state
    }

    fn tabs(active: usize, children: Vec<PanelState>) -> PanelState {
        let mut state = PanelState::new("TabPanel");
        state.info = PanelInfo::tabs(active);
        state.children = children;
        state
    }

    /// A side zone as `dump` writes one: a tree, a side, a size, an open flag.
    fn dock(tree: PanelState, size: gpui::Pixels) -> Option<gpui_component::dock::DockState> {
        Some(gpui_component::dock::DockState::new(
            tree,
            gpui_component::dock::DockPlacement::Bottom,
            size,
            true,
        ))
    }

    /// A session opens the Git screen on its changes, wherever the tab sits and
    /// whatever was on screen when the window closed — a plugin's board, here.
    #[test]
    fn the_git_screen_opens_on_its_changes() {
        let mut state = stack(
            vec![px(400.), px(900.)],
            vec![
                tabs(
                    5,
                    vec![
                        leaf("ClaudhubChanges"),
                        leaf("ClaudhubHistory"),
                        leaf("ClaudhubPlugin:ci/main"),
                    ],
                ),
                tabs(0, vec![leaf("ClaudhubDiff")]),
            ],
        );
        assert!(open_on(&mut state, "ClaudhubChanges"));
        assert!(matches!(
            state.children[0].info,
            PanelInfo::Tabs { active_index: 0 }
        ));
        // A screen that no longer holds the panel is left exactly as it was.
        let mut without = tabs(1, vec![leaf("ClaudhubDiff"), leaf("ClaudhubNotes")]);
        assert!(!open_on(&mut without, "ClaudhubChanges"));
        assert!(matches!(without.info, PanelInfo::Tabs { active_index: 1 }));
    }

    /// The shape the file actually carries: the group is two splits deep, and
    /// the plugin's tab is the sixth of six.
    #[test]
    fn the_saved_git_screen_opens_on_its_changes() {
        let mut state = stack(
            vec![px(400.), px(900.)],
            vec![
                stack(
                    vec![px(600.), px(200.)],
                    vec![
                        tabs(
                            5,
                            vec![
                                leaf("ClaudhubChanges"),
                                leaf("ClaudhubBranch"),
                                leaf("ClaudhubHistory"),
                                leaf("ClaudhubTags"),
                                leaf("ClaudhubConflicts"),
                                leaf("ClaudhubPlugin:ci/main"),
                            ],
                        ),
                        tabs(0, vec![leaf("ClaudhubNotes")]),
                    ],
                ),
                tabs(0, vec![leaf("ClaudhubDiff")]),
            ],
        );
        assert!(open_on(&mut state, "ClaudhubChanges"));
        assert!(matches!(
            state.children[0].children[0].info,
            PanelInfo::Tabs { active_index: 0 }
        ));
    }

    /// The dead name inside a container is what the pruning is for, and the
    /// container's own name is **not** one the registry knows: judging it too
    /// stopped the walk at the root, so nothing was ever pruned and the screen
    /// carried "panel type is not registered" at every start.
    #[test]
    fn a_container_is_not_judged_by_its_name() {
        let known = |name: &str| name == "ClaudhubDiff";
        let mut state = tabs(0, vec![leaf("ClaudhubMultiplexer")]);
        assert!(prune(&mut state, &|name| !known(name)), "nothing is left");
        let mut state = stack(
            vec![px(400.), px(900.)],
            vec![tabs(
                0,
                vec![leaf("ClaudhubMultiplexer"), leaf("ClaudhubDiff")],
            )],
        );
        assert!(!prune(&mut state, &|name| !known(name)));
        assert_eq!(state.panel_name, "TabPanel", "the lone split collapsed");
        assert_eq!(state.children.len(), 1);
        assert_eq!(state.children[0].panel_name, "ClaudhubDiff");
    }

    /// The terminal goes, and the split it was half of goes with it.
    ///
    /// What the survivor must **not** keep is its size: it was measured while
    /// the terminal sat below it, and read back it pins the slot to a height
    /// that describes a window nobody has any more.
    #[test]
    fn a_split_left_with_one_slot_stops_being_a_split() {
        let mut state = stack(
            vec![px(1108.), px(260.)],
            vec![leaf("ClaudhubEditor"), leaf("ClaudhubTerminal")],
        );
        assert!(!prune(&mut state, &|name| name == "ClaudhubTerminal"));
        assert_eq!(state.panel_name, "ClaudhubEditor");
        assert!(state.children.is_empty());
    }

    /// A split that keeps two slots keeps its sizes, and loses the one whose
    /// panel went — the sizes of a stack are positional.
    #[test]
    fn a_split_that_keeps_two_slots_keeps_its_shape() {
        let mut state = stack(
            vec![px(300.), px(400.), px(260.)],
            vec![
                leaf("ClaudhubChanges"),
                leaf("ClaudhubNotes"),
                leaf("ClaudhubTerminal"),
            ],
        );
        assert!(!prune(&mut state, &|name| name == "ClaudhubTerminal"));
        assert_eq!(state.children.len(), 2);
        match state.info {
            PanelInfo::Stack { sizes, .. } => assert_eq!(sizes, vec![px(300.), px(400.)]),
            _ => panic!("still a stack"),
        }
    }

    /// A container emptied by the pruning is dropped by its parent, and does
    /// not come back as a bare tab bar.
    #[test]
    fn an_emptied_container_is_dropped() {
        let mut state = stack(vec![px(260.)], vec![leaf("ClaudhubTerminal")]);
        assert!(prune(&mut state, &|name| name == "ClaudhubTerminal"));
    }

    /// A terminal docked in the bottom zone is taken out before writing, like
    /// one in the centre. This is the whole reason `prune_dock` exists: a
    /// terminal is a process, nothing can build one back, and the name left
    /// behind is what paints "panel type is not registered" over the strip.
    #[test]
    fn a_terminal_of_a_side_zone_is_pruned() {
        let mut zone = dock(
            tabs(0, vec![leaf("ClaudhubTerminal"), leaf("ClaudhubTestRun")]),
            px(260.),
        );
        prune_dock(&mut zone, &|name| name == "ClaudhubTerminal");
        let zone = zone.expect("the zone still holds the run");
        assert_eq!(zone.panel().children.len(), 1);
        assert_eq!(zone.panel().children[0].panel_name, "ClaudhubTestRun");
    }

    /// A zone whose tree is doomed whole goes away entirely. Kept as an empty
    /// `DockState`, it would reopen on nothing at the next start.
    #[test]
    fn a_zone_emptied_whole_goes_away() {
        let mut zone = dock(tabs(0, vec![leaf("ClaudhubTerminal")]), px(260.));
        prune_dock(&mut zone, &|name| name == "ClaudhubTerminal");
        assert!(zone.is_none());
    }

    /// What the zone was — its side, its size, whether it was unfolded —
    /// survives the taking apart. It is the reason the four readers are read
    /// rather than a fresh `DockState` being made up.
    #[test]
    fn a_zone_keeps_its_size_and_its_opening() {
        let mut zone = dock(
            tabs(0, vec![leaf("ClaudhubTerminal"), leaf("ClaudhubTestRun")]),
            px(260.),
        );
        prune_dock(&mut zone, &|name| name == "ClaudhubTerminal");
        let zone = zone.expect("the zone survives");
        assert_eq!(zone.size(), px(260.));
        assert!(zone.open());
        assert_eq!(
            zone.placement(),
            gpui_component::dock::DockPlacement::Bottom
        );
    }

    /// A name nothing can build any more is taken out of a side zone too —
    /// the case the reading side asks about, and the one a comment long said
    /// was out of reach.
    #[test]
    fn a_dead_name_of_a_side_zone_is_pruned() {
        let mut zone = dock(
            tabs(0, vec![leaf("ClaudhubGone"), leaf("ClaudhubNotes")]),
            px(320.),
        );
        prune_dock(&mut zone, &|name| name == "ClaudhubGone");
        let zone = zone.expect("the notes are still there");
        assert_eq!(zone.panel().children.len(), 1);
    }

    /// The rules of `prune` hold inside a zone as they do in the centre: a
    /// split left with one slot drops the stale size with its wrapper.
    #[test]
    fn a_split_left_with_one_slot_flattens_in_a_zone_too() {
        let mut zone = dock(
            stack(
                vec![px(300.), px(260.)],
                vec![leaf("ClaudhubNotes"), leaf("ClaudhubTerminal")],
            ),
            px(560.),
        );
        prune_dock(&mut zone, &|name| name == "ClaudhubTerminal");
        let zone = zone.expect("the notes are still there");
        assert_eq!(zone.panel().panel_name, "ClaudhubNotes");
        assert!(zone.panel().children.is_empty());
    }
}
