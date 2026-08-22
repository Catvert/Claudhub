//! The dock's panels.
//!
//! Every area of the interface is a separate entity, which gpui-component's
//! dock requires in order to move it: the dock handles dragging, tabs and dock
//! zones. The panels carry no state — they delegate to `ClaudhubApp`, which
//! remains the single source.
//!
//! The reference to `ClaudhubApp` is **weak**. Strong, it would form a cycle —
//! the application holds the dock, which holds the panels — and nothing would
//! be freed when the window closes.
//!
//! Rendering from an `update` on `ClaudhubApp` is legitimate because a child
//! view's render happens *after* the parent's render closure has returned:
//! layout is done outside that borrow.

use gpui::{
    div, prelude::*, App, AppContext, Context, Entity, EventEmitter, FocusHandle, Focusable,
    IntoElement, Render, WeakEntity, Window,
};
use gpui_component::button::{Button, ButtonVariants as _};
use gpui_component::dock::{BasePanel, Panel, PanelControl, PanelEvent};
use gpui_component::menu::{ContextMenuExt as _, DropdownMenu as _};
use gpui_component::menu::{PopupMenu, PopupMenuItem};
use gpui_component::ActiveTheme;
use gpui_component::Sizable as _;

use gpui_component::dock::{panel_handle, register_panel};

use crate::tr;
use crate::ui::app::ClaudhubApp;
use crate::ui::find::Pane;
use crate::ui::settings::Settings;

/// A panel's background, and the bottom corners of the card carrying it.
///
/// No card here any more: it is the **group's frame** that is one now — the
/// fork rounds `TabGroupSkin::frame` and spaces the splits with a gutter, so
/// the tab bar and the content share one surface, with no seam or border
/// between them. Redrawing a card inside would put back the seam just removed.
///
/// `rounded_b`: gpui's content mask is **rectangular** — the group frame's
/// rounding does not clip its children, and a square background painted here
/// would cover the card's bottom corners. At the top, the tab rail is inset and
/// lets the frame show; at the bottom, this background has the last word. Every
/// panel must therefore go through it: one that skips it has square corners,
/// and nothing points that out.
fn pane_frame(content: impl IntoElement, cx: &App) -> gpui::Div {
    div()
        .size_full()
        .rounded_b(cx.theme().radius_lg)
        .bg(cx.theme().background)
        .child(content)
}

/// The same, while recording the panel just **touched**: that is what gives
/// `Ctrl+F` a target.
///
/// The click and not the focus: the dock puts focus on the active tab of
/// **each** zone, there are three shown at once, and nothing in that says which
/// one the user is looking at. In the **capture** phase, so before the children
/// and without any of them being able to stop it: a diff line, like a checkbox,
/// consumes its click, and the panel would never know it had been touched.
///
/// The terminals have no search panel — `Ctrl+F` there belongs to the running
/// program — and it is for them that the two functions are separate: the frame
/// is theirs, the note is not.
fn pane_root(
    app: &Entity<ClaudhubApp>,
    pane: Pane,
    content: impl IntoElement,
    cx: &App,
) -> impl IntoElement {
    let app = app.clone();
    pane_frame(content, cx).capture_any_mouse_down(move |_, _window, cx| {
        app.update(cx, |app, cx| app.touch_pane(pane, cx));
    })
}

