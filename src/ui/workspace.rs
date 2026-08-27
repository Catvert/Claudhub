//! The sub-applications.
//!
//! Claudhub does four jobs with almost nothing in common: reviewing a diff,
//! touching up a file, querying a database, working through an error. As long
//! as they shared one window, each paid for the other three's room — eight tabs
//! in the centre of which only two are ever looked at, and a central panel that
//! changed nature according to the last gesture. The settings make a fifth
//! screen, the only one that is not work: they were a modal window, which is
//! what one reaches for when there is nowhere to put a form.
//!
//! Each screen therefore has **its own dock**, with its panels, its tabs and
//! its sizes, remembered separately. You move between them through the bottom
//! bar; tuning the review no longer moves anything on the databases screen.
//!
//! **Two views are everywhere**: the repositories and the terminals. The first
//! says *where* you work — the choice holds across every screen — the
//! second is what you talk to while looking at any of them. They are therefore
//! the only two panels instantiated once per dock.
//!
//! The central panel, for its part, **stops being shared**: the diff belongs to
//! the review, the editor to editing, the SQL console to the databases. That is
//! the most visible thing the split buys — a tab whose title changed from
//! "Diff" to "Editor" to "SQL" depending on what you had just done was saying
//! plainly that it carried three things.
//!
//! **And one screen puts them back together**, which is not a retraction: the
//! split is right for the hour spent on one job, and wrong for the half hour
//! spent on all four — read the diff, fix the file it named, ask the database
//! what the fix claims. `Workspace::Home` carries every other screen's panels,
//! its columns as tabs of one sidebar and its centres as tabs of one group, and
//! what opens there stays there. See the variant, and `ClaudhubApp::reveal`.

use gpui::{prelude::*, px, App, Context, Entity, Window};
use gpui_component::{
    button::{Button, ButtonGroup, ButtonVariants as _},
    dock::{DockArea, DockLayout, DockPlacement},
};

use crate::plugin::manifest;
use crate::tr;
use crate::ui::app::ClaudhubApp;
use crate::ui::icons::icon;
use crate::ui::panels;

/// The initial width of the left column.
const SIDEBAR_WIDTH: gpui::Pixels = px(280.);

/// The same, for the home screen: its column carries the review's file list,
/// which is the one panel of the lot that is read across rather than down.
const HOME_SIDEBAR_WIDTH: gpui::Pixels = px(360.);

/// The initial width of the column saying what to review, left of the diff.
const REVIEW_LIST_WIDTH: gpui::Pixels = px(420.);

/// A screen, and the order in which the bar offers them.
///
/// The order is not arbitrary: it is the order of the work. Home is where one
/// lands, then you review, you fix what you read, you check in the database
/// what the code claims, and Sentry is the starting point on days when you did
/// not choose your subject. The settings come last, being the only one that is
/// not work.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum Workspace {
    /// Every other screen at once: their columns as tabs of one sidebar, their
    /// centres as tabs of one group.
    ///
    /// **The screen that undoes the split**, and it is not a contradiction of
    /// what the rest of this module says: the split exists because four jobs
    /// have almost nothing in common, and this is for the day one is doing all
    /// four in the same half hour — read the diff, fix the file it named, ask
    /// the database what the fix claims. Moving between them by the bar costs a
    /// key and loses the arrangement of the previous screen; here they are
    /// tabs, which is the gesture PhpStorm makes and the reason its users never
    /// leave one window.
    ///
    /// **Its centre is where the others' centres go.** Opening a file, a diff,
    /// a query while standing here does not leave for the screen that owns it:
    /// it brings that screen's tab forward — see `ClaudhubApp::reveal`. Which
    /// tab is `centre_tab`.
    ///
    /// It costs nothing the window did not already build: a screen is a dock,
    /// the panels are the same types instantiated once more, and the state they
    /// draw lives in `ClaudhubApp` and not in them.
    #[default]
    Home,
    Git,
    Files,
    /// Searching the whole project — the magnifier. See `ui::search_view`.
    ///
    /// A screen and not a panel dropped into the editing one: a search covers
    /// the whole checkout while an editor holds one file, the answer is a list
    /// one walks for minutes, and it wants the width the tree and the editor
    /// already share. Its two halves — the results, the file under the cursor —
    /// are read together, which is what makes them two panels side by side
    /// rather than two tabs.
    Search,
    Db,
    Sentry,
    /// Every terminal of the window at once, in a grid grouped by worktree.
    ///
    /// **A screen and not a panel**, and for the reason every other screen
    /// exists: what one comes here for is the whole window's terminals, and no
    /// panel can show them — a terminal is a dock panel, a panel belongs to one
    /// dock area at a time, and the dock only ever shows the worktree being
    /// looked at. The grid is the one view that crosses the worktrees, which is
    /// exactly the question it answers: which of the agents I left running has
    /// finished.
    ///
    /// **It is not in the screen picker**, for the same reason the settings are
    /// not: one does not come here to work, one comes to look. It has its own
    /// button beside the gear, at the far right of the title bar.
    Multiplexer,
    /// The settings, and the log they let you read.
    ///
    /// **A screen and not a dialog.** They were a modal window, which is what
    /// one reaches for when there is nowhere to put a form: it covered what you
    /// were adjusting, it could not be left open while you looked at the effect,
    /// and the two things one comes here for — trying a theme, reading why
    /// something failed — are exactly the two that want the rest of the window
    /// still visible. A screen costs nothing that was not already built: the
    /// bar was there, the dock knew how to carry a panel.
    Settings,
}

