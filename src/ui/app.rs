//! The root entity: the window's state and the event pump.
//!
//! The sub-views are not separate entities but `impl ClaudhubApp` blocks spread
//! across files (`sidebar`, `review`, `branches`). Everything they show comes
//! from the same state, and passing it between entities would cost more code
//! than it saves. The terminals are the exception: they have their own lifetime
//! and are `Entity<TerminalView>`.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use gpui::{
    div, prelude::*, px, App, Context, Entity, FocusHandle, Focusable, Render, SharedString, Window,
};
use gpui_component::{
    button::{Button, ButtonVariants},
    dock::{DockArea, DockSkin},
    h_flex,
    input::{InputState, TextareaState},
    menu::{DropdownMenu, PopupMenuItem},
    select::{SearchableVec, SelectEvent, SelectState},
    separator::Separator as Divider,
    v_flex, ActiveTheme, Root, Selectable, Sizable, StyledExt, TitleBar, WindowExt,
};

use crate::git::{Branch, Commit, DiffFile, DiffRange, GraphRow, LogRange, Status, Worktree};
use crate::runtime::{self, Action, Cmd, Evt};
use crate::tr;

use crate::ui::base_select::BaseChoice;
use crate::ui::diff_view::Rendered;
use crate::ui::icons::icon;
use crate::ui::settings::Settings;
use crate::ui::store::Store;
use crate::ui::terminal_view::TerminalGroup;

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
const LAYOUT_VERSION: usize = 11;

/// The saved layouts, one per screen.
///
/// **One file and not one per screen**: it is *one* window's state, and reading it in
/// pieces would serve nobody — least of all whoever opens it to understand why
/// their screen is crooked. The version wraps the whole thing: screens can
/// appear, disappear and be renamed, and a map of unknown names builds nothing.
#[derive(serde::Serialize, serde::Deserialize, Default)]
struct Layouts {
    version: Option<usize>,
    /// The screen being looked at when closing.
    ///
    /// It lives here and not in the settings: it is a window's state, just like
    /// the panels' places, and not a preference written by hand.
    current: Option<String>,
    workspaces: std::collections::BTreeMap<String, gpui_component::dock::DockAreaState>,
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
    /// The review notes taken on this worktree, all files together.
    ///
    /// They live here and not in the diff: a note survives a reload of the file,
    /// a change of range and Claudhub being closed, whereas a `Rendered` does
    /// not survive the next file write.
    pub notes: Vec<crate::ui::notes::Note>,
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
}

/// Which end a file opened with the keyboard starts from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Jump {
    /// Going down: we enter by the first change.
    First,
    /// Going up: we enter by the last, where reading stops.
    Last,
}

/// The history as the view shows it.
pub struct History {
    pub commits: Vec<Commit>,
    pub graph: Vec<GraphRow>,
    /// The graph's number of columns, to size its gutter.
    pub width: usize,
}