/// Declares the panels to the dock's registry.
///
/// That is what makes it possible to rebuild a saved layout: it holds only
/// names, and the registry says how to build the matching entity. Without this
/// declaration, a layout read back shows "unknown" panels in place of ours.
pub fn register(app: &Entity<ClaudhubApp>, cx: &mut App) {
    macro_rules! declare {
        ($($name:ident => $id:literal),* $(,)?) => { $(
            let handle = app.clone();
            register_panel(cx, $id, move |_state, _window, cx| {
                let handle = handle.clone();
                panel_handle(cx.new(|cx| $name::new(&handle, cx)))
            });
        )* };
    }
    declare! {
        ChangesPanel => "ClaudhubChanges",
        BranchPanel => "ClaudhubBranch",
        HistoryPanel => "ClaudhubHistory",
        NotesPanel => "ClaudhubNotes",
        ConflictsPanel => "ClaudhubConflicts",
        FilesPanel => "ClaudhubFiles",
        DbPanel => "ClaudhubDb",
        SearchPanel => "ClaudhubSearch",
        SearchPreviewPanel => "ClaudhubSearchPreview",
        DiffPanel => "ClaudhubDiff",
        EditorPanel => "ClaudhubEditor",
        ConsolePanel => "ClaudhubConsole",
        SettingsPanel => "ClaudhubSettings",
    }
    // The plugins' panels. One type, one instance per plugin, named after its
    // directory — see `ui::plugin_view`. They are registered here and not
    // built on the fly for one reason that decides the rest: a panel has to be
    // in this registry **before** `layout.json` is read back, or a plugin's tab
    // comes back as an empty frame. That is also why adding or removing a
    // plugin takes a restart, while its *script* reloads hot.
    for manifest in crate::ui::plugin_view::manifests() {
        let handle = app.clone();
        let panel = manifest.panel;
        register_panel(cx, panel, move |_state, _window, cx| {
            let handle = handle.clone();
            panel_handle(cx.new(|cx| PluginPanel::new(&handle, panel, cx)))
        });
    }
    // No builder for the terminals, and it is not an oversight: they are the
    // only panel whose content is a **process**, and a saved layout is read
    // long after that process has died. They are pruned from the layout before
    // it is written (`app::save_layouts`), so none is ever read back — see
    // "Les terminaux dans le dock" in CLAUDE.md.
}

/// "Hide this view", the only entry the dock's `…` menu deserves.
///
/// Everything else a panel can do lives in its own bar — the review's tree, the
/// diff's two columns, the explorer's collapse — and duplicating it here would
/// make two paths for one gesture. Hiding, for its part, is not about the
/// panel's content but about its place in the window: the dock is what holds
/// it, and the dock's menu is the only place the gesture is found for every one
/// of the views.
///
/// You come back through the main menu (`VIEWS`): a hidden view has no tab left,
/// so nothing left to click.
fn hide_view(app: &WeakEntity<ClaudhubApp>, name: &'static str, menu: PopupMenu) -> PopupMenu {
    let app = app.clone();
    menu.item(
        PopupMenuItem::new(tr!("action-hide-view"))
            .icon(crate::ui::icons::icon("eye-off"))
            .on_click(move |_, _window, cx| {
                let _ = app.update(cx, |this, cx| this.set_panel_visible(name, false, cx));
            }),
    )
}

/// A view's visibility at the moment its panel is built.
///
/// Read from the settings and not from `ClaudhubApp`: the panels are built
/// **during** `ClaudhubApp::new`, and reading the root entity there while it is
/// updating is what gpui refuses with a panic. Both say the same thing — the
/// application holds its list from the settings.
fn visible_at_startup(name: &str, cx: &App) -> bool {
    !Settings::global(cx).hidden_panels.iter().any(|n| n == name)
}

/// Zoom is a **button**, not a menu entry.
///
/// It is the only action the dock puts in its `…` menu — none of our panels
/// closes — and a dropdown holding a single line costs two clicks for what is
/// worth one. `PanelControl::Toolbar` brings it out into the tab bar, next to
/// the title.
///
/// What cannot be done, and should not be looked for:
/// gpui-component 0.5.1's `TabPanel::render_toolbar` places the `…` button
/// **unconditionally**. It therefore stays visible, its zoom entry greyed out.
/// Removing it would mean vendoring the library for one button.
fn zoom_in_toolbar() -> Option<PanelControl> {
    Some(PanelControl::Toolbar)
}