impl Workspace {
    /// Its rank in `ALL`, which is what `Alt+1`… names — and the index of a
    /// terminal's face for that screen, `OpenTerminal::panels` being in the
    /// same order.
    pub fn index(self) -> usize {
        Self::ALL.iter().position(|w| *w == self).unwrap_or(0)
    }

    pub const ALL: [Workspace; 8] = [
        // **First, and that is what `Alt+1` names.** The home screen is where
        // one lands and where one comes back to, and a rank in this array is
        // the key that reaches it.
        Workspace::Home,
        Workspace::Git,
        Workspace::Files,
        Workspace::Search,
        Workspace::Db,
        Workspace::Sentry,
        Workspace::Settings,
        // **Last, and it is not an aesthetic choice**: `Alt+1` to `Alt+7` are
        // ranks in this array, and a screen inserted in the middle would move
        // every key after it. See `shortcuts::go_to_workspace`.
        Workspace::Multiplexer,
    ];

    /// The two screens one does not **work** in, in the order they sit at the
    /// right of the title bar.
    ///
    /// The multiplexer is where one goes to see what is running everywhere at
    /// once before leaving for the worktree it pointed at, and the settings are
    /// where one changes how the rest behaves. They are a button group of their
    /// own there rather than a seventh and eighth button in the picker, which is
    /// the row of the work — see `topbar::render_topbar`. `working` is `ALL`
    /// less these two, so the two lists cannot drift apart.
    pub const ASIDE: [Workspace; 2] = [Workspace::Multiplexer, Workspace::Settings];

    /// The screens the home screen gathers, in the order their tabs sit.
    ///
    /// `working()` less the home screen itself, written out rather than
    /// derived: `working` filters on what a plugin puts where, and the layout
    /// installed once at startup must not depend on that — a screen dropped
    /// from the bar because no plugin landed on it still has panels of ours to
    /// contribute here. Sentry is in it for the opposite reason: it has none,
    /// and it is by this list that its plugins reach the home screen.
    pub const AT_HOME: [Workspace; 5] = [
        Workspace::Git,
        Workspace::Files,
        Workspace::Search,
        Workspace::Db,
        Workspace::Sentry,
    ];

    /// The screens that read files, in the order `Editing::panels` holds a
    /// file's tabs.
    ///
    /// A file is a **panel**, and a panel belongs to one dock area at a time:
    /// a file open in two centres is two panels rendering one editor state, the
    /// arrangement a terminal already has. Two and not eight — the other six
    /// have nowhere to put a file.
    ///
    /// The home screen comes first so that the order of this list is the order
    /// of `ALL`, which is the one every other per-screen list is read in.
    pub const WITH_EDITOR: [Workspace; 2] = [Workspace::Home, Workspace::Files];

