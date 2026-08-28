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
    canvas, div, point, prelude::*, App, AppContext, Context, Entity, EventEmitter, FocusHandle,
    Focusable, Hsla, IntoElement, PathBuilder, Pixels, Render, WeakEntity, Window,
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

/// A gesture shared by two controls of the same tab — the cross and the
/// wheel button both close the file.
type Closing = std::rc::Rc<dyn Fn(&mut Window, &mut App)>;

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
    let radius = cx.theme().radius_lg;
    let outside = crate::ui::theme::gutter(cx);
    div()
        .size_full()
        .relative()
        .rounded_b(radius)
        .bg(cx.theme().background)
        .child(content)
        // And the corners are cut back **over** the content, for the same
        // rectangular mask: this background has the last word only over what is
        // painted before it. A panel whose content fills its own surface —
        // the editor paints an opaque quad the height of the panel under its
        // line numbers, the result grid one per row — brought the square corner
        // back, and no rounding of ours can reach inside a child.
        .child(corner_cut(radius, true, outside))
        .child(corner_cut(radius, false, outside))
}

/// The sliver a rounded corner leaves out, painted in the colour behind the
/// card.
///
/// A **path** and not a small square of that colour: a square would also cover
/// what falls inside the corner — the last centimetre of a scrollbar's travel
/// on the right, the gutter's own colour on the left — and would only be right
/// where the two happen to be the same. What is painted is exactly the outside
/// of the quarter circle, so the card looks cut rather than patched.
///
/// The quarter is a cubic Bézier at the usual constant: an elliptical arc would
/// say the same thing with two flags whose meaning depends on which way the y
/// axis runs.
fn corner_cut(radius: Pixels, left: bool, colour: Hsla) -> impl IntoElement {
    /// What makes a cubic Bézier a quarter circle, to within a thousandth.
    const KAPPA: f32 = 0.5523;

    canvas(
        |_, _, _| {},
        move |bounds, _, window, _| {
            let (x, y) = (bounds.origin.x, bounds.origin.y);
            let (r, k) = (radius, radius * KAPPA);
            let mut path = PathBuilder::fill();
            if left {
                // The panel's corner is at the bottom left of this box, and the
                // rounding is centred on its top-right.
                path.move_to(point(x, y));
                path.line_to(point(x, y + r));
                path.line_to(point(x + r, y + r));
                path.cubic_bezier_to(point(x, y), point(x + r - k, y + r), point(x, y + k));
            } else {
                path.move_to(point(x + r, y));
                path.line_to(point(x + r, y + r));
                path.line_to(point(x, y + r));
                path.cubic_bezier_to(point(x + r, y), point(x + k, y + r), point(x + r, y + k));
            }
            if let Ok(path) = path.build() {
                window.paint_path(path, colour);
            }
        },
    )
    .absolute()
    .bottom_0()
    .map(|el| if left { el.left_0() } else { el.right_0() })
    .w(radius)
    .h(radius)
}