macro_rules! panels {
    ($($name:ident => ($id:literal, $title:literal, $render:ident, $pane:ident)),* $(,)?) => { $(
        pub struct $name {
            app: WeakEntity<ClaudhubApp>,
            focus: FocusHandle,
            /// Cached for the same reason as the conflicts panel's: `visible`
            /// is called while the layout is being built, so in the middle of
            /// `ClaudhubApp::new`.
            visible: bool,
        }

        impl $name {
            pub const NAME: &'static str = $id;

            pub fn new(app: &Entity<ClaudhubApp>, cx: &mut Context<Self>) -> Self {
                // Without this observation, the panel would keep the picture of
                // the state at the moment it was built: it is `ClaudhubApp`
                // that changes, not the panel.
                cx.observe(app, |this: &mut Self, app, cx| {
                    let visible = app.read(cx).panel_visible(Self::NAME);
                    if this.visible != visible {
                        this.visible = visible;
                        // It is the area that re-reads its tabs' visibility:
                        // notifying the panel alone would not make it
                        // disappear.
                        cx.emit(PanelEvent::LayoutChanged);
                    }
                    cx.notify();
                })
                .detach();
                Self {
                    app: app.downgrade(),
                    focus: cx.focus_handle(),
                    visible: visible_at_startup(Self::NAME, cx),
                }
            }
        }

        impl Focusable for $name {
            fn focus_handle(&self, _: &App) -> FocusHandle {
                self.focus.clone()
            }
        }

        impl EventEmitter<PanelEvent> for $name {}

        // Two traits since the dock rework: `BasePanel` carries what decides the
        // layout — the persisted name, visibility, closing, zoom — and lives in
        // `gpui-base`, which cannot draw. `Panel` carries the presentation, and
        // exists only in the skin. It is that separation that would let us write
        // a skin of our own without taking over the engine.
        impl BasePanel for $name {
            fn panel_name(&self) -> &'static str {
                $id
            }

            /// No panel closes: nothing would make it possible to reopen one,
            /// and a review without its file list is no longer a review.
            fn closable(&self, _: &App) -> bool {
                false
            }

            fn visible(&self, _: &App) -> bool {
                self.visible
            }
        }

        impl Panel for $name {
            fn title(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
                tr!($title)
            }

            fn zoom_control(&self, _: &App) -> Option<PanelControl> {
                zoom_in_toolbar()
            }

            fn dropdown_menu(
                &mut self,
                menu: PopupMenu,
                _: &mut Window,
                _: &mut Context<Self>,
            ) -> PopupMenu {
                hide_view(&self.app, Self::NAME, menu)
            }
        }

        impl Render for $name {
            fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
                let Some(app) = self.app.upgrade() else {
                    return div().into_any_element();
                };
                let content = app.update(cx, |app, cx| app.$render(window, cx).into_any_element());
                pane_root(&app, Pane::$pane, content, cx).into_any_element()
            }
        }
    )* };
}

panels! {
    ChangesPanel => ("ClaudhubChanges", "range-working", render_changes, Changes),
    BranchPanel => ("ClaudhubBranch", "range-branch", render_branch_review, Branch),
    NotesPanel => ("ClaudhubNotes", "panel-notes", render_notes, Notes),
    TagsPanel => ("ClaudhubTags", "panel-tags", render_tags, Tags),
    FilesPanel => ("ClaudhubFiles", "panel-files", render_files, Files),
    DbPanel => ("ClaudhubDb", "panel-databases", render_db, Db),
    SqlHistoryPanel => ("ClaudhubSqlHistory", "panel-sql-history", render_sql_history, SqlHistory),
    SearchPanel => ("ClaudhubSearch", "panel-search", render_search, Search),
    SearchPreviewPanel => ("ClaudhubSearchPreview", "panel-search-preview", render_search_preview, SearchPreview),
    // The centre of each screen. **Three panels and not one whose title
    // changes**: they belonged to the same one because they were fighting over
    // the central slot, and a tab announcing "Diff", "Editor" or "SQL"
    // depending on the last gesture was saying plainly that it carried three.
    // The screens give each of them its own place.
    DiffPanel => ("ClaudhubDiff", "panel-diff", render_diff, Diff),
    EditorPanel => ("ClaudhubEditor", "panel-editor", render_editor_panel, Editor),
    ConsolePanel => ("ClaudhubConsole", "panel-sql", render_console_panel, Console),
    SettingsPanel => ("ClaudhubSettings", "panel-settings", render_settings_panel, Settings),
}