    /// A screen's rank in `WITH_EDITOR` — the index of its face in
    /// `Editing::panels`. `None` for a screen that reads no file.
    pub fn editor_index(self) -> Option<usize> {
        Self::WITH_EDITOR.iter().position(|w| *w == self)
    }

    /// The panel this screen is reached by when its centre is a tab of the home
    /// screen — what `reveal` brings forward instead of changing screen.
    ///
    /// `None` for a screen that has no centre of ours: Sentry's is whatever
    /// plugin landed on it, and the two aside screens are not folded in at all.
    pub fn centre_tab(self) -> Option<&'static str> {
        use panels::*;
        match self {
            Self::Git => Some(DiffPanel::NAME),
            Self::Files => Some(EditorPanel::NAME),
            Self::Search => Some(SearchPreviewPanel::NAME),
            Self::Db => Some(ConsolePanel::NAME),
            Self::Home | Self::Sentry | Self::Settings | Self::Multiplexer => None,
        }
    }

    /// Whether standing on the home screen answers a gesture that asks for this
    /// screen — see `ClaudhubApp::reveal`.
    pub fn folds_into_home(self) -> bool {
        Self::AT_HOME.contains(&self)
    }

    /// The screens the bar offers, in order.
    ///
    /// The settings are not among them — one does not go there to work — and
    /// neither is a screen with **nothing to show**: since Sentry became a
    /// plugin, its screen carries no panel of ours, and a button opening an
    /// empty room is worse than no button. See `plugin_view::screen_has_content`.
    ///
    /// An iterator and not a list: the bar builds this on every frame, and
    /// nothing here needs the seven of them at once.
    pub fn working(cx: &App) -> impl Iterator<Item = Workspace> + '_ {
        Self::ALL
            .into_iter()
            .filter(|w| !Self::ASIDE.contains(w))
            .filter(|w| crate::ui::plugin_view::screen_has_content(*w, cx))
    }

    /// Does this screen hold panels of its own, or only whatever plugins land
    /// on it.
    ///
    /// Only Sentry's does not, and that is not an accident of naming: it is
    /// where the plugins that watch what happens **outside** the repository go,
    /// and everything it used to carry itself is now one of them.
    pub fn carries_own_panels(self) -> bool {
        !matches!(self, Self::Sentry)
    }

    /// Whether this screen shows the terminals of **every** worktree.
    ///
    /// Only the multiplexer, and it is the whole of what makes that screen: its
    /// dock holds the same panels as any other, but their faces there are
    /// always visible instead of following the worktree being looked at, and
    /// their tabs say which project they belong to — the one dock that mixes
    /// them being the one place a tab has to.
    pub fn shows_every_worktree(self) -> bool {
        matches!(self, Self::Multiplexer)
    }

    /// The name this screen's layout is saved under.
    ///
    /// A stable key and not the variant's index: inserting a screen in the
    /// middle would otherwise read back the neighbour's layout.
    pub fn key(self) -> &'static str {
        match self {
            // "review" and not "git": the key is what a saved layout is read
            // back by, and renaming the screen must not lose where its panels
            // were. That is the whole point of it being a name of its own.
            Self::Home => "home",
            Self::Git => "review",
            Self::Files => "files",
            Self::Search => "search",
            Self::Db => "db",
            Self::Sentry => "sentry",
            Self::Settings => "settings",
            Self::Multiplexer => "multiplexer",
        }
    }

    pub fn from_key(key: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|w| w.key() == key)
    }

    /// The i18n key of the name, the one in the tooltip.
    pub fn label(self) -> &'static str {
        match self {
            Self::Home => "workspace-home",
            Self::Git => "workspace-git",
            Self::Files => "workspace-files",
            Self::Search => "workspace-search",
            Self::Db => "workspace-db",
            Self::Sentry => "workspace-sentry",
            Self::Settings => "workspace-settings",
            Self::Multiplexer => "workspace-multiplexer",
        }
    }

    /// The bar's icon. It says what the screen **contains**, not what it is
    /// called: it is what you aim at, the name only comes in the tooltip.
    pub fn icon(self) -> &'static str {
        match self {
            Self::Home => "layout-dashboard",
            Self::Git => "file-diff",
            Self::Files => "file-code",
            Self::Search => "search",
            Self::Db => "database",
            Self::Sentry => "triangle-alert",
            Self::Settings => "settings",
            Self::Multiplexer => "grid-3x3",
        }
    }

    /// This screen's dock id.
    ///
    /// Distinct per screen: the areas coexist, and two sharing an id would
    /// share the state gpui files under it.
    pub fn dock_id(self) -> String {
        format!("claudhub-{}", self.key())
    }

    /// The views the "Views" menu offers to hide on this screen.
    ///
    /// **Per screen, and not the whole list.** Hiding "SQL console" from the
    /// review would make nothing visibly change, and an entry that does nothing
    /// reads as a broken entry. The terminals are at the end of each: they are
    /// everywhere.
    pub fn views(self) -> &'static [(&'static str, &'static str)] {
        use panels::*;
        match self {
            // Every other screen's, in one list: this screen carries every one
            // of those panels, and an entry missing here would be a view with
            // no way back once hidden — the `…` menu is what hides it, and the
            // main menu is the only place it is offered again.
            Self::Home => &[
                (ChangesPanel::NAME, "range-working"),
                (BranchPanel::NAME, "range-branch"),
                (FilesPanel::NAME, "panel-files"),
                (SearchPanel::NAME, "panel-search"),
                (DbPanel::NAME, "panel-databases"),
                (NotesPanel::NAME, "panel-notes"),
                (HistoryPanel::NAME, "panel-history"),
                (TagsPanel::NAME, "panel-tags"),
                (StashesPanel::NAME, "panel-stashes"),
                (TestsPanel::NAME, "panel-tests"),
                (SqlHistoryPanel::NAME, "panel-sql-history"),
                (DiffPanel::NAME, "panel-diff"),
                (EditorPanel::NAME, "panel-editor"),
                (SearchPreviewPanel::NAME, "panel-search-preview"),
                (ConsolePanel::NAME, "panel-sql"),
                (TerminalPanel::NAME, "panel-terminal"),
            ],
            Self::Git => &[
                (NotesPanel::NAME, "panel-notes"),
                (ChangesPanel::NAME, "range-working"),
                (BranchPanel::NAME, "range-branch"),
                (HistoryPanel::NAME, "panel-history"),
                (TagsPanel::NAME, "panel-tags"),
                (StashesPanel::NAME, "panel-stashes"),
                (TestsPanel::NAME, "panel-tests"),
                (DiffPanel::NAME, "panel-diff"),
                (TerminalPanel::NAME, "panel-terminal"),
            ],
            Self::Files => &[
                (FilesPanel::NAME, "panel-files"),
                (TestsPanel::NAME, "panel-tests"),
                (EditorPanel::NAME, "panel-editor"),
                (TerminalPanel::NAME, "panel-terminal"),
            ],
            Self::Search => &[
                (SearchPanel::NAME, "panel-search"),
                (SearchPreviewPanel::NAME, "panel-search-preview"),
                (TerminalPanel::NAME, "panel-terminal"),
            ],
            Self::Db => &[
                (DbPanel::NAME, "panel-databases"),
                (SqlHistoryPanel::NAME, "panel-sql-history"),
                (ConsolePanel::NAME, "panel-sql"),
                (TerminalPanel::NAME, "panel-terminal"),
            ],
            Self::Sentry => &[(TerminalPanel::NAME, "panel-terminal")],
            // The grid holds the terminals itself: there is no terminal panel
            // here to offer hiding, and hiding the grid would leave a screen
            // with nothing on it.
            Self::Multiplexer => &[],
            // The settings themselves are not offered: this screen holds them
            // and nothing else, and hiding them would leave a screen the bar
            // still points at with nothing on it.
            Self::Settings => &[(TerminalPanel::NAME, "panel-terminal")],
        }
    }
}