/// Closing a tab with the wheel button.
///
/// The gesture of every browser and every editor, and the only one that closes
/// a tab without aiming at a cross the size of a full stop — which is what one
/// wants when six terminals have to go.
///
/// **`on_aux_click` and not `on_mouse_down`**: a click is a press *and* a
/// release on the same tab, so a wheel button pressed by mistake is taken back
/// by letting go somewhere else — a middle button has no other way to be
/// cancelled, having no drag. And the event carries its button, which is what
/// this listener needs: gpui sends it every click that is not the left one, the
/// right one included, and the right one belongs to the rename menu.
///
/// The click is **consumed**: the tab under it would otherwise bring forward
/// the panel it is about to close, exactly as it does under the cross.
/// Brings a panel's own tab forward in the group that holds it.
///
/// The two closable panels — a file, a terminal — need exactly this and say it
/// the same way: the panel exists and sits in the right group, but the tab on
/// screen is whichever was added last.
fn select_own_tab(
    group: Option<gpui::WeakEntity<gpui_component::dock::TabGroup>>,
    panel: gpui::EntityId,
    window: &mut Window,
    cx: &mut App,
) {
    let Some(group) = group.and_then(|group| group.upgrade()) else {
        return;
    };
    let me = gpui_component::dock::PanelId::from(panel);
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

fn close_on_middle_click<E: gpui::StatefulInteractiveElement>(
    tab: E,
    close: impl Fn(&mut Window, &mut App) + 'static,
) -> E {
    tab.on_aux_click(move |event, window, cx| {
        let gpui::ClickEvent::Mouse(mouse) = event else {
            return;
        };
        if mouse.up.button != gpui::MouseButton::Middle {
            return;
        }
        cx.stop_propagation();
        close(window, cx);
    })
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

/// The names the dock can build, which is what a saved layout may name.
///
/// Kept as they are declared rather than written out a second time: two lists
/// diverge on the first addition, which is the whole reason `register_generated`
/// is generated. What reads it is `is_registered`.
static REGISTERED: std::sync::OnceLock<std::sync::Mutex<std::collections::HashSet<&'static str>>> =
    std::sync::OnceLock::new();

/// Declares one panel to the dock's registry, and to ourselves.
fn declare_panel<F>(cx: &mut App, name: &'static str, build: F)
where
    F: Fn(
            gpui_component::dock::PanelBuildContext,
            &mut Window,
            &mut App,
        ) -> std::sync::Arc<dyn gpui_component::dock::BasePanelView>
        + 'static,
{
    REGISTERED
        .get_or_init(Default::default)
        .lock()
        .expect("BUG: the panel names are only ever inserted")
        .insert(name);
    register_panel(cx, name, build);
}

/// Can the dock build a panel of this name.
///
/// What a layout read back is filtered through: a name nothing can build comes
/// back as an empty frame — or, worse, as the registry's own "panel type is not
/// registered" printed across the screen — and it comes back **at every
/// start**, a reset of the view only deferring it. It happens for a plugin
/// uninstalled between two sessions, and for a panel of ours that has gone;
/// `LAYOUT_VERSION` answers the second only by throwing away every screen's
/// arrangement, which is a heavy price for one dead name.
pub fn is_registered(name: &str) -> bool {
    REGISTERED
        .get_or_init(Default::default)
        .lock()
        .is_ok_and(|names| names.contains(name))
}

/// The registry's own copy of a name, which is a `&'static str`.
///
/// What a recorded place needs: the store and the dock's tree hand back a
/// `String`, and every gesture that shows a panel wants the static name the
/// dock knows it by. The registry already holds one of each, leaked for the
/// window's life — a plugin's included, `manifest` leaking it too.
pub fn registered_name(name: &str) -> Option<&'static str> {
    REGISTERED
        .get_or_init(Default::default)
        .lock()
        .ok()?
        .get(name)
        .copied()
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
            declare_panel(cx, $id, move |_state, _window, cx| {
                let handle = handle.clone();
                panel_handle(cx.new(|cx| $name::new(&handle, cx)))
            });
        )* };
    }
    declare! {
        ConflictsPanel => "ClaudhubConflicts",
    }
    register_generated(app, cx);
    // The plugins' panels. One type, one instance per plugin, named after its
    // directory — see `ui::plugin_view`. They are registered here and not
    // built on the fly for one reason that decides the rest: a panel has to be
    // in this registry **before** `layout.json` is read back, or a plugin's tab
    // comes back as an empty frame. That is also why adding or removing a
    // plugin takes a restart, while its *script* reloads hot.
    for manifest in crate::ui::plugin_view::manifests() {
        for spec in &manifest.panels {
            let handle = app.clone();
            let panel = spec.name;
            declare_panel(cx, panel, move |_state, _window, cx| {
                let handle = handle.clone();
                panel_handle(cx.new(|cx| PluginPanel::new(&handle, panel, cx)))
            });
        }
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

/// Adds a panel to **a region** and moves it where the caller says, keeping
/// the tab that region's first group was displaying.
///
/// The region is named rather than assumed: a terminal does not land in the
/// centre — the tool zones are docks. `add_panel_view` makes the region when
/// the area has none (`size` is what it is born at, the one moment we choose a
/// dock's extent), and knows how to place into an empty one by splitting its
/// root.
///
/// `add_panel_view` takes no target: it appends the panel to the region's
/// **first tab group** and activates it there, and the move takes it right
/// back out. Removing the tab a group displays leaves its active index one
/// past the end, and the clamp lands on the **last** tab — on the Git screen
/// the CI plugin's board, which every session then opened on despite
/// `app::open_on`: the session's terminal passes through that group at every
/// start. Noting the displayed tab before the add and giving it back after
/// the move is the whole cure.
///
/// Two exceptions, both meaning "the fresh panel is the one to look at": no
/// target — the panel stays where the add put it — and a target joining the
/// very group the add disturbed.
///
/// The target is computed **after** the add, which both callers need: a
/// sibling's node is looked up in the tree the add has just edited.
pub(super) fn dock_panel_at(
    dock: &mut gpui_component::dock::DockArea,
    handle: std::sync::Arc<dyn gpui_component::dock::BasePanelView>,
    placement: gpui_component::dock::DockPlacement,
    size: Option<gpui::Pixels>,
    target: impl FnOnce(&gpui_component::dock::DockArea) -> Option<gpui_component::dock::InsertTarget>,
    window: &mut Window,
    cx: &mut Context<gpui_component::dock::DockArea>,
) {
    use gpui_component::dock::{InsertTarget, NodeId, PaneNode, PaneRef, PanelId};

    /// The first tab group's displayed panel — the group `add_panel_view`
    /// lands in, found by the same walk it uses.
    fn displayed(node: &PaneNode) -> Option<(NodeId, usize, PanelId)> {
        match node.kind() {
            PaneRef::Tabs { panels, active_ix } => panels
                .get(active_ix)
                .map(|panel| (node.id(), active_ix, *panel)),
            PaneRef::Split { children, .. } => children.iter().find_map(displayed),
            PaneRef::Tiles { .. } => None,
        }
    }

    let id = handle.panel_id(cx);
    let noted = dock
        .layout(placement)
        .and_then(|tree| displayed(tree.root()));
    dock.add_panel_view(handle, placement, size, window, cx);
    let Some(target) = target(dock) else {
        return;
    };
    dock.move_panel(id, target, window, cx);
    let Some((node, ix, panel)) = noted else {
        return;
    };
    if matches!(target, InsertTarget::Tabs { node: joined, .. } if joined == node) {
        return;
    }
    dock.move_panel(
        panel,
        InsertTarget::Tabs {
            node,
            ix: Some(ix),
            activate: true,
        },
        window,
        cx,
    );
}

/// Brings the tab a panel of this name sits in forward, in one screen's dock.
///
/// What the home screen needs and no other does: elsewhere a centre holds one
/// panel of ours, so "show the diff" is "go to the Git screen"; here the four
/// centres are four tabs of one group, and showing one means selecting it.
///
/// **By name and not by entity**, which is what makes it work at all: the
/// panels are built inside `install_default_layout` and dropped there, and a
/// layout read back from `layout.json` was never built by us — nothing outside
/// the dock holds a handle on the diff's tab. `DockArea::panel` gives the view
/// back from the id the tree carries, and `panel_name` is on it.
///
/// **A move on to itself, and not a `select_tab`**: the group entity behind a
/// node is private to the dock, and `move_panel` back into its own node at its
/// own index with `activate` is the same reinstatement `dock_panel_at` makes to
/// give a disturbed group its tab back.
///
/// Every region and not the centre alone: a panel can have been dragged into a
/// side dock, and a gesture that then did nothing would read as broken.
pub(super) fn select_panel_named(
    dock: &Entity<gpui_component::dock::DockArea>,
    name: &str,
    window: &mut Window,
    cx: &mut App,
) -> bool {
    use gpui_component::dock::{DockPlacement, InsertTarget, NodeId, PaneNode, PaneRef, PanelId};

    /// The node holding a panel of this name, and where it sits in it.
    fn seat(
        node: &PaneNode,
        dock: &gpui_component::dock::DockArea,
        name: &str,
        cx: &App,
    ) -> Option<(NodeId, usize, PanelId)> {
        match node.kind() {
            PaneRef::Tabs { panels, .. } => panels.iter().enumerate().find_map(|(ix, panel)| {
                let found = dock.panel(*panel)?;
                (found.panel_name(cx) == name).then_some((node.id(), ix, *panel))
            }),
            PaneRef::Split { children, .. } => children
                .iter()
                .find_map(|child| seat(child, dock, name, cx)),
            PaneRef::Tiles { .. } => None,
        }
    }

    let found = {
        let area = dock.read(cx);
        [
            DockPlacement::Center,
            DockPlacement::Left,
            DockPlacement::Right,
            DockPlacement::Bottom,
        ]
        .into_iter()
        .find_map(|placement| seat(area.layout(placement)?.root(), area, name, cx))
    };
    let Some((node, ix, panel)) = found else {
        return false;
    };
    dock.update(cx, |dock, cx| {
        dock.move_panel(
            panel,
            InsertTarget::Tabs {
                node,
                ix: Some(ix),
                activate: true,
            },
            window,
            cx,
        );
    });
    true
}

/// Every panel whose whole shape is "a title, a render, a pane".
///
/// Two optional pieces, each of which used to be a hand-written copy of this
/// body: `visible:` names the method that decides whether the tab is there,
/// when it is not simply "not hidden"; `prepare:` names what the panel asks
/// the application for on its first paint. Everything else — the registry
/// entry included — comes from this one list.
macro_rules! panels {
    // Whether the tab is on screen: the flag, or the method a panel names.
    (@shown $app:expr, $id:literal) => { $app.panel_visible($id) };
    (@shown $app:expr, $id:literal, $visible:ident) => { $app.$visible() };

    // And whether it has anything to show.
    //
    // The centre gathers the documents into one tab group, and a group
    // announcing "Diff", "Editor", "Search" and "SQL" before one has clicked
    // anything is four names for four empty rooms. The rule used to hold on the
    // home screen alone, the other screens each having a centre of their own
    // whose empty state *was* the screen; with one workspace the centre is
    // always that group, so it holds everywhere.
    //
    // It is asked of the **application** and not of the panel, which is licit
    // for the reason it always was: a panel does not know which region holds it
    // — the registry rebuilds one from a name alone.
    (@needed $app:expr) => { true };
    (@needed $app:expr, $needed:ident) => { $app.$needed() };

    // The cached value the panel is born with, the application not being
    // readable then. See `visible_at_startup`.
    (@at_startup $name:expr, $cx:expr) => { visible_at_startup($name, $cx) };
    // Nothing is open when the window is built — no file picked, no console, no
    // hit — so a panel that answers `needed:` starts hidden and the first notify
    // replaces this with the real answer. What it buys is that the first frame
    // is not a different one.
    (@at_startup $name:expr, $cx:expr, $needed:ident) => { false };

    ($($name:ident => ($id:literal, $title:literal, $render:ident, $pane:ident
        $(, visible: $visible:ident)?
        $(, needed: $needed:ident)?
        $(, prepare: $prepare:ident)?)),* $(,)?) => { $(
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
                    let visible = {
                        let app = app.read(cx);
                        panels!(@shown app, $id $(, $visible)?)
                            && panels!(@needed app $(, $needed)?)
                    };
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
                    visible: panels!(@at_startup Self::NAME, cx $(, $needed)?),
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
                let content = app.update(cx, |app, cx| {
                    $( app.$prepare(cx); )?
                    app.$render(window, cx).into_any_element()
                });
                pane_root(&app, Pane::$pane, content, cx).into_any_element()
            }
        }
    )*

        /// Registers every panel this macro builds.
        ///
        /// Generated from the same list as the types themselves: a panel
        /// declared above and forgotten here comes back from `layout.json` as
        /// "panel type is not registered", once per restart, and the only
        /// cure is to reset the view. Two lists would diverge on the first
        /// addition — this one cannot.
        pub fn register_generated(app: &Entity<ClaudhubApp>, cx: &mut App) {
            $(
                let handle = app.clone();
                declare_panel(cx, $id, move |_state, _window, cx| {
                    let handle = handle.clone();
                    panel_handle(cx.new(|cx| $name::new(&handle, cx)))
                });
            )*
        }
    };
}

