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

use gpui::{prelude::*, px, Context, Entity, Window};
use gpui_component::{
    button::{Button, ButtonGroup, ButtonVariants as _},
    dock::{DockArea, DockLayout, DockPlacement},
};

use crate::tr;
use crate::ui::app::ClaudhubApp;
use crate::ui::icons::icon;
use crate::ui::panels;

/// The initial width of the left column.
const SIDEBAR_WIDTH: gpui::Pixels = px(280.);

/// The initial width of the column saying what to review, left of the diff.
const REVIEW_LIST_WIDTH: gpui::Pixels = px(420.);

/// A screen, and the order in which the bar offers them.
///
/// The order is not arbitrary: it is the order of the work. You review, you fix
/// what you read, you check in the database what the code claims, and Sentry is
/// the starting point on days when you did not choose your subject. The
/// settings come last, being the only one that is not work.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum Workspace {
    #[default]
    Review,
    Files,
    Db,
    Sentry,
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
    /// Its rank in `ALL`, which is how a terminal's five panels are indexed —
    /// one per screen, in this order.
    pub fn index(self) -> usize {
        Self::ALL.iter().position(|w| *w == self).unwrap_or(0)
    }

    pub const ALL: [Workspace; 5] = [
        Workspace::Review,
        Workspace::Files,
        Workspace::Db,
        Workspace::Sentry,
        Workspace::Settings,
    ];

    /// The name this screen's layout is saved under.
    ///
    /// A stable key and not the variant's index: inserting a screen in the
    /// middle would otherwise read back the neighbour's layout.
    pub fn key(self) -> &'static str {
        match self {
            Self::Review => "review",
            Self::Files => "files",
            Self::Db => "db",
            Self::Sentry => "sentry",
            Self::Settings => "settings",
        }
    }

    pub fn from_key(key: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|w| w.key() == key)
    }

    /// The i18n key of the name, the one in the tooltip.
    pub fn label(self) -> &'static str {
        match self {
            Self::Review => "workspace-review",
            Self::Files => "workspace-files",
            Self::Db => "workspace-db",
            Self::Sentry => "workspace-sentry",
            Self::Settings => "workspace-settings",
        }
    }

    /// The bar's icon. It says what the screen **contains**, not what it is
    /// called: it is what you aim at, the name only comes in the tooltip.
    pub fn icon(self) -> &'static str {
        match self {
            Self::Review => "file-diff",
            Self::Files => "file-code",
            Self::Db => "database",
            Self::Sentry => "triangle-alert",
            Self::Settings => "settings",
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
            Self::Review => &[
                (NotesPanel::NAME, "panel-notes"),
                (ChangesPanel::NAME, "range-working"),
                (BranchPanel::NAME, "range-branch"),
                (HistoryPanel::NAME, "panel-history"),
                (DiffPanel::NAME, "panel-diff"),
                (TerminalPanel::NAME, "panel-terminal"),
            ],
            Self::Files => &[
                (FilesPanel::NAME, "panel-files"),
                (EditorPanel::NAME, "panel-editor"),
                (TerminalPanel::NAME, "panel-terminal"),
            ],
            Self::Db => &[
                (DbPanel::NAME, "panel-databases"),
                (SqlHistoryPanel::NAME, "panel-sql-history"),
                (ConsolePanel::NAME, "panel-sql"),
                (TerminalPanel::NAME, "panel-terminal"),
            ],
            Self::Sentry => &[
                (SentryPanel::NAME, "panel-sentry"),
                (TerminalPanel::NAME, "panel-terminal"),
            ],
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
pub fn install_default_layout(
    workspace: Workspace,
    app: &Entity<ClaudhubApp>,
    area: &mut DockArea,
    window: &mut Window,
    cx: &mut Context<DockArea>,
) {
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
    let (left, center): (Option<DockLayout>, DockLayout) = match workspace {
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
        Workspace::Review => {
            let list = DockLayout::v_split()
                .child(
                    DockLayout::tabs()
                        .panel_view(panel!(ChangesPanel), cx)
                        .panel_view(panel!(BranchPanel), cx)
                        .panel_view(panel!(HistoryPanel), cx)
                        // Hidden while there is nothing to resolve: a permanent
                        // tab would shift the others aside to serve one time in
                        // a hundred.
                        .panel_view(panel!(ConflictsPanel), cx),
                    None,
                )
                .child(
                    DockLayout::tabs().panel_view(panel!(NotesPanel), cx),
                    Some(third),
                );
            let center = DockLayout::h_split()
                .child(list, Some(REVIEW_LIST_WIDTH))
                .child(
                    DockLayout::tabs().panel_view(panel!(DiffPanel), cx),
                    Some(width - REVIEW_LIST_WIDTH),
                );
            (None, center)
        }
        // Editing: the project tree on the left, the editor in the centre.
        Workspace::Files => {
            let left = DockLayout::tabs().panel_view(panel!(FilesPanel), cx);
            let center = DockLayout::tabs().panel_view(panel!(EditorPanel), cx);
            (Some(left), center)
        }
        // The databases: the schema tree on the left, the console in the
        // centre. This is PhpStorm's explorer, and the gesture is the same —
        // you unfold what you are looking for, you query what you have found.
        Workspace::Db => {
            // The history is a tab beside the tree and not a panel of its own:
            // one looks at the schema **or** at what one has already asked of
            // it, and two columns would leave the console half the screen.
            let left = DockLayout::tabs()
                .panel_view(panel!(DbPanel), cx)
                .panel_view(panel!(SqlHistoryPanel), cx);
            let center = DockLayout::tabs().panel_view(panel!(ConsolePanel), cx);
            (Some(left), center)
        }
        // Sentry stands alone: the issue list and the trace of the one opened
        // are two halves of a single panel.
        Workspace::Sentry => {
            let center = DockLayout::tabs().panel_view(panel!(SentryPanel), cx);
            (None, center)
        }
        // The settings take the whole width: the form has a sidebar of its own,
        // and two side by side would be two lists of pages to read before
        // finding the field. The terminals stay underneath — a setting is
        // adjusted then checked, and what checks it is a shell.
        Workspace::Settings => {
            let center = DockLayout::tabs().panel_view(panel!(SettingsPanel), cx);
            (None, center)
        }
    };

    area.set_center(center, window, cx);
    if let Some(left) = left {
        area.set_dock(DockPlacement::Left, left, window, cx);
        area.set_dock_size(DockPlacement::Left, SIDEBAR_WIDTH, window, cx);
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
        ButtonGroup::new("workspace-nav")
            .compact()
            .children(Workspace::ALL.map(|workspace| {
                let here = workspace == current;
                Button::new(("workspace", workspace as usize))
                    .icon(icon(workspace.icon()))
                    .tooltip(tr!(workspace.label()))
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
            .on_click(cx.listener(|this, selected: &Vec<usize>, window, cx| {
                let Some(index) = selected.first() else {
                    return;
                };
                let Some(workspace) = Workspace::ALL.get(*index).copied() else {
                    return;
                };
                this.enter_workspace(workspace, window, cx);
            }))
    }
}