/// The conflicts only appear when there are some.
///
/// `Panel::visible`, like the terminals: a permanently present "Conflicts" tab
/// would shift the others aside and serve one time in a hundred. It stays
/// visible while an operation is in progress, even with no conflicted file —
/// that is where what is needed to continue or abort it is found.
///
/// **Visibility is cached and not read on demand.** `visible` is called by
/// `TabPanel::active_panel`, including from `add_panel` while the layout is
/// being built — that is, **inside** `ClaudhubApp::new`. Reading the root
/// entity there would read it while it is being updated, which gpui refuses
/// with a panic. The observation set up in the constructor, on the other hand,
/// fires outside any borrow.
pub struct ConflictsPanel {
    app: WeakEntity<ClaudhubApp>,
    focus: FocusHandle,
    visible: bool,
}

impl ConflictsPanel {
    pub fn new(app: &Entity<ClaudhubApp>, cx: &mut Context<Self>) -> Self {
        cx.observe(app, |this: &mut Self, app, cx| {
            let app = app.read(cx);
            let visible = app.pending_operation().is_some() || !app.conflicted_files().is_empty();
            if this.visible != visible {
                this.visible = visible;
                // The dock re-reads its tabs' visibility when the zone
                // redraws: it is the area's notification, and not the panel's,
                // that makes a tab appear or disappear.
                cx.emit(PanelEvent::LayoutChanged);
            }
            cx.notify();
        })
        .detach();
        Self {
            app: app.downgrade(),
            focus: cx.focus_handle(),
            // False to begin with, and that is not a makeshift: no repository is
            // open yet when the layout is built.
            visible: false,
        }
    }
}

impl Focusable for ConflictsPanel {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl EventEmitter<PanelEvent> for ConflictsPanel {}

impl BasePanel for ConflictsPanel {
    fn panel_name(&self) -> &'static str {
        "ClaudhubConflicts"
    }
    fn closable(&self, _: &App) -> bool {
        false
    }
    fn visible(&self, _: &App) -> bool {
        self.visible
    }
}

impl Panel for ConflictsPanel {
    fn title(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        tr!("panel-conflicts")
    }

    fn zoom_control(&self, _: &App) -> Option<PanelControl> {
        zoom_in_toolbar()
    }
}

impl Render for ConflictsPanel {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let Some(app) = self.app.upgrade() else {
            return div().into_any_element();
        };
        let content = app.update(cx, |app, cx| {
            app.render_conflicts(window, cx).into_any_element()
        });
        pane_root(&app, Pane::Conflicts, content, cx).into_any_element()
    }
}

/// One open terminal, as the dock shows it.
///
/// **A panel per terminal**, where there used to be one panel drawing a strip
/// of tabs of its own. The dock's bar is that strip now, which is what lets a
/// terminal be dragged into a split, sent to another zone or zoomed like any
/// other view.
///
/// One panel per **screen** too: a panel belongs to a single dock area at a
/// time and there are five, so five of these share one `TerminalView` — one
/// pty, five faces. Only one dock is displayed at a time, so no two of them
/// ever draw the same grid in the same frame.
///
/// It carries the terminal's worktree because that is what decides whether it
/// is shown: the terminals of the worktree one is not looking at stay in the
/// tree, invisible, which is what keeps a terminal dragged into a split exactly
/// where it was put — including across a round trip through another worktree.
pub struct TerminalPanel {
    app: WeakEntity<ClaudhubApp>,
    worktree: std::path::PathBuf,
    view: Entity<crate::ui::terminal_view::TerminalView>,
    /// The tab group showing it, as the dock hands it over.
    ///
    /// It is the only way in: `DockArea` keeps its groups to itself, and
    /// `on_added_to` is the seam through which a panel learns which one it is
    /// in. Without it, "show this terminal" could only be done by *moving* the
    /// panel into its own group — which activates it, and reorders the tabs on
    /// the way.
    group: Option<gpui::WeakEntity<gpui_component::dock::TabGroup>>,
    /// Cached for the same reason as the conflicts panel's: `visible` is called
    /// while the layout is being built, so in the middle of
    /// `ClaudhubApp::new`.
    visible: bool,
}