panels! {
    ChangesPanel => ("ClaudhubChanges", "range-working", render_changes, Changes),
    BranchPanel => ("ClaudhubBranch", "range-branch", render_branch_review, Branch),
    NotesPanel => ("ClaudhubNotes", "panel-notes", render_notes, Notes),
    TagsPanel => ("ClaudhubTags", "panel-tags", render_tags, Tags),
    StashesPanel => ("ClaudhubStashes", "panel-stashes", render_stashes, Stashes),
    // The tab exists only where `vendor/bin/pest` does: on everything else the
    // honest panel is no panel — there is nothing to run.
    TestsPanel => ("ClaudhubTests", "panel-tests", render_pest, Tests, visible: tests_visible),
    // The run being followed. On the home screen its tab only shows once a
    // run exists, the console's rule.
    TestRunPanel => ("ClaudhubTestRun", "panel-test-run", render_test_run, TestRun, needed: test_run_open),
    FilesPanel => ("ClaudhubFiles", "panel-files", render_files, Files),
    DbPanel => ("ClaudhubDb", "panel-databases", render_db, Db),
    SqlHistoryPanel => ("ClaudhubSqlHistory", "panel-sql-history", render_sql_history, SqlHistory),
    SearchPanel => ("ClaudhubSearch", "panel-search", render_search, Search),
    SearchPreviewPanel => ("ClaudhubSearchPreview", "panel-search-preview", render_search_preview, SearchPreview, needed: search_preview_open),
    // The centre of each screen. **Three panels and not one whose title
    // changes**: they belonged to the same one because they were fighting over
    // the central slot, and a tab announcing "Diff", "Editor" or "SQL"
    // depending on the last gesture was saying plainly that it carried three.
    // The screens give each of them its own place.
    DiffPanel => ("ClaudhubDiff", "panel-diff", render_diff, Diff, needed: diff_on_screen),
    ConsolePanel => ("ClaudhubConsole", "panel-sql", render_console_panel, Console, needed: db_console_open),
    // The history needs loading the first time it is looked at.
    //
    // Doing it at render time rather than at construction is what avoids a
    // `git log` on a tab nobody will open; `ensure_history` only asks once,
    // otherwise every frame would restart the command.
    HistoryPanel => ("ClaudhubHistory", "panel-history", render_history, History, prepare: ensure_history),
    // The centre when **nothing** is open.
    //
    // No longer a constraint of the engine: `add_panel_view` splits an empty
    // region's root to place the first panel, so a centre with no tab has
    // somewhere to drop a file after all. What is left is a choice — a window
    // that opens on nothing at all says nothing of what it can do, and at a
    // first start the diff, the console and the preview are every one of them
    // conditional and therefore absent.
    //
    // So it stands for the whole centre and not for the editor alone, and it
    // steps aside as soon as anything arrives there.
    EditorPanel => ("ClaudhubEditor", "panel-editor", render_editor_panel, Editor, visible: empty_centre_visible),
}