/// A screen's panels, made for it.
///
/// `BasePanelView` and not `PanelView`: that is the type `panel_handle`
/// returns, and it is **the** one needed. `Entity<P>` converts itself into
/// `Arc<dyn BasePanelView>` — and the dock takes it without complaint, but
/// without the presentation that goes with it: no tab, no title, no content.
/// That is the silent failure of the dock rework, and the only thing
/// `panel_handle` prevents.
type View = std::sync::Arc<dyn gpui_component::dock::BasePanelView>;

/// The terminals are **no longer part of the default layout**.
///
/// They used to be one permanent panel per screen, placed here under the
/// centre. Each terminal is a panel of its own now — that is what lets it be
/// dragged into a split — and there is none to place until one is opened:
/// `ClaudhubApp::open_terminal` puts the first one in exactly this slot, under
/// the whole centre, and the ones after it in its tab group.
/// Builds the panels a screen needs, and installs its initial layout.
///
/// Each screen has **its** instances, including of the two shared views: a
/// panel belongs to only one dock at a time, and only one dock is displayed.
/// A screen's plugin panels for one place, in manifest order.
///
/// They join the screen's **existing tab groups** rather than places of their
/// own: a plugin's panel is one more way of looking at the worktree, and a
/// column reserved for it would cost the review its width whether a plugin is
/// installed or not. What `place` chooses is which of the two groups — the list
/// column or the wide half — and that is what lets one plugin lay a list on the
/// left and what one picked from it in the centre.
fn plugin_views(
    workspace: Workspace,
    place: manifest::Place,
    app: &Entity<ClaudhubApp>,
    cx: &mut Context<DockArea>,
) -> Vec<View> {
    // The home screen holds every other one's panels, plugins included: it
    // reads their screens' keys and not its own, which no plugin can name —
    // `manifest::SCREENS` does not offer it, a plugin choosing "home" would be
    // choosing to appear twice.
    let screens: &[Workspace] = if workspace == Workspace::Home {
        &Workspace::AT_HOME
    } else {
        std::slice::from_ref(&workspace)
    };
    screens
        .iter()
        .flat_map(|screen| crate::ui::plugin_view::on_screen(screen.key()))
        .flat_map(|found| found.panels.iter())
        .filter(|spec| spec.place == place)
        .map(|spec| {
            let panel = spec.name;
            gpui_component::dock::panel_handle(
                cx.new(|cx| panels::PluginPanel::new(app, panel, cx)),
            ) as View
        })
        .collect()
}