impl TerminalPanel {
    pub const NAME: &'static str = "ClaudhubTerminal";

    /// `visible` is **given** and not read off the application.
    ///
    /// A terminal is opened from inside an `update` on `ClaudhubApp`, so the
    /// entity is out of the table while this runs: reading it there panics with
    /// "cannot read … while it is already being updated". Its caller holds a
    /// `&self` on the application and knows the answer; the observation below
    /// takes over from the next change.
    pub fn new(
        app: &Entity<ClaudhubApp>,
        worktree: std::path::PathBuf,
        view: Entity<crate::ui::terminal_view::TerminalView>,
        visible: bool,
        cx: &mut Context<Self>,
    ) -> Self {
        let mine = worktree.clone();
        cx.observe(app, move |this: &mut Self, app, cx| {
            let visible = app.read(cx).terminal_shown(&mine, cx);
            if this.visible != visible {
                this.visible = visible;
                cx.emit(PanelEvent::LayoutChanged);
            }
            cx.notify();
        })
        .detach();
        // The terminal redraws several times a second while an agent works, and
        // its label — the running program — is the tab's title.
        cx.observe(&view, |_, _, cx| cx.notify()).detach();
        Self {
            app: app.downgrade(),
            visible,
            worktree,
            view,
            group: None,
        }
    }

    /// Makes this terminal the displayed tab of its group.
    ///
    /// What "open a terminal" and "send this to the agent" need: the panel
    /// exists and is in the right group, but the tab beside it is the one on
    /// screen.
    pub fn activate(panel: &Entity<Self>, window: &mut Window, cx: &mut App) {
        let Some(group) = panel
            .read(cx)
            .group
            .clone()
            .and_then(|group| group.upgrade())
        else {
            return;
        };
        let me = gpui_component::dock::PanelId::from(panel.entity_id());
        group.update(cx, |group, cx| {
            if let Some(ix) = group
                .panels()
                .iter()
                .position(|panel| panel.panel_id(cx) == me)
            {
                group.select_tab(ix, window, cx);
            }
        });
    }
}

/// The "+" of the terminals: a shell, or one of the agent profiles.
///
/// One entry per profile, as the hand-painted strip used to offer: the menu is
/// the only place the choice arises, and a list coming from the settings saves
/// reopening them to launch something else.
fn new_terminal_button(app: &WeakEntity<ClaudhubApp>) -> impl IntoElement {
    let app = app.clone();
    Button::new("new-terminal")
        .ghost()
        .xsmall()
        .icon(crate::ui::icons::icon("plus"))
        .tooltip(tr!("terminal-new"))
        .dropdown_menu(move |menu, _window, cx| {
            let shell = app.clone();
            let profiles = Settings::global(cx).terminal.agents.clone();
            let menu = menu.item(
                PopupMenuItem::new(tr!("terminal-new"))
                    .icon(crate::ui::icons::icon("plus"))
                    .on_click(move |_, window, cx| {
                        open_terminal(&shell, None, window, cx);
                    }),
            );
            if profiles.is_empty() {
                return menu;
            }
            profiles
                .into_iter()
                .fold(menu.separator(), |menu, profile| {
                    let app = app.clone();
                    let label = gpui::SharedString::from(profile.label().to_string());
                    menu.item(
                        PopupMenuItem::new(label)
                            .icon(crate::ui::icons::icon("bot"))
                            .on_click(move |_, window, cx| {
                                open_terminal(&app, Some(profile.clone()), window, cx);
                            }),
                    )
                })
        })
}