/// One open file, as the dock shows it.
///
/// **A panel per file**, like a panel per terminal: the dock's bar is the tab
/// bar, which is what lets a file be dragged into a split, zoomed or sent
/// beside another without a strip of our own painted under the one the window
/// already has.
///
/// One panel and not one per screen, where a terminal has six: a file is only
/// read on the editing screen. It carries its worktree for the same reason a
/// terminal does — the tabs of the tree one is not looking at stay in the dock,
/// invisible, keeping the place they were given.
pub struct FilePanel {
    app: WeakEntity<ClaudhubApp>,
    root: std::path::PathBuf,
    path: std::path::PathBuf,
    /// What the tab says, worked out once: a path's last segment never moves,
    /// and the title is asked for on every frame the bar paints.
    name: gpui::SharedString,
    /// The editor's state, held rather than looked up: `focus_handle` and the
    /// tab's title are asked for by the dock at moments when reading the
    /// application would read it while it is being updated.
    input: Entity<gpui_component::input::EditorState>,
    group: Option<gpui::WeakEntity<gpui_component::dock::TabGroup>>,
    visible: bool,
}

impl FilePanel {
    pub const NAME: &'static str = "ClaudhubFile";

    /// `visible` is **given**, as a terminal panel's is: this runs inside an
    /// `update` on the application, which cannot be read from there.
    pub fn new(
        app: &Entity<ClaudhubApp>,
        root: std::path::PathBuf,
        path: std::path::PathBuf,
        input: Entity<gpui_component::input::EditorState>,
        visible: bool,
        cx: &mut Context<Self>,
    ) -> Self {
        let mine = root.clone();
        cx.observe(app, move |this: &mut Self, app, cx| {
            let app = app.read(cx);
            let visible = app.editing_root().as_deref() == Some(mine.as_path())
                && app.panel_visible(EditorPanel::NAME);
            if this.visible != visible {
                this.visible = visible;
                cx.emit(PanelEvent::LayoutChanged);
            }
            cx.notify();
        })
        .detach();
        let name = path
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_else(|| path.display().to_string());
        Self {
            app: app.downgrade(),
            root,
            path,
            name: name.into(),
            input,
            group: None,
            visible,
        }
    }