pub fn install_default_layout(
    workspace: Workspace,
    app: &Entity<ClaudhubApp>,
    area: &mut DockArea,
    window: &mut Window,
    cx: &mut Context<DockArea>,
) {
    // The multiplexer holds nothing but the terminals, which dock themselves as
    // they open: there is no default layout to install, and an empty centre is
    // what a fresh area already is. It is also why that screen has no chrome of
    // its own — no panel, so no tab bar above the terminals' own, and no title
    // saying what the six other screens' button already said.
    if workspace == Workspace::Multiplexer {
        return;
    }
    // **The three docks go first, whatever they hold.** `set_dock` keeps the
    // size and open state of the dock it refills, and a dock the default layout
    // does not name is not touched at all: a left column dragged to a sliver and
    // then collapsed came back collapsed and a sliver, and a right or bottom
    // dock one had opened by hand kept its width — the reset repaired the
    // centre and left the rest as it was. Removed, they are rebuilt below with
    // a fresh `Dock`: open, at the default size.
    for placement in [
        DockPlacement::Left,
        DockPlacement::Right,
        DockPlacement::Bottom,
    ] {
        area.remove_dock(placement, window, cx);
    }
    use gpui_component::dock::panel_handle;
    macro_rules! panel {
        ($name:ident) => {
            panel_handle(cx.new(|cx| panels::$name::new(app, cx))) as View
        };
    }

    // The **fixed** half of a split is the bottom one, never the top: the dock
    // area is smaller than the window — bars, padding, gutters — and two fixed
    // sizes adding up to the window height overflow.
    let third = window.viewport_size().height.max(px(600.)) / 3.;
    // The width **of the centre** and not of the window: a split's sizes are
    // shared out in proportion to their sum, and counting the left column in
    // would give the diff a share it never asked for.
    let width = (window.viewport_size().width - SIDEBAR_WIDTH).max(px(600.));

    // `Option`, because three screens have no left column at all: the review
    // and Sentry fill the width, and the settings' form has a sidebar of pages
    // of its own.
    // The plugin panels, split by where their manifest puts them: the list
    // column, or the wide half beside the diff and the editor.
    let mut listed = plugin_views(workspace, manifest::Place::Left, app, cx);
    let mut centred = plugin_views(workspace, manifest::Place::Centre, app, cx);
    macro_rules! with {
        ($views:expr, $group:expr) => {{
            let mut group = $group;
            for view in std::mem::take(&mut $views) {
                group = group.panel_view(view, cx);
            }
            group
        }};
    }

    let (left, center): (Option<DockLayout>, DockLayout) = match workspace {
        // Every other screen at once, and the shape is the one PhpStorm has:
        // **one column of tool tabs, one group of document tabs.**
        //
        // The column is split like the review's and the databases' are, and for
        // the reason both give: the top half **chooses** what one works on —
        // the changed files, the branch, the tree, the hits, the schema — and
        // the bottom half says **where one stands** — the notes, what happened,
        // what was already asked. Read together and not instead of each other,
        // which is what a tab would have made of them.
        //
        // The centre holds the four centres, and that is the whole point of the
        // screen: a file opened from the tree, a table's console, a hit one
        // jumped to all land in the group already on screen instead of taking
        // the window to another one. Open files join it as their own tabs, as
        // they do on the editing screen — see `explorer::open_file_tab`.
        Workspace::Home => {
            let left = DockLayout::v_split()
                .child(
                    DockLayout::tabs()
                        .panel_view(panel!(ChangesPanel), cx)
                        .panel_view(panel!(BranchPanel), cx)
                        .panel_view(panel!(ConflictsPanel), cx)
                        .panel_view(panel!(FilesPanel), cx)
                        .panel_view(panel!(SearchPanel), cx)
                        .panel_view(panel!(DbPanel), cx),
                    None,
                )
                .child(
                    with!(
                        listed,
                        DockLayout::tabs()
                            .panel_view(panel!(NotesPanel), cx)
                            .panel_view(panel!(HistoryPanel), cx)
                            .panel_view(panel!(TagsPanel), cx)
                            .panel_view(panel!(StashesPanel), cx)
                            .panel_view(panel!(TestsPanel), cx)
                            .panel_view(panel!(SqlHistoryPanel), cx)
                    ),
                    Some(third),
                );
            let center = with!(
                centred,
                DockLayout::tabs()
                    .panel_view(panel!(DiffPanel), cx)
                    .panel_view(panel!(EditorPanel), cx)
                    .panel_view(panel!(SearchPreviewPanel), cx)
                    .panel_view(panel!(ConsolePanel), cx)
            );
            (Some(left), center)
        }
        // The review takes the whole width: the ways of choosing what to review
        // — what changes now, what the branch wrote, what is already committed —
        // are tabs and not panels side by side, since they answer the same
        // question.
        //
        // **The notes are not one of those tabs but the column's bottom third.**
        // They do not say what there is to read, they say where one stands —
        // what is left to do, what one had to say — and that is read *while*
        // choosing a file, not instead of. A tab hid it behind a click, which
        // is exactly how a to-do list stops being kept.
        Workspace::Git => {
            let list = DockLayout::v_split()
                .child(
                    DockLayout::tabs()
                        .panel_view(panel!(ChangesPanel), cx)
                        .panel_view(panel!(BranchPanel), cx)
                        // Hidden while there is nothing to resolve: a permanent
                        // tab would shift the others aside to serve one time in
                        // a hundred.
                        .panel_view(panel!(ConflictsPanel), cx),
                    None,
                )
                // The bottom third holds what one reads *while* choosing a
                // file: where one stands, and where the branch comes from.
                // The history and the tags joined the notes there rather than
                // staying above — they do not answer "what is there to read",
                // they answer "what happened", and pushed aside the two tabs
                // that do choose the file.
                //
                // **This screen's plugins land here too**, and not in the group
                // above: a plugin asking for the list column is asking to sit
                // beside what one reads, and the shipped one — the CI, whose
                // runs answer "what happened to this branch" — is the whole
                // point. The three tabs above choose the file being reviewed,
                // and nothing else belongs among them.
                .child(
                    with!(
                        listed,
                        DockLayout::tabs()
                            .panel_view(panel!(NotesPanel), cx)
                            .panel_view(panel!(HistoryPanel), cx)
                            // Beside the history, and that is where it belongs: a
                            // tag names a commit, and the gesture one makes after
                            // finding the commit worth marking is right there.
                            .panel_view(panel!(TagsPanel), cx)
                            // And the stashes beside them: the three answer
                            // "what happened", where the tabs above choose the
                            // file being read.
                            .panel_view(panel!(StashesPanel), cx)
                            // The tests too — "does it pass" is read while
                            // choosing what to review, not instead of. The tab
                            // only shows on a Pest project.
                            .panel_view(panel!(TestsPanel), cx)
                    ),
                    Some(third),
                );
            let center = DockLayout::h_split()
                .child(list, Some(REVIEW_LIST_WIDTH))
                .child(
                    with!(
                        centred,
                        DockLayout::tabs().panel_view(panel!(DiffPanel), cx)
                    ),
                    Some(width - REVIEW_LIST_WIDTH),
                );
            (None, center)
        }
        // Editing: the project tree on the left, the editor in the centre.
        Workspace::Files => {
            let left = with!(
                listed,
                DockLayout::tabs()
                    .panel_view(panel!(FilesPanel), cx)
                    // Run the test of what one is writing without leaving the
                    // screen; hidden wherever there is no Pest suite.
                    .panel_view(panel!(TestsPanel), cx)
            );
            let center = with!(
                centred,
                DockLayout::tabs().panel_view(panel!(EditorPanel), cx)
            );
            (Some(left), center)
        }
        // The search: the results on the left, the file under the cursor in
        // the centre. The same shape as the editing screen, and for the same
        // reason — one walks a list on one side and reads on the other.
        Workspace::Search => {
            let left = with!(
                listed,
                DockLayout::tabs().panel_view(panel!(SearchPanel), cx)
            );
            let center = with!(
                centred,
                DockLayout::tabs().panel_view(panel!(SearchPreviewPanel), cx)
            );
            (Some(left), center)
        }
        // The databases: the schema tree on the left, the console in the
        // centre. This is PhpStorm's explorer, and the gesture is the same —
        // you unfold what you are looking for, you query what you have found.
        Workspace::Db => {
            // **The history is the column's bottom third**, the way the notes
            // are on the review screen, and for the same reason: it does not
            // say what there is to query, it says where one stands — what has
            // already been asked, and what it answered — and that is read
            // *while* writing the next query rather than instead of. Behind a
            // tab it was one click away from the schema, so one reached for it
            // only after having retyped what was already in it.
            //
            // A row and not a second column: the console keeps the whole width
            // it had, which was the reason the tab was chosen in the first
            // place.
            let left = DockLayout::v_split()
                .child(
                    with!(listed, DockLayout::tabs().panel_view(panel!(DbPanel), cx)),
                    None,
                )
                .child(
                    DockLayout::tabs().panel_view(panel!(SqlHistoryPanel), cx),
                    Some(third),
                );
            let center = with!(
                centred,
                DockLayout::tabs().panel_view(panel!(ConsolePanel), cx)
            );
            (Some(left), center)
        }
        // Sentry's screen carries no panel of ours any more: Sentry is a
        // plugin, and this screen is where the plugins that watch what happens
        // outside the repository land. Empty when none does — a screen the bar
        // still points at with nothing on it, which is the honest picture.
        // This screen has no column of ours to join, so a plugin that asks for
        // one **gets one**: that is how a master/detail plugin lays its list on
        // the left of a screen it is alone on. No plugin asks, no column — a
        // sidebar that is only ever empty would be a sidebar to hide.
        Workspace::Sentry => {
            let left = (!listed.is_empty()).then(|| with!(listed, DockLayout::tabs()));
            let center = with!(centred, DockLayout::tabs());
            (left, center)
        }
        // The settings take the whole width: the form has a sidebar of its own,
        // and two side by side would be two lists of pages to read before
        // finding the field. The terminals stay underneath — a setting is
        // adjusted then checked, and what checks it is a shell.
        // No plugin lands here: `manifest::SCREENS` does not offer the
        // settings, which hold the form and nothing else.
        // Returned above; the arm exists because the match is exhaustive.
        Workspace::Multiplexer => return,
        Workspace::Settings => {
            let _ = (&listed, &centred);
            let center = DockLayout::tabs().panel_view(panel!(SettingsPanel), cx);
            (None, center)
        }
    };

    area.set_center(center, window, cx);
    if let Some(left) = left {
        let sidebar = match workspace {
            Workspace::Home => HOME_SIDEBAR_WIDTH,
            _ => SIDEBAR_WIDTH,
        };
        area.set_dock(DockPlacement::Left, left, window, cx);
        area.set_dock_size(DockPlacement::Left, sidebar, window, cx);
    }
}