/// Opens a shell, or an agent profile, on the worktree being looked at.
fn open_terminal(
    app: &WeakEntity<ClaudhubApp>,
    profile: Option<crate::ui::settings::AgentProfile>,
    window: &mut Window,
    cx: &mut App,
) {
    let Some(app) = app.upgrade() else {
        return;
    };
    app.update(cx, |app, cx| {
        let Some(worktree) = app.active_path() else {
            return;
        };
        let launch = match &profile {
            Some(profile) => crate::ui::terminal_view::Launch::agent(profile),
            None => crate::ui::terminal_view::Launch::shell(),
        };
        app.open_terminal(&worktree, launch, window, cx);
    });
}

impl Focusable for TerminalPanel {
    fn focus_handle(&self, cx: &App) -> FocusHandle {
        self.view.read(cx).focus_handle(cx)
    }
}

impl EventEmitter<PanelEvent> for TerminalPanel {}

impl BasePanel for TerminalPanel {
    fn panel_name(&self) -> &'static str {
        Self::NAME
    }
    /// Closable, unlike every other panel of this window: closing a terminal
    /// tab is how one ends a shell, and it is the only panel whose content is a
    /// process rather than a view of the repository.
    fn closable(&self, _: &App) -> bool {
        true
    }
    fn visible(&self, _: &App) -> bool {
        self.visible
    }

    /// The pty dies with the tab, and takes its four other faces with it.
    ///
    /// `on_removed` fires on the one panel the user closed — one screen's —
    /// and the other four would otherwise stay as tabs showing a dead shell.
    fn on_added_to(
        &mut self,
        group: gpui::WeakEntity<gpui_component::dock::TabGroup>,
        _: &mut Window,
        _: &mut Context<Self>,
    ) {
        self.group = Some(group);
    }

    fn on_removed(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let id = self.view.entity_id();
        let Some(app) = self.app.upgrade() else {
            return;
        };
        // **Deferred**, and it is not a precaution: `on_removed` is called from
        // inside the dock's own edit, and taking the four other faces down goes
        // back through `DockArea::remove_panel` — including this area, which is
        // in the middle of being updated. Straight through, that is the panic
        // that reads "cannot update … while it is already being updated".
        cx.defer_in(window, move |_, window, cx| {
            app.update(cx, |app, cx| app.close_terminal(id, window, cx));
        });
    }

    /// What `layout.json` keeps of a terminal: where it worked.
    ///
    /// Not the pty, which does not survive the process, and not its scrollback.
    /// A terminal read back is a **fresh shell in the same place** — the layout
    /// comes back, the conversation does not, and pretending otherwise would be
    /// worse than saying so.
    fn dump(&self, _: &App) -> gpui_component::dock::PanelState {
        let mut state = gpui_component::dock::PanelState::new(Self::NAME);
        state.info = gpui_component::dock::PanelInfo::panel(
            serde_json::json!({ "worktree": self.worktree }),
        );
        state
    }
}