    /// Makes this file the displayed tab of its group.
    ///
    /// What restoring a session needs: the panels exist and sit in the right
    /// order, but the tab on screen is whichever was added last.
    pub fn activate(panel: &Entity<Self>, window: &mut Window, cx: &mut App) {
        let group = panel.read(cx).group.clone();
        select_own_tab(group, panel.entity_id(), window, cx);
    }

    /// Takes the editor state a reopening has built.
    ///
    /// Reading a file again — after a save, after an agent has written — makes
    /// a fresh `EditorState`, and the tab has to follow it or its title and its
    /// focus would speak for a text nobody holds.
    pub fn rebind(
        &mut self,
        input: Entity<gpui_component::input::EditorState>,
        cx: &mut Context<Self>,
    ) {
        self.input = input;
        cx.notify();
    }
}

impl Focusable for FilePanel {
    fn focus_handle(&self, cx: &App) -> FocusHandle {
        self.input.focus_handle(cx)
    }
}

impl EventEmitter<PanelEvent> for FilePanel {}

impl BasePanel for FilePanel {
    fn panel_name(&self) -> &'static str {
        Self::NAME
    }

    /// Closable, like a terminal: closing the tab is how one closes a file.
    fn closable(&self, _: &App) -> bool {
        true
    }

    fn visible(&self, _: &App) -> bool {
        self.visible
    }

    fn on_added_to(
        &mut self,
        group: gpui::WeakEntity<gpui_component::dock::TabGroup>,
        _: &mut Window,
        _: &mut Context<Self>,
    ) {
        self.group = Some(group);
    }

    /// **Deferred**, for the reason a terminal's is: this is called from inside
    /// the dock's own edit, and closing goes back through `DockArea::
    /// remove_panel` — the very area being updated.
    fn on_removed(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(app) = self.app.upgrade() else {
            return;
        };
        let (root, path) = (self.root.clone(), self.path.clone());
        cx.defer_in(window, move |_, window, cx| {
            app.update(cx, |app, cx| app.close_file(root, path, window, cx));
        });
    }
}