impl ClaudhubApp {
    /// The screen picker, at the left of the status bar.
    ///
    /// **In the status bar and not in a bar of its own.** The two followed each
    /// other, thirty pixels tall between them to carry a handful of buttons and a branch
    /// name — two grey bands stacked under the window, where the dock fights for
    /// every line. They say the same thing anyway: *where* you are. The branch
    /// name, how far ahead of the upstream you are and the screen you are
    /// looking at are three ways of answering, and they read at a single glance
    /// when they are on the same line.
    ///
    /// It is painted by the **root view** and not by the repositories panel: a
    /// panel gets dragged elsewhere and hidden, and navigation cannot leave with
    /// it.
    pub(super) fn render_workspace_nav(&mut self, cx: &mut Context<Self>) -> impl IntoElement {
        let current = self.workspace;
        let shown: Vec<Workspace> = Workspace::working(cx).collect();
        ButtonGroup::new("workspace-nav")
            .compact()
            .children(shown.iter().map(|workspace| {
                let workspace = *workspace;
                let here = workspace == current;
                Button::new(("workspace", workspace as usize))
                    .icon(icon(workspace.icon()))
                    // **The name is written, not hovered.** The icon says what
                    // the screen holds and is what one aims at, but six icons in
                    // a row is a rebus one learns rather than reads, and a
                    // tooltip is a name one has to ask for. The bar has the
                    // width: it sits at the end of a status line that is empty
                    // most of the time.
                    .label(tr!(workspace.label()))
                    // **Solid against outline**, and not the "selected" state of
                    // a whole outlined group: that is only a slightly lighter
                    // background, invisible on half the themes. It is the same
                    // observation as for a connection's engine picker, and
                    // "where am I" is exactly the question this bar has to
                    // answer without being looked for.
                    .map(|button| {
                        if here {
                            button.primary()
                        } else {
                            button.outline()
                        }
                    })
            }))
            .on_click(cx.listener(move |this, selected: &Vec<usize>, window, cx| {
                let Some(index) = selected.first() else {
                    return;
                };
                // The index is into what the group **shows**, which is not
                // `ALL`: the settings are not in it, and reading `ALL` here
                // would open the screen next door.
                let Some(workspace) = shown.get(*index).copied() else {
                    return;
                };
                // **The step is written down**: this bar is where one leaves a
                // screen for another, and `Ctrl+O` — or the back button — is
                // how one comes back to what one was doing there.
                this.travel_to(workspace, window, cx);
            }))
    }
}