impl Panel for TerminalPanel {
    /// The running program and a cross to end it.
    ///
    /// The program, because that is what one looks for among five tabs — not
    /// the word "Terminal" five times over. And the cross **in the tab**,
    /// because that is where one closes a terminal: the dock offers closing a
    /// whole group from its menu, which is not the same gesture, and the strip
    /// this panel replaced had one on every tab.
    ///
    /// It is painted here and not by the dock's skin, which draws no per-tab
    /// close button: `Panel::title` renders an element, and an element can
    /// carry a button. That saves a sixth commit on the fork for something only
    /// the terminals want.
    fn title(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let id = self.view.entity_id();
        let app = self.app.clone();
        // The name comes from the application and not from the view: it can be
        // given by hand, and the five panels of one terminal must say the same
        // thing. Reading it here is legitimate — a panel renders after the
        // application's own render closure has returned.
        let label = app
            .upgrade()
            .map(|app| app.read(cx).terminal_label(id, cx))
            .unwrap_or_default();
        let rename = app.clone();
        let last = app
            .upgrade()
            .is_some_and(|app| app.read(cx).is_last_terminal(id));
        gpui_component::h_flex()
            .id(("terminal-tab", id))
            .gap_1()
            .items_center()
            // Renaming is a right click and not a double click: a double click
            // on a tab bar already means "zoom this group" everywhere else, and
            // the tab under this element consumes the plain click to select.
            .context_menu(move |menu, _window, _cx| {
                let app = rename.clone();
                menu.item(
                    gpui_component::menu::PopupMenuItem::new(tr!("terminal-rename"))
                        .icon(crate::ui::icons::icon("pencil"))
                        .on_click(move |_, window, cx| {
                            let Some(app) = app.upgrade() else {
                                return;
                            };
                            app.update(cx, |app, cx| app.ask_terminal_name(id, window, cx));
                        }),
                )
            })
            .child(label)
            .child(
                Button::new("close-terminal")
                    .ghost()
                    .xsmall()
                    .icon(crate::ui::icons::icon("x"))
                    .on_click(move |_, window, cx| {
                        // The tab under it selects on click: without this, the
                        // cross would first bring forward the terminal it is
                        // about to close.
                        cx.stop_propagation();
                        let Some(app) = app.upgrade() else {
                            return;
                        };
                        // Deferred for the reason `on_removed` is: closing goes
                        // through `DockArea::remove_panel`, and we are inside
                        // the dock's own event dispatch.
                        window.defer(cx, move |window, cx| {
                            app.update(cx, |app, cx| app.close_terminal(id, window, cx));
                        });
                    }),
            )
            // The "+" **follows the last tab** rather than sticking to the
            // right edge of the bar. That is where the eye finishes reading the
            // tabs, and a button at the other end of the panel makes one cross
            // it to open the next terminal. It was the rule of the hand-painted
            // strip this replaced, and the dock's bar offers no place for it —
            // so it rides in the last tab's own title.
            .when(last, |el| el.child(new_terminal_button(&self.app)))
    }

    fn zoom_control(&self, _: &App) -> Option<PanelControl> {
        zoom_in_toolbar()
    }
}

impl Render for TerminalPanel {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // No `pane_root`: the terminals have no search of their own, `Ctrl+F`
        // there belonging to the program that runs.
        pane_frame(self.view.clone(), cx).into_any_element()
    }
}

/// The history needs loading the first time it is looked at.
///
/// Doing it at render time rather than at construction is what avoids a `git
/// log` on a tab nobody will open; `ensure_history` only asks once, otherwise
/// every frame would restart the command.
pub struct HistoryPanel {
    app: WeakEntity<ClaudhubApp>,
    focus: FocusHandle,
    visible: bool,
}

impl HistoryPanel {
    pub const NAME: &'static str = "ClaudhubHistory";

    pub fn new(app: &Entity<ClaudhubApp>, cx: &mut Context<Self>) -> Self {
        cx.observe(app, |this: &mut Self, app, cx| {
            let visible = app.read(cx).panel_visible(Self::NAME);
            if this.visible != visible {
                this.visible = visible;
                cx.emit(PanelEvent::LayoutChanged);
            }
            cx.notify();
        })
        .detach();
        Self {
            app: app.downgrade(),
            focus: cx.focus_handle(),
            visible: visible_at_startup(Self::NAME, cx),
        }
    }
}

impl Focusable for HistoryPanel {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl EventEmitter<PanelEvent> for HistoryPanel {}

impl BasePanel for HistoryPanel {
    fn panel_name(&self) -> &'static str {
        Self::NAME
    }
    fn closable(&self, _: &App) -> bool {
        false
    }
    fn visible(&self, _: &App) -> bool {
        self.visible
    }
}

impl Panel for HistoryPanel {
    fn title(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        tr!("panel-history")
    }