impl Panel for FilePanel {
    /// The file's name and a cross, as a terminal tab carries the program and
    /// its own.
    ///
    /// The name alone and not the path: a tab bar is read across, and
    /// `app/Http/Controllers/UserController.php` in six tabs is one long line
    /// of directories. The full path is one row below, in the file's bar.
    fn title(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let name = self.name.clone();
        let (dirty, ephemeral) = self
            .app
            .upgrade()
            .and_then(|app| {
                let app = app.read(cx);
                let tabs = app.editors(&self.root)?;
                let editing = tabs.open.get(tabs.index_of(&self.path)?)?;
                Some((editing.dirty, editing.ephemeral))
            })
            .unwrap_or((false, false));
        let app = self.app.clone();
        // The same closing as the cross's, and it has to be: the wheel button is
        // the other way of making the same gesture. **One closure shared by
        // both** — it used to be written twice, with a copy of the two paths
        // each, on every frame.
        let closing: Closing = {
            let (root, path) = (self.root.clone(), self.path.clone());
            std::rc::Rc::new(move |window: &mut Window, cx: &mut App| {
                let Some(app) = app.upgrade() else {
                    return;
                };
                let (root, path) = (root.clone(), path.clone());
                window.defer(cx, move |window, cx| {
                    app.update(cx, |app, cx| app.ask_close_file(root, path, window, cx));
                });
            })
        };
        let on_cross = closing.clone();
        let tab = gpui_component::h_flex()
            .id(("file-tab", self.input.entity_id()))
            .gap_1()
            .items_center()
            .child(crate::ui::file_icons::file_icon(&self.path, cx))
            // Italic for the preview tab, as VS Code's is: the tab is about to
            // be replaced, and one is entitled to know it before opening ten
            // files and finding one. A shape and not a colour — a colour in a
            // tab bar is read as selection.
            .child(div().when(ephemeral, |el| el.italic()).child(name))
            // A dot and not a coloured name: the tab of the file one is typing
            // in is already the selected one, and a colour there would be read
            // as selection rather than as "not saved".
            .when(dirty, |el| el.child(div().text_xs().child("•")))
            .child(
                Button::new("close-file")
                    .ghost()
                    .xsmall()
                    .icon(crate::ui::icons::icon("x"))
                    .on_click(move |_, window, cx| {
                        // The tab under it selects on click: without this, the
                        // cross would first bring forward the file it is about
                        // to close.
                        cx.stop_propagation();
                        on_cross(window, cx);
                    }),
            );
        close_on_middle_click(tab, move |window, cx| closing(window, cx))
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
        hide_view(&self.app, EditorPanel::NAME, menu)
    }
}

impl Render for FilePanel {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let Some(app) = self.app.upgrade() else {
            return div().into_any_element();
        };
        let (root, path) = (self.root.clone(), self.path.clone());
        let content = app.update(cx, |app, cx| {
            // **The panel that renders is the file being read.** A tab group
            // draws exactly one of its tabs, so being drawn is what says this
            // file is on screen — a fact to read off the frame rather than an
            // event to catch. Everything that follows from it — the language
            // server's open document, what the session files — happens here.
            app.show_file(root, path, window, cx);
            app.render_editor_panel(window, cx).into_any_element()
        });
        pane_root(&app, Pane::Editor, content, cx).into_any_element()
    }
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
            let visible = app.pending_operation().is_some() || app.has_conflicts();
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
            // `terminal_shown` answers the grid too: with every worktree's
            // terminals on, a panel shows itself whatever is being looked at.
            // It used to be a second kind of panel — the multiplexer's face —
            // built once and never told otherwise; it is a state now, read like
            // any other.
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
        let group = panel.read(cx).group.clone();
        select_own_tab(group, panel.entity_id(), window, cx);
    }
}