impl Default for ReviewState {
    fn default() -> Self {
        Self {
            range: DiffRange::Working,
            status: Status::default(),
            files: HashMap::new(),
            pending_files: std::collections::HashSet::new(),
            selected: None,
            diff: None,
            diff_selection: None,
            collapsed: std::collections::HashSet::new(),
            base: None,
            history: None,
            history_range: LogRange::All,
            history_pending: false,
            commit: None,
            notes: Vec::new(),
            next_note: 1,
            reviewed: Vec::new(),
            journal: String::new(),
            todo: None,
            notes_loaded: false,
            notes_on_disk: false,
            note_marks: std::rc::Rc::new(crate::ui::notes::Marks::default()),
            drifted: std::collections::HashSet::new(),
            pending_jump: None,
            pending_note: None,
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

/// The last action's result, shown in the status bar.
pub struct Toast {
    pub text: SharedString,
    pub error: bool,
}

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
    /// The selected worktree: the key to almost everything else.
    pub(super) active: Option<PathBuf>,
    pub(super) review: HashMap<PathBuf, ReviewState>,
    pub(super) terminals: HashMap<PathBuf, Entity<TerminalGroup>>,
    pub(super) commit_input: Entity<TextareaState>,
    /// The comparison base's selector. It is searchable: a living repository has
    /// dozens of branches, and scrolling a list of seventy entries to find one
    /// whose name you already know is exactly what a search field saves.
    pub(super) base_select: Entity<SelectState<SearchableVec<BaseChoice>>>,
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
    /// What `wt` knows about each worktree: started or not, its addresses.
    pub(super) wt_states: HashMap<PathBuf, crate::runtime::protocol::WtWorktree>,
    /// The guided creation under way.
    pub(super) creation: Option<crate::ui::worktree_ops::WtPrompt>,
    /// The worktree whose integration has gone out, and its branch: it is on the
    /// success arriving that the cleanup is offered.
    pub(super) integrated: Option<(PathBuf, String)>,
    /// Each worktree's files and their tree.
    pub(super) explorers: HashMap<PathBuf, crate::ui::explorer::Explorer>,
    /// The file open in the built-in editor, if there is one.
    pub(super) editing: Option<crate::ui::explorer::Editing>,
    pub(super) files_scroll: gpui::UniformListScrollHandle,
    /// The explorer tree's focus, which gives it its arrows.
    ///
    /// A handle of its own and not the root view's: it is what tells "the arrows
    /// browse the tree" from "the arrows browse the diff", and the bindings'
    /// predicate is read on the focused node's context.
    pub(super) explorer_focus: FocusHandle,
    /// The current repository's Sentry issues, and the one being looked at.
    pub(super) sentry: crate::ui::sentry_view::SentryState,
    /// The databases tree: connections, schemas, tables, columns.
    pub(super) db: crate::ui::db::DbState,
    pub(super) db_scroll: gpui::UniformListScrollHandle,
    /// The databases tree's focus, which gives it its arrows.
    ///
    /// A handle of its own, like the project explorer's: it is what tells "the
    /// arrows browse the schema" from "the arrows browse the diff".
    pub(super) db_focus: FocusHandle,
    /// The SQL console, when one is open.
    pub(super) query: crate::ui::db_query::QueryState,
    /// The console's editor. Created **once**: recreated at render time, it
    /// would lose the cursor, the selection and the text on the first keystroke.
    pub(super) db_query_input: Entity<gpui_component::input::EditorState>,
    /// The names the console completes, shared with the completion provider the
    /// editor holds.
    pub(super) db_schema: std::rc::Rc<std::cell::RefCell<crate::ui::db_query::SchemaIndex>>,
    /// The result table. An entity created once as well: rebuilding it on every
    /// query would lose the column widths just adjusted with the mouse.
    pub(super) db_table: Entity<gpui_component::table::TableState<crate::ui::db_query::Results>>,
    /// The height split between the console's editor and its grid.
    ///
    /// An entity, because that is what `v_resizable` asks for, and created once:
    /// recreated at render time, the handle would go back to its place on every
    /// frame and would not let itself be dragged.
    pub(super) db_split: Entity<gpui_component::resizable::ResizableState>,
    /// A worktree being waited for, and the prompt to deliver in it once created.
    ///
    /// `wt`'s creation runs hooks that take minutes; nothing but the arrival of
    /// the worktree list says it has finished.
    pub(super) awaiting_agent: Option<(PathBuf, String)>,
    pub(super) toast: Option<Toast>,
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
    /// The current screen. See `ui::workspace`.
    pub(super) workspace: crate::ui::workspace::Workspace,
    /// The current screen's dock — the one the root view shows.
    ///
    /// A copy of the `docks` entry, and not an index: everything that talks to
    /// the dock — collapsing the left zone, the zoom, the width the navigation
    /// bar reads — addresses the one being looked at, and finding it by a key
    /// every time would only add one more chance to get it wrong.
    pub(super) dock: Entity<DockArea>,
    /// The docks, one per screen.
    ///
    /// They are all **built at startup** rather than on first visit: a dock is
    /// built with `window`, and doing it at render time would amount to creating
    /// entities in the middle of a frame. The cost is a score of panels, which
    /// carry no state.
    pub(super) docks: HashMap<crate::ui::workspace::Workspace, Entity<DockArea>>,
    /// The dock's skin, kept alive: it is what draws the tabs, and it is through
    /// it that the presentation settings pass.
    #[allow(dead_code)]
    dock_skin: std::rc::Rc<DockSkin>,
    /// True when a deferred write of the layout is already scheduled.
    layout_save_scheduled: bool,
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
    pub(super) hidden_panels: std::collections::HashSet<String>,

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
    /// The handle of the wrapped two-column view.
    ///
    /// A second handle and not the same one: the entries there no longer have
    /// the same height, `v_virtual_list` paints them, and it can only scroll with
    /// its own. The two are never shown at the same time.
    pub(super) diff_wrap_scroll: gpui_component::VirtualListScrollHandle,
    pub(super) history_scroll: gpui::UniformListScrollHandle,
    pub(super) branch_scroll: gpui::UniformListScrollHandle,
    /// The file lists' scrolling, **one per range**: "Review" and "Changes" are
    /// shown at the same time, and a single handle would scroll them together.
    file_scroll: HashMap<DiffRange, gpui::UniformListScrollHandle>,
    /// Each panel's search, created on its first opening.
    pub(super) finders: HashMap<crate::ui::find::Pane, crate::ui::find::Finder>,
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
    /// The branches panel's filter. An entity created once: recreated per frame,
    /// it would lose the cursor and the text on the first keystroke.
    pub(super) branch_filter: Entity<InputState>,
    /// The split between the graph and the chosen commit's file list.
    pub(super) history_split: Entity<gpui_component::resizable::ResizableState>,
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
    /// The log records, copied from the ring only when it has moved.
    pub(super) logs: std::rc::Rc<Vec<crate::logging::Entry>>,
    pub(super) logs_seen: u64,
    focus: FocusHandle,
}

/// The SQL console's entities, handed to the constructor in one piece.
struct Console {
    db_schema: std::rc::Rc<std::cell::RefCell<crate::ui::db_query::SchemaIndex>>,
    db_query_input: Entity<gpui_component::input::EditorState>,
    db_table: Entity<gpui_component::table::TableState<crate::ui::db_query::Results>>,
    db_split: Entity<gpui_component::resizable::ResizableState>,
}

/// The screens' docks, and which of them was being looked at.
struct Docks {
    docks: HashMap<crate::ui::workspace::Workspace, Entity<DockArea>>,
    dock: Entity<DockArea>,
    dock_skin: std::rc::Rc<DockSkin>,
    workspace: crate::ui::workspace::Workspace,
    /// Whether a layout had to be built from scratch, and therefore has to be
    /// written at once: without that, the file keeps an earlier version's until
    /// the first move.
    needs_save: bool,
}

/// One row of the views menu: the tick, the name, and the gesture that toggles it.
///
/// A `PopupMenuItem::element` and not an ordinary entry, for two reasons both of
/// which come down to the same thing — **several of them get toggled in a row**:
///
/// - `PopupMenu::confirm` **closes the menu** after calling an entry's handler,
///   with no way to prevent it. The row therefore consumes the click itself
///   (`stop_propagation`): the entry carrying it never sees it, and nothing
///   closes.
/// - A `checked` is frozen at the menu's construction, which happens only once.
///   The tick is therefore painted by the row, which re-reads the state on every
///   frame.
fn view_toggle(app: Entity<ClaudhubApp>, name: &'static str, title: &'static str) -> PopupMenuItem {
    PopupMenuItem::element(move |_window, cx| {
        let visible = app.read(cx).panel_visible(name);
        let app = app.clone();
        h_flex()
            .id(name)
            .w_full()
            .gap_2()
            .items_center()
            // The tick's column is reserved permanently: without it, the names
            // would jump one notch on every toggle.
            .child(
                div()
                    .w(px(14.))
                    .when(visible, |this| this.child(icon("check").xsmall())),
            )
            .child(tr!(title))
            .on_click(move |_, _window, cx| {
                cx.stop_propagation();
                app.update(cx, |this, cx| this.toggle_panel(name, cx));
            })
    })
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

    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let (git, events, remote) = Self::spawn_backend();

        let commit_input =
            cx.new(|cx| TextareaState::new(window, cx).placeholder(tr!("commit-placeholder")));

        let branch_filter =
            cx.new(|cx| InputState::new(window, cx).placeholder(tr!("branch-filter-placeholder")));

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

        let Console {
            db_schema,
            db_query_input,
            db_table,
            db_split,
        } = Self::build_console(window, cx);

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

        // The docks and their panels. The panels carry no state: they delegate
        // to this entity, of which they only keep a weak reference so as not to
        // form a cycle.
        crate::ui::panels::register(&cx.entity(), cx);
        let Docks {
            docks,
            dock,
            dock_skin,
            workspace,
            needs_save: app_needs_layout_save,
        } = Self::build_docks(window, cx);

        let mut app = Self {
            git,
            repos: crate::ui::repos::Repos::default(),
            server_state: super::server::ServerState::default(),
            wsl_prompt: None,
            server_wsl: false,
            active: None,
            review: HashMap::new(),
            terminals: HashMap::new(),
            commit_input,
            base_select,
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
            wt_states: HashMap::new(),
            creation: None,
            integrated: None,
            explorers: HashMap::new(),
            editing: None,
            files_scroll: gpui::UniformListScrollHandle::new(),
            sentry: Default::default(),
            db: Default::default(),
            db_scroll: gpui::UniformListScrollHandle::new(),
            db_focus: cx.focus_handle(),
            query: Default::default(),
            db_query_input,
            db_schema,
            db_table,
            db_split,
            awaiting_agent: None,
            toast: None,
            pending_status: std::collections::HashSet::new(),
            last_auto_fetch: None,
            suggesting_message: None,
            workspace,
            dock,
            docks,
            dock_skin,
            layout_save_scheduled: false,
            hidden_panels: Settings::global(cx).hidden_panels.iter().cloned().collect(),
            summaries: HashMap::new(),
            agents: crate::agent::Tracker::default(),
            diff_dragging: false,
            diff_scroll: gpui::UniformListScrollHandle::new(),
            diff_width: gpui::px(0.),
            diff_measures: 0,
            diff_wrap_scroll: gpui_component::VirtualListScrollHandle::new(),
            history_scroll: gpui::UniformListScrollHandle::new(),
            branch_scroll: gpui::UniformListScrollHandle::new(),
            file_scroll: HashMap::new(),
            scrolls: HashMap::new(),
            motions: HashMap::new(),
            explorer_focus: cx.focus_handle(),
            finders: HashMap::new(),
            // The diff by default: it is the panel one looks at on arriving, and
            // the only one that is always there.
            pane: crate::ui::find::Pane::Diff,
            diff_search: crate::ui::find::DiffSearch::default(),
            branch_filter,
            history_split: cx.new(|_| gpui_component::resizable::ResizableState::default()),
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
        app.pump_events(events, window, cx);
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
        app
    }

    /// The previous session's repositories, then the current directory if it is
    /// one — that is what somebody launching `claudhub` from their project
    /// expects.
    ///
    /// The check goes to the worker: `is_repo` costs a git subprocess, which the
    /// constructor is not allowed to pay for.
    fn open_remembered_repositories(&mut self, remote: bool, cx: &mut Context<Self>) {
        let remembered = Settings::global(cx).repositories.clone();
        for path in remembered {
            self.git.send(Cmd::OpenRepo(path));
        }
        // Remotely, the directory that counts is the **server's**: it arrives
        // with `Evt::ServerHello`, and ours names nothing over there.
        if !remote {
            if let Ok(cwd) = std::env::current_dir() {
                self.git.send(Cmd::OpenIfRepo(cwd));
            }
        }
    }

    /// The SQL console's entities: its editor, its completion index, its table.
    ///
    /// All of them live as long as the window — that is the rule for gpui
    /// entities — and the console only fills them.
    fn build_console(window: &mut Window, cx: &mut Context<Self>) -> Console {
        let db_schema: std::rc::Rc<std::cell::RefCell<crate::ui::db_query::SchemaIndex>> =
            Default::default();
        let db_query_input = cx.new(|cx| {
            gpui_component::input::EditorState::new(window, cx)
                .language("sql")
                .line_number(true)
                .placeholder("SELECT * FROM …")
        });
        db_query_input.update(cx, |state, cx| {
            state.lsp_mut().completion_provider =
                Some(std::rc::Rc::new(crate::ui::db_query::SqlCompletions {
                    schema: db_schema.clone(),
                }));
            cx.notify();
        });
        let db_table = cx.new(|cx| {
            gpui_component::table::TableState::new(
                crate::ui::db_query::Results::default(),
                window,
                cx,
            )
            // The headers carry their sort arrow. gpui-component's cell
            // selection, for its part, stays off: it knows only one at a time,
            // and what one copies from a result grid is almost always a block —
            // see `db_query::Results::selection`.
            .sortable(true)
        });
        Console {
            db_schema,
            db_query_input,
            db_table,
            db_split: crate::ui::db_query::split_state(cx),
        }
    }

    /// One dock per screen, restored or built, and the one that was being looked
    /// at when the window was closed.
    ///
    /// The four are built at startup and not on first visit: a dock is built
    /// with `window`, and doing it at render time would amount to creating
    /// entities in the middle of a frame.
    fn build_docks(window: &mut Window, cx: &mut Context<Self>) -> Docks {
        let this = cx.entity();
        // A saved layout takes precedence over the default one. The whole file
        // is discarded if its version differs: the panels may have changed name,
        // and rebuilding from unknown names would give a window full of empty
        // frames.
        let saved = load_layouts();
        let mut needs_save = false;
        let mut docks = HashMap::new();
        let mut dock_skin = None;
        for workspace in crate::ui::workspace::Workspace::ALL {
            // **Through `DockSkin` and not through `DockArea::new`.** Since the
            // layout engine moved into `gpui-base`, an area built without a skin
            // docks, drags and persists perfectly well — but draws **no chrome**
            // at all: no tab bar, no title, no frame. The panels then stack bare,
            // which reads as a broken window without a single error being
            // reported.
            let (area, skin) =
                DockSkin::dock_area(workspace.dock_id(), Some(LAYOUT_VERSION), window, cx);
            // `Segmented`: the rounded pill in a rail, in place of the bordered
            // rectangle whose radius is a hard-coded zero. It is our commit on
            // the fork that exposes this setting.
            skin.set_tab_variant(gpui_component::tab::TabVariant::Segmented, cx);
            // A tab bar everywhere, including on groups with a single panel: the
            // default (`Auto`) renders a flat title there, and "Branches" or
            // "Terminals" would not have the same band as their neighbours — two
            // chromes for one window.
            skin.set_panel_style(gpui_component::dock::PanelStyle::TabBar, cx);

            let restored = saved
                .workspaces
                .get(workspace.key())
                .cloned()
                .and_then(|state| area.update(cx, |a, cx| a.load(state, window, cx)).ok())
                .is_some();
            if !restored {
                area.update(cx, |a, cx| {
                    crate::ui::workspace::install_default_layout(workspace, &this, a, window, cx);
                });
                // The initial layout is written at once: without that, the file
                // keeps an earlier version's until the first move, and that is
                // what would be read back on the next startup.
                needs_save = true;
            }
            // The dock notifies on every move, resize or tab change: it is the
            // save signal, deferred so a drag does not write one file per pixel.
            cx.observe(&area, |this, _, cx| this.schedule_layout_save(cx))
                .detach();
            docks.insert(workspace, area);
            dock_skin = Some(skin);
        }
        let workspace = saved
            .current
            .as_deref()
            .and_then(crate::ui::workspace::Workspace::from_key)
            .unwrap_or_default();
        Docks {
            dock: docks[&workspace].clone(),
            docks,
            dock_skin: dock_skin.expect("BUG: at least one screen"),
            workspace,
            needs_save,
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
                let layouts = Layouts {
                    version: Some(LAYOUT_VERSION),
                    current: Some(this.workspace.key().to_string()),
                    workspaces: this
                        .docks
                        .iter()
                        .map(|(workspace, area)| {
                            (workspace.key().to_string(), area.read(cx).dump(cx))
                        })
                        .collect(),
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
            path: dir.join(crate::ui::vault::NOTES),
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

    fn file_changed(&mut self, path: &Path, cx: &mut Context<Self>) {
        let Some(active) = self.active.clone() else {
            return;
        };
        // The vault is not inside the worktree, and what changes there is not
        // read with `git status`: it is the folder itself that has to be re-read.
        if let Some(vault) = self.notes_dir(&active, cx) {
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
            Evt::RepoUnavailable { path, message } => self.repo_unavailable(path, message, cx),
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
            Evt::History {
                worktree,
                range,
                commits,
                graph,
            } => self.history_arrived(worktree, range, commits, graph),
            Evt::Branches {
                main,
                branches,
                default_base,
            } => self.branches_arrived(main, branches, default_base, window, cx),

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
            } => self.action_failed(worktree, action, message),
            Evt::Fetched { main } => self.fetched(main),
            Evt::CommitMessage { worktree, message } => {
                self.commit_message_arrived(worktree, message, window, cx)
            }

            // — `wt` ——————————————————————————————————————————————
            Evt::WtProject { main, project } => {
                self.wt_projects.insert(main, project);
            }
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

            // — Sentry ————————————————————————————————————————————
            Evt::Issues { issues } => self.issues_arrived(issues, cx),
            Evt::IssueEvent { issue, event } => self.issue_event_arrived(issue, event, cx),

            // — Files and vault ———————————————————————————————————
            Evt::ProjectFiles { worktree, files } => self.project_files_arrived(worktree, files),
            Evt::FileContent {
                worktree,
                path,
                content,
            } => self.file_content_arrived(worktree, path, content, window, cx),
            Evt::FilesChanged { paths } => {
                for path in paths {
                    self.file_changed(&path, cx);
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
        // Failing the checkout the opening came from, the first of the list,
        // which is the main repository.
        let first = opened_at.or_else(|| worktrees.first().map(|w| w.path.clone()));
        // `open` says no when the repository is already there: reopening must
        // not duplicate its row. A folder that is back — remounted, recloned —
        // stops being missing at the same time.
        if !self.repos.open(main.clone(), name, worktrees) {
            return;
        }
        Settings::update_global(cx, |s| s.remember_repository(&main));
        self.forget_missing_worktrees(&main, cx);
        self.ensure_wt_project(&main);
        self.git.send(Cmd::LoadBranches { main });
        if self.active.is_none() {
            if let Some(path) = first {
                self.select_worktree(path, window, cx);
            }
        }
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
            self.toast = Some(Toast {
                text: SharedString::from(message),
                error: true,
            });
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
                self.terminals.remove(&active);
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
        let state = self.review.entry(worktree.clone()).or_default();
        state.status = status;
        if state.base.is_none() {
            state.base = base;
        }
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
        state.files.insert(range, files);
        if gone {
            state.selected = None;
            state.diff = None;
            state.diff_selection = None;
        }
        if pruned {
            self.persist_notes(&worktree, cx);
        }
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
        let split = Settings::global(cx).diff_split;
        let mut jumped = None;
        let mut note = None;
        if let Some(state) = self.review.get_mut(&worktree) {
            if state.selected.as_deref() == Some(path.as_path()) {
                let rendered = std::rc::Rc::new(Rendered::new(&path, diff, &theme));
                // The arrow that opened this file expects a change, not the top
                // of the file.
                if let Some(jump) = state.pending_jump.take() {
                    let headers = rendered.headers(split);
                    jumped = match jump {
                        Jump::First => headers.first().copied(),
                        Jump::Last => headers.last().copied(),
                    };
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
            self.reveal_diff_row(row, gpui::ScrollStrategy::Top, cx);
        }
    }

    fn history_arrived(
        &mut self,
        worktree: PathBuf,
        range: LogRange,
        commits: Vec<Commit>,
        graph: Vec<GraphRow>,
    ) {
        let Some(state) = self.review.get_mut(&worktree) else {
            return;
        };
        state.history_pending = false;
        // A late answer, for a range no longer being looked at, would replace
        // the history with the wrong one.
        if state.history_range == range {
            let width = crate::git::history::width(&graph);
            state.history = Some(std::rc::Rc::new(History {
                commits,
                graph,
                width,
            }));
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
        if action == Action::Commit {
            self.commit_input.update(cx, |input, cx| {
                input.set_value("", window, cx);
            });
        }
        let text = if output.trim().is_empty() {
            tr!(action.success_key())
        } else {
            SharedString::from(output.trim().to_string())
        };
        self.toast = Some(Toast { text, error: false });
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
    }

    fn action_failed(&mut self, worktree: Option<PathBuf>, action: Action, message: String) {
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
        self.toast = Some(Toast {
            text: SharedString::from(message),
            error: true,
        });
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
        let mut reviewed = Vec::new();
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
            state.notes = notes;
            state.reviewed = reviewed;
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
        // directory that says so, not ours.
        self.git.send(Cmd::OpenIfRepo(cwd));
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
        self.active = Some(path.clone());
        self.ensure_review(&path, cx);
        // The free note follows the displayed worktree: the input is unique, and
        // keeping the previous one's text would write it here.
        self.sync_journal_input(&path, window, cx);
        self.request_status(path);
        // Every worktree has its base: the selector has to show this one, not
        // the one of the worktree just left.
        self.refresh_base_choices(window, cx);
        cx.notify();
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
        // The previous diff is cleared at once: keeping another file's for the
        // length of the read would give the impression that the click did
        // nothing, and then that the content changes by itself.
        state.diff = None;
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
        self.git.send(Cmd::LoadFileDiff {
            worktree,
            range: range.clone(),
            path: path.clone(),
            context: Settings::global(cx).context_lines(),
            untracked,
        });
        // The list follows the open file: an arrow that changes file would
        // otherwise leave it out of view, and one would review without knowing
        // where one is. The scrolling is non-strict — an already-visible file
        // does not make the list jump under one's eyes.
        self.reveal_file(&range, &path, cx);
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
        let choices: Vec<BaseChoice> = repo.branches.iter().map(BaseChoice::of).collect();
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
        if let Some(state) = self.review.get_mut(&worktree) {
            state.history = None;
        }
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
        let (base, next_note) = (state.base.clone(), state.next_note);
        Store::update_global(cx, |store| {
            let saved = store.worktree_mut(worktree, &main);
            saved.base = base;
            saved.collapsed = collapsed;
            saved.next_note = next_note;
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

    /// Brings a diff entry into view.
    pub(super) fn reveal_diff_row(&self, row: usize, strategy: gpui::ScrollStrategy, cx: &App) {
        if self.diff_wrapped(cx) {
            self.diff_wrap_scroll.scroll_to_item(row, strategy);
        } else {
            self.diff_scroll.scroll_to_item(row, strategy);
        }
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

    /// The current review, if there is one.
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
        self.toast = Some(Toast { text, error: false });
        cx.notify();
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

    fn worktree_exists(&self, path: &Path) -> bool {
        self.repos.contains_worktree(path)
    }

    pub(super) fn first_worktree(&self) -> Option<PathBuf> {
        self.repos.first_worktree()
    }

    fn default_base_for(&self, worktree: &Path) -> Option<String> {
        self.repos.default_base_for(worktree)
    }

    // — Rendering ——————————————————————————————————————————————————

    fn render_topbar(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let worktree = self.active_worktree();
        let label = worktree
            .map(|w| w.label())
            .unwrap_or_else(|| tr!("no-worktree").to_string());

        // The top bar **is** the window's title bar.
        //
        // `TitleBar::title_bar_options()` asks the platform not to draw one: on
        // Windows the window therefore had nothing left to be moved, minimised
        // or closed by. One is needed, and stacking one above this would cost
        // thirty pixels to repeat what it already says — that is the reasoning
        // that moved the screen picker down into the status bar. `TitleBar`
        // brings the drag, the double click that maximises and the window
        // buttons; our actions live inside it. It keeps our height and our
        // colours, not its own.
        //
        // The buttons placed in the drag region stay clickable: the region is
        // returned as `HTCAPTION`, but gpui handles non-client mouse messages
        // and redistributes them. It is what Zed's title bar does, on the same
        // terms.
        TitleBar::new()
            .h(super::theme::toolbar_height(cx))
            .border_color(cx.theme().border)
            .bg(cx.theme().title_bar)
            .child(
                h_flex()
                    .w_full()
                    .h_full()
                    .pr_2()
                    .gap_2()
                    .items_center()
                    .child(self.render_main_menu(cx))
                    // The worktree, and nothing else: the branch and its
                    // divergence have moved down into the status bar, which
                    // carried only an occasional message while this one
                    // overflowed.
                    .child(div().font_semibold().text_sm().child(label))
                    .child(div().flex_1())
                    // Neither `fetch`, nor `pull`, nor `push`: they have moved
                    // down into the "Changes" panel's bar, where the rest of the
                    // gesture happens — tick, commit, push. The history and the
                    // branches, for their part, are dock tabs: one more button
                    // here would make two paths for the same gesture.
                    .child(
                        Button::new("terminal")
                            .ghost()
                            .small()
                            .icon(icon("square-terminal"))
                            .tooltip(tr!("panel-terminal"))
                            .selected(self.terminal_visible(cx))
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.toggle_terminal_panel(window, cx);
                            })),
                    ),
            )
    }

    /// The application's menu.
    ///
    /// A single entry point for what is not about the repository being looked at
    /// — settings, layout, quit — rather than buttons scattered through a
    /// toolbar that talks about the current worktree.
    fn render_main_menu(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let entity = cx.entity();
        Button::new("main-menu")
            .ghost()
            .small()
            .icon(icon("menu"))
            .tooltip(tr!("menu-title"))
            .dropdown_menu(move |menu, window, cx| {
                let entity = entity.clone();
                let for_reset = entity.clone();
                let for_shortcuts = entity.clone();
                let for_views = entity.clone();
                menu.item(PopupMenuItem::new(tr!("settings-title")).on_click(
                    move |_, window, cx| {
                        entity.update(cx, |this, cx| this.open_settings(window, cx));
                    },
                ))
                // The shortcuts are what one looks for when one no longer knows:
                // they therefore live where one goes looking, beside the
                // settings, and not in a help one would have to guess at.
                .item(
                    PopupMenuItem::new(tr!("shortcuts-title")).on_click(move |_, window, cx| {
                        for_shortcuts.update(cx, |this, cx| this.open_shortcuts(window, cx));
                    }),
                )
                // Hidden views have no tab left: this is the only place to call
                // them back from, and therefore the only place that says what the
                // window is not showing.
                // The views of **this screen**, and not the eleven of the window:
                // hiding the SQL console from the review would make nothing
                // visibly change, and an entry with no effect reads as a broken
                // entry.
                .submenu(tr!("menu-views"), window, cx, move |menu, _window, cx| {
                    let views = for_views.read(cx).workspace.views();
                    views.iter().fold(menu, |menu, &(name, title)| {
                        menu.item(view_toggle(for_views.clone(), name, title))
                    })
                })
                .item(PopupMenuItem::new(tr!("menu-reset-layout")).on_click(
                    move |_, window, cx| {
                        for_reset.update(cx, |this, cx| this.reset_layout(window, cx));
                    },
                ))
                .separator()
                .item(PopupMenuItem::new(tr!("menu-quit")).on_click(|_, _window, cx| cx.quit()))
            })
    }

    /// The status bar: where one is, and what has just happened.
    ///
    /// The branch and its divergence live there because they almost never change
    /// and have no business taking up the toolbar; the message, for its part, is
    /// occasional, and a bar carrying only it stays empty most of the time.
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

    fn render_status_bar(&mut self, cx: &mut Context<Self>) -> impl IntoElement {
        let (text, error) = match &self.toast {
            Some(t) => (t.text.clone(), t.error),
            None => (SharedString::default(), false),
        };
        let branch = self
            .active_worktree()
            .and_then(|w| w.branch.clone())
            .unwrap_or_else(|| tr!("branch-detached").to_string());
        let (ahead, behind) = self
            .active_review()
            .map(|r| (r.status.ahead, r.status.behind))
            .unwrap_or((0, 0));
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
            // The screen picker opens the bar: it is the broadest of the three
            // ways of saying where one is, and the other two — the branch, the
            // lead over the upstream — line up behind it.
            .child(self.render_workspace_nav(cx))
            .child(Divider::vertical().h(px(12.)))
            .when(self.active.is_some(), |el| {
                el.child(icon("git-branch").xsmall())
                    .child(div().max_w(px(220.)).truncate().child(branch))
                    // Behind before ahead: that is what has to be integrated
                    // before one can push.
                    .when(behind > 0, |el| el.child(format!("↓{behind}")))
                    .when(ahead > 0, |el| el.child(format!("↑{ahead}")))
                    .child(Divider::vertical().h(px(12.)))
            })
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
            .child(
                div()
                    .flex_1()
                    .truncate()
                    .when(error, |el| el.text_color(cx.theme().danger))
                    .child(text),
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
        v_flex()
            // Vim mode is read at render time and not at construction: the
            // context is what turns its bindings on, and the setting changes
            // along the way.
            .key_context(super::shortcuts::context(Settings::global(cx).vim_mode))
            .track_focus(&self.focus)
            .on_action(cx.listener(super::shortcuts::refresh))
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
            .on_action(cx.listener(super::shortcuts::close_find))
            .on_action(cx.listener(super::shortcuts::find_next))
            .on_action(cx.listener(super::shortcuts::find_previous))
            .on_action(cx.listener(super::shortcuts::explorer_up))
            .on_action(cx.listener(super::shortcuts::explorer_down))
            .on_action(cx.listener(super::shortcuts::explorer_left))
            .on_action(cx.listener(super::shortcuts::explorer_right))
            .on_action(cx.listener(super::shortcuts::explorer_open))
            .on_action(cx.listener(super::shortcuts::explorer_home))
            .on_action(cx.listener(super::shortcuts::explorer_end))
            .on_action(cx.listener(super::shortcuts::db_up))
            .on_action(cx.listener(super::shortcuts::db_down))
            .on_action(cx.listener(super::shortcuts::db_left))
            .on_action(cx.listener(super::shortcuts::db_right))
            .on_action(cx.listener(super::shortcuts::db_open))
            .on_action(cx.listener(super::shortcuts::run_db_query))
            .on_action(cx.listener(super::shortcuts::go_to_workspace))
            .on_action(cx.listener(super::shortcuts::copy_db_result))
            .on_action(cx.listener(super::shortcuts::select_whole_result))
            .on_action(cx.listener(super::shortcuts::export_db_csv))
            .on_action(cx.listener(super::shortcuts::show_shortcuts))
            .on_action(cx.listener(super::shortcuts::toggle_sidebar))
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
            .child(div().flex_1().min_h_0().p(px(4.)).child(self.dock.clone()))
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
        let on_ok = std::rc::Rc::new(on_ok);
        window.open_dialog(cx, move |dialog, _window, _cx| {
            let (input, entity, on_ok) = (input.clone(), entity.clone(), on_ok.clone());
            dialog
                .title(title.clone())
                .overlay_closable(false)
                .close_button(false)
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
    }
}

impl ClaudhubApp {
    pub(super) fn active_path(&self) -> Option<PathBuf> {
        self.active.clone()
    }

    /// Gives the current screen back its initial layout.
    ///
    /// The escape hatch of a system where everything can be moved: a panel
    /// dragged out of view has no other way back.
    ///
    /// **This one and not all four**: one breaks a screen's geometry by pulling
    /// a separator crooked, not everyone's, and taking the other three along
    /// would make a repair gesture expensive.
    pub(super) fn reset_layout(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let this = cx.entity();
        let workspace = self.workspace;
        self.dock.update(cx, |area, cx| {
            crate::ui::workspace::install_default_layout(workspace, &this, area, window, cx);
        });
        self.schedule_layout_save(cx);
        cx.notify();
    }

    /// Switches to another screen.
    ///
    /// Nothing to do but change dock: the state — the chosen worktree, the open
    /// file, the running query — lives in this entity and not in the panels, and
    /// is therefore the same on all four sides.
    pub(super) fn enter_workspace(
        &mut self,
        workspace: crate::ui::workspace::Workspace,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.workspace == workspace {
            return;
        }
        self.workspace = workspace;
        self.dock = self.docks[&workspace].clone();
        cx.notify();
    }

    pub(super) fn terminal_visible(&self, _cx: &App) -> bool {
        self.panel_visible(super::panels::TerminalPanel::NAME)
    }

    pub(super) fn show_terminal_panel(&mut self, _window: &mut Window, cx: &mut Context<Self>) {
        self.set_panel_visible(super::panels::TerminalPanel::NAME, true, cx);
    }

    /// A view is visible until it has been hidden.
    ///
    /// The question is asked in the negative: a panel just added shows up without
    /// anything having to declare it, and a name unknown to the settings file — a
    /// renamed panel — no longer hides anything.
    pub(super) fn panel_visible(&self, name: &str) -> bool {
        !self.hidden_panels.contains(name)
    }

    pub(super) fn set_panel_visible(&mut self, name: &str, visible: bool, cx: &mut Context<Self>) {
        let changed = if visible {
            self.hidden_panels.remove(name)
        } else {
            self.hidden_panels.insert(name.to_string())
        };
        if !changed {
            return;
        }
        // Sorted before being written, like the store's collapses: a set
        // serialised in a different order every time would make a settings file
        // that changes with nothing having changed.
        let mut hidden: Vec<String> = self.hidden_panels.iter().cloned().collect();
        hidden.sort();
        Settings::update_global(cx, |s| s.hidden_panels = hidden);
        cx.notify();
    }

    pub(super) fn toggle_panel(&mut self, name: &str, cx: &mut Context<Self>) {
        let visible = self.panel_visible(name);
        self.set_panel_visible(name, !visible, cx);
    }

    /// The area keyboard zoom aims at.
    ///
    /// The focus decides: a terminal being looked at enlarges itself, and
    /// everywhere else it is the reviewed code one wants larger. Asking the user
    /// to name an area before zooming would be one more gesture for an intention
    /// that is never ambiguous.
    fn zoom_zone(&self, window: &Window, cx: &App) -> crate::ui::settings::Zoom {
        let terminal_focused = self
            .active
            .as_ref()
            .and_then(|worktree| self.terminals.get(worktree))
            .is_some_and(|group| group.read(cx).is_focused(window, cx));
        if terminal_focused {
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

    pub(super) fn toggle_terminal_panel(&mut self, _window: &mut Window, cx: &mut Context<Self>) {
        self.toggle_panel(super::panels::TerminalPanel::NAME, cx);
    }

    /// Shows or hides the left zone — repositories, branches, files.
    ///
    /// `toggle_dock` only notifies the inner dock, and it is the area we observe
    /// to save the layout: without this `notify`, the window would reopen with
    /// the zone in the state from before the gesture.
    pub(super) fn toggle_sidebar(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.dock.update(cx, |area, cx| {
            area.toggle_dock(gpui_component::dock::DockPlacement::Left, window, cx);
            cx.notify();
        });
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