    fn zoom_control(&self, _: &App) -> Option<PanelControl> {
        zoom_in_toolbar()
    }

    fn dropdown_menu(
        &mut self,
        menu: PopupMenu,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> PopupMenu {
        hide_view(&self.app, Self::NAME, menu)
    }
}

impl Render for HistoryPanel {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let Some(app) = self.app.upgrade() else {
            return div().into_any_element();
        };
        let content = app.update(cx, |app, cx| {
            app.ensure_history(cx);
            app.render_history(window, cx).into_any_element()
        });
        pane_root(&app, Pane::History, content, cx).into_any_element()
    }
}

/// A plugin's panel.
///
/// One Rust type for every plugin: what differs between two of them is a
/// `&'static str`, not a shape. It carries no state — like every other panel it
/// delegates to `ClaudhubApp`, which holds the script, its state and the tree
/// it last produced.
pub struct PluginPanel {
    app: WeakEntity<ClaudhubApp>,
    focus: FocusHandle,
    /// The plugin's id in the dock's registry, leaked once at discovery.
    /// `BasePanel::panel_name` wants a `&'static str` and a plugin's name is
    /// only known at run time; see `plugin::manifest::Manifest::panel`.
    name: &'static str,
    /// Cached for the same reason as the others': `visible` is called while
    /// the layout is being built, so inside `ClaudhubApp::new`, where reading
    /// the root entity is a panic.
    visible: bool,
}

impl PluginPanel {
    pub fn new(app: &Entity<ClaudhubApp>, name: &'static str, cx: &mut Context<Self>) -> Self {
        cx.observe(app, move |this: &mut Self, app, cx| {
            // Two reasons a plugin's tab is not there: hidden from the "Views"
            // menu like any other panel, or the plugin switched off — which is
            // not the same gesture and does not live in the same file.
            let visible = app.read(cx).panel_visible(this.name)
                && crate::ui::plugin_view::panel_enabled(this.name, cx);
            if this.visible != visible {
                this.visible = visible;
                cx.emit(PanelEvent::LayoutChanged);
            }
            cx.notify();
        })
        .detach();
        Self {
            app: app.downgrade(),
            focus: cx.focus_handle(),
            name,
            visible: visible_at_startup(name, cx)
                && crate::ui::plugin_view::panel_enabled(name, cx),
        }
    }
}

impl Focusable for PluginPanel {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl EventEmitter<PanelEvent> for PluginPanel {}

impl BasePanel for PluginPanel {
    fn panel_name(&self) -> &'static str {
        self.name
    }

    fn closable(&self, _: &App) -> bool {
        false
    }

    fn visible(&self, _: &App) -> bool {
        self.visible
    }
}

impl Panel for PluginPanel {
    /// The title comes from the manifest and not from a catalogue: a plugin's
    /// strings are its own — `tr!` reads catalogues compiled into the binary,
    /// and a test compares their keys.
    fn title(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        gpui::SharedString::from(
            crate::ui::plugin_view::by_panel(self.name)
                .map(|manifest| manifest.title().to_string())
                .unwrap_or_else(|| self.name.to_string()),
        )
    }

    fn zoom_control(&self, _: &App) -> Option<PanelControl> {
        zoom_in_toolbar()
    }

    fn dropdown_menu(
        &mut self,
        menu: PopupMenu,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> PopupMenu {
        hide_view(&self.app, self.name, menu)
    }
}

impl Render for PluginPanel {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let Some(app) = self.app.upgrade() else {
            return div().into_any_element();
        };
        let name = self.name;
        let content = app.update(cx, |app, cx| {
            app.render_plugin(name, window, cx).into_any_element()
        });
        // `pane_frame` and not `pane_root`: a plugin's panel is not searchable
        // — `Ctrl+F` searches a list whose order we own, and here the script
        // owns it. The terminals are in the same case, for the same reason.
        pane_frame(content, cx).into_any_element()
    }
}