/// The "+" of the terminals: a shell, or one of the agent profiles.
///
/// One entry per profile, as the hand-painted strip used to offer: the menu is
/// the only place the choice arises, and a list coming from the settings saves
/// reopening them to launch something else.
///
/// It is used in two places, and it is the same button: the last tab's title,
/// where the row of terminals ends, and the status bar beside the toggle — the
/// corner they open on is also where one asks for one more, and with the
/// terminals hidden there is no tab bar left to ask from.
pub(super) fn new_terminal_button(
    app: &WeakEntity<ClaudhubApp>,
    worktree: Option<std::path::PathBuf>,
) -> impl IntoElement {
    let app = app.clone();
    Button::new("new-terminal")
        .ghost()
        .xsmall()
        .icon(crate::ui::icons::icon("plus"))
        .tooltip(tr!("terminal-new"))
        .dropdown_menu(move |menu, _window, cx| {
            let shell = app.clone();
            let here = worktree.clone();
            let profiles = Settings::global(cx).terminal.agents.clone();
            let menu = menu.item(
                PopupMenuItem::new(tr!("terminal-new"))
                    .icon(crate::ui::icons::icon("plus"))
                    .on_click(move |_, window, cx| {
                        open_terminal(&shell, here.clone(), None, window, cx);
                    }),
            );
            if profiles.is_empty() {
                return menu;
            }
            profiles
                .into_iter()
                .fold(menu.separator(), |menu, profile| {
                    let app = app.clone();
                    let here = worktree.clone();
                    let label = gpui::SharedString::from(profile.label().to_string());
                    menu.item(
                        PopupMenuItem::new(label)
                            .icon(crate::ui::icons::icon("bot"))
                            .on_click(move |_, window, cx| {
                                open_terminal(
                                    &app,
                                    here.clone(),
                                    Some(profile.clone()),
                                    window,
                                    cx,
                                );
                            }),
                    )
                })
        })
}

/// Opens a shell, or an agent profile, on the worktree being looked at.
fn open_terminal(
    app: &WeakEntity<ClaudhubApp>,
    // `None` means the worktree being looked at, which is what every bar but
    // the multiplexer's is about.
    worktree: Option<std::path::PathBuf>,
    profile: Option<crate::ui::settings::AgentProfile>,
    window: &mut Window,
    cx: &mut App,
) {
    let Some(app) = app.upgrade() else {
        return;
    };
    app.update(cx, |app, cx| {
        let Some(worktree) = worktree.or_else(|| app.active_path()) else {
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
        // With the grid on, the dock holds every terminal of the window, so the
        // tab is the only thing that can say which project this one is: the
        // repository greyed, then the worktree, the way the picker writes it.
        let in_grid = app
            .upgrade()
            .is_some_and(|app| app.read(cx).terminals_everywhere);
        let project = in_grid
            .then(|| {
                app.upgrade()
                    .map(|app| app.read(cx).project_label(&self.worktree))
            })
            .flatten();
        let muted = cx.theme().muted_foreground;
        let mine = self.worktree.clone();
        let go = app.clone();
        // The wheel button closes, like the cross — the confirmation included:
        // what dies with a terminal is a build half done, and the question does
        // not depend on which gesture asked.
        let closing = {
            let app = app.clone();
            move |window: &mut Window, cx: &mut App| {
                let Some(app) = app.upgrade() else {
                    return;
                };
                window.defer(cx, move |window, cx| {
                    app.update(cx, |app, cx| app.ask_close_terminal(id, window, cx));
                });
            }
        };
        // The listener goes on the row itself and before the menu wraps it: a
        // context menu is not an interactive element, and there is nothing to
        // hang a click on once it has.
        let tab = close_on_middle_click(
            gpui_component::h_flex()
                .id(("terminal-tab", id))
                .gap_1()
                .items_center(),
            closing,
        );
        tab
            // Renaming is a right click and not a double click: a double click
            // on a tab bar already means "zoom this group" everywhere else, and
            // the tab under this element consumes the plain click to select.
            .context_menu(move |menu, _window, _cx| {
                let app = rename.clone();
                let go = go.clone();
                let mine = mine.clone();
                let menu = menu.item(
                    gpui_component::menu::PopupMenuItem::new(tr!("terminal-rename"))
                        .icon(crate::ui::icons::icon("pencil"))
                        .on_click(move |_, window, cx| {
                            let Some(app) = app.upgrade() else {
                                return;
                            };
                            app.update(cx, |app, cx| app.ask_terminal_name(id, window, cx));
                        }),
                );
                // Only in the grid: everywhere else the tab already belongs to
                // the worktree one is looking at, and the entry would be a way
                // of going where one is.
                if !in_grid {
                    return menu;
                }
                menu.item(
                    gpui_component::menu::PopupMenuItem::new(tr!("multiplexer-open-worktree"))
                        .icon(crate::ui::icons::icon("arrow-right"))
                        .on_click(move |_, window, cx| {
                            let Some(app) = go.upgrade() else {
                                return;
                            };
                            app.update(cx, |app, cx| app.work_in_worktree(&mine, window, cx));
                        }),
                )
            })
            .when_some(project, |el, (repo, worktree)| {
                el.child(
                    gpui_component::h_flex()
                        .gap_1()
                        .items_center()
                        .text_color(muted)
                        .when_some(repo, |el, repo| {
                            el.child(repo)
                                .child(div().text_color(muted.opacity(0.5)).child("/"))
                        })
                        .child(worktree),
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
                            app.update(cx, |app, cx| app.ask_close_terminal(id, window, cx));
                        });
                    }),
            )
            // The "+" rides in **every** tab's title rather than sticking to
            // the right edge of the bar: the dock's bar offers no place for
            // it, and carried by the last tab alone it went out of sight as
            // soon as that tab did — a bar full of terminals scrolls.
            // In the grid it opens on **this tab's** worktree and not on the
            // one being looked at: the bar mixes the projects, so "the current
            // worktree" is not a thing one can read off it.
            .child(new_terminal_button(
                &self.app,
                in_grid.then(|| self.worktree.clone()),
            ))
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
    /// Whether this panel is one of the wide half's, read once from the
    /// manifest.
    ///
    /// It is what decides whether the home screen's rule applies to it — and
    /// the rule is for the **centre** and no other place, for a reason that is
    /// not taste: a plugin is started the first time one of its panels is
    /// *drawn* (`plugin_boot`), so a panel hidden for having nothing to show
    /// would never draw, never start the script, and never have anything to
    /// show. A plugin's list panel stays, boots the script, and its detail
    /// panel gets a tree — empty until one picks a line, which is exactly when
    /// its tab is worth a name.
    centre: bool,
}

impl PluginPanel {
    pub fn new(app: &Entity<ClaudhubApp>, name: &'static str, cx: &mut Context<Self>) -> Self {
        let centre = is_centre_panel(name);
        cx.observe(app, move |this: &mut Self, app, cx| {
            // Three reasons a plugin's tab is not there: hidden from the
            // "Views" menu like any other panel, the plugin switched off —
            // which is not the same gesture and does not live in the same file
            // — and, in the centre, nothing to show. The third is the `needed:`
            // rule of the macro, asked of a panel a script draws: it answers on
            // what it has painted, a script having no way to answer the
            // question itself.
            let visible = {
                let read = app.read(cx);
                read.panel_visible(this.name)
                    && crate::ui::plugin_view::panel_enabled(this.name, cx)
                    && !(this.centre && read.plugin_panel_empty(this.name))
            };
            if this.visible != visible {
                // A tab that has just come back is a tab one has just made
                // appear — picking an error in the list beside it. Bringing it
                // forward is what makes that gesture change the centre; the
                // group would otherwise go on showing the diff. Filed rather
                // than done: this runs between two frames, and selecting a tab
                // wants a window. `update` and not the entity we are observing
                // — it notifies nothing, so there is no loop.
                if visible && this.centre {
                    let name = this.name;
                    app.update(cx, |app, _| app.reveal_panel_later(name));
                }
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
            // Nothing has run when the window is built, so a centre panel opens
            // hidden — the `@at_startup` arm of the macro says the same thing
            // for the panels of ours.
            visible: visible_at_startup(name, cx)
                && crate::ui::plugin_view::panel_enabled(name, cx)
                && !centre,
            centre,
        }
    }
}

/// Does this plugin panel belong to the wide half. Unknown names — a plugin
/// removed between two sessions — are not, which keeps the tab there to be
/// pruned rather than hidden by a rule that cannot apply to it.
fn is_centre_panel(name: &str) -> bool {
    crate::ui::plugin_view::by_panel(name)
        .is_some_and(|(_, spec)| spec.place == crate::plugin::manifest::Place::Centre)
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
                .map(|(_, panel)| panel.title.clone())
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
