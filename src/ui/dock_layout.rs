//! The one dock area: its default arrangement, and what it says about itself.
//!
//! There used to be nine areas, one per screen, and a screen was chosen from a
//! bar. There is one now, and what a screen used to be is an arrangement of
//! tool windows around a centre of documents — the shape of every editor that
//! keeps its users in a single window.
//!
//! Three things live here, and nothing else does:
//!
//! - **the default layout**, built from `rails::TOOLS` and from what the
//!   plugins' manifests ask for. One list, read once: the tools table names the
//!   panels, the `panels!` macro registers how to build them, and this module
//!   asks the dock's registry — so a panel added to one is not missing from the
//!   other;
//! - **`seats`**, which flattens the area's tree into what `ui::rails` needs.
//!   It is read every frame and kept nowhere, which is why a panel dragged from
//!   one edge to the other changes rail with nothing to update;
//! - **the gestures**: pressing a rail button, folding a zone, the zen fold.
//!
//! What it deliberately does not hold is any second opinion about where a panel
//! is. The tree answers that, always.

use gpui::{prelude::*, px, App, Context, Window};
use gpui_component::dock::{
    BasePanelView, DockArea, DockLayout, DockPlacement, PanelInfo, PanelState,
};

use crate::ui::rails::{self, Side};

/// The area's id, under which gpui files what it keeps for it.
///
/// A constant and no longer one id per screen: there is one area, and the name
/// it is filed under has no reason to move again.
pub const DOCK_ID: &str = "claudhub";

/// The dock placement of an edge.
pub fn placement_of(side: Side) -> DockPlacement {
    match side {
        Side::Left => DockPlacement::Left,
        Side::Right => DockPlacement::Right,
        Side::Bottom => DockPlacement::Bottom,
    }
}

/// The edge of a dock placement, `None` for the centre.
fn side_of(placement: DockPlacement) -> Option<Side> {
    match placement {
        DockPlacement::Left => Some(Side::Left),
        DockPlacement::Right => Some(Side::Right),
        DockPlacement::Bottom => Some(Side::Bottom),
        DockPlacement::Center => None,
    }
}

/// Where a panel goes when it has to be put back: its edge if it is a tool
/// window, the centre if it is a document.
///
/// A document has no rail button, so it is never *restored* by a press — but
/// it is opened by a gesture that finds it absent from the tree, which is the
/// same question.
pub fn home_of(panel: &str) -> DockPlacement {
    rails::tool(panel)
        .map(|tool| placement_of(tool.home))
        .unwrap_or(DockPlacement::Center)
}

/// Builds a panel from the dock's own registry, by name.
///
/// **The registry and not a match of our own**, and that is what keeps the
/// tools table honest: `panels!` declares the builders from the same list that
/// declares the types, so a panel `rails::TOOLS` names is one that can be
/// built, or one that was never a panel at all. A `match` here would be the
/// second list this whole module exists to avoid.
///
/// `None` for a name nothing builds — the terminals, whose content is a
/// **process**, and the open files, whose content is a text on the disk. Both
/// are deliberately unregistered; see `panels::register`.
pub fn build(
    name: &str,
    window: &mut Window,
    cx: &mut Context<DockArea>,
) -> Option<std::sync::Arc<dyn BasePanelView>> {
    let area = cx.entity().downgrade();
    let state = PanelState::new(name);
    let info = PanelInfo::panel(serde_json::Value::Null);
    let context = gpui_component::dock::PanelBuildContext::new(area, &state, &info);
    gpui_component::dock::PanelRegistry::build_panel(name, context, window, cx)
}

/// The tool windows of one edge, in the table's order, followed by the plugin
/// panels that asked for it.
fn tools_of(side: Side, window: &mut Window, cx: &mut Context<DockArea>) -> DockLayout {
    let mut group = DockLayout::tabs();
    for tool in rails::TOOLS.iter().filter(|tool| tool.home == side) {
        if let Some(view) = build(tool.panel, window, cx) {
            group = group.panel_view(view, cx);
        }
    }
    for name in plugin_panels(Some(side)) {
        if let Some(view) = build(name, window, cx) {
            group = group.panel_view(view, cx);
        }
    }
    group
}

/// The centre: what one reads, as against what one picks from.
fn documents(window: &mut Window, cx: &mut Context<DockArea>) -> DockLayout {
    let mut group = DockLayout::tabs();
    for name in DOCUMENTS {
        if let Some(view) = build(name, window, cx) {
            group = group.panel_view(view, cx);
        }
    }
    for name in plugin_panels(None) {
        if let Some(view) = build(name, window, cx) {
            group = group.panel_view(view, cx);
        }
    }
    group
}

/// The centre's own panels, in the order their tabs sit.
///
/// Not a table like `rails::TOOLS`, and for a reason: a document has no button
/// to be ranked among, and nothing outside this list asks about it. The open
/// files join them as tabs of their own, and so does the settings form the
/// first time it is called up.
const DOCUMENTS: &[&str] = &[
    "ClaudhubDiff",
    "ClaudhubEditor",
    "ClaudhubSearchPreview",
    "ClaudhubConsole",
];

/// The plugin panels that asked for one place, in manifest order.
fn plugin_panels(side: Option<Side>) -> Vec<&'static str> {
    use crate::plugin::manifest::Place;
    let wanted = |place: Place| match place {
        Place::Left => Some(Side::Left),
        Place::Right => Some(Side::Right),
        Place::Bottom => Some(Side::Bottom),
        Place::Centre => None,
    };
    crate::ui::plugin_view::manifests()
        .iter()
        .flat_map(|manifest| manifest.panels.iter())
        .filter(|spec| wanted(spec.place) == side)
        .map(|spec| spec.name)
        .collect()
}

/// Gives the window the arrangement it opens on the first time.
///
/// The escape hatch of a system where everything moves: a panel dragged out of
/// sight has no other way back. It rebuilds the whole window now rather than
/// one screen out of nine, which is what makes it worth reaching for — and the
/// rails put a folded zone one click from its return, so the two uses a saved
/// arrangement would have had are both covered.
pub fn install_default_layout(
    area: &mut DockArea,
    window: &mut Window,
    cx: &mut Context<DockArea>,
) {
    // **The three zones go first, whatever they hold.** `set_dock` keeps the
    // size and the open state of the zone it refills, so a left column dragged
    // to a sliver and then folded came back a folded sliver. Removed, they are
    // rebuilt below: open, at the size the edge declares.
    for side in Side::ALL {
        area.remove_dock(placement_of(side), window, cx);
    }

    let centre = documents(window, cx);
    area.set_center(centre, window, cx);

    for side in Side::ALL {
        let tools = tools_of(side, window, cx);
        area.set_dock(placement_of(side), tools, window, cx);
        area.set_dock_size(placement_of(side), side.default_size(), window, cx);
    }

    // The three open. Which of them a fresh window ought to greet you with is
    // the rails' business, and a zone folded before they exist would be a zone
    // with no way back.
    cx.notify();
}

/// What the dock's tree says of every panel it holds — the rails' only input,
/// and the reason there is no second list.
///
/// Walked every frame: four regions, a couple of dozen nodes, one table lookup
/// each. Cheaper than keeping anything in step, and it cannot be wrong.
pub fn seats(area: &DockArea, cx: &App) -> Vec<rails::Seat> {
    use gpui_component::dock::{PaneNode, PaneRef};

    fn walk(
        node: &PaneNode,
        area: &DockArea,
        side: Option<Side>,
        open: bool,
        cx: &App,
        out: &mut Vec<rails::Seat>,
    ) {
        match node.kind() {
            PaneRef::Tabs { panels, active_ix } => {
                for (ix, id) in panels.iter().enumerate() {
                    let Some(panel) = area.panel(*id) else {
                        continue;
                    };
                    out.push(rails::Seat {
                        panel: panel.panel_name(cx).to_string(),
                        side,
                        shown: ix == active_ix,
                        open,
                    });
                }
            }
            PaneRef::Split { children, .. } => {
                for child in children {
                    walk(child, area, side, open, cx, out);
                }
            }
            // A tile is its own window inside the region; nothing here docks
            // into one, and a rail has nothing to say about it.
            PaneRef::Tiles { .. } => {}
        }
    }

    let mut out = Vec::new();
    for placement in [
        DockPlacement::Center,
        DockPlacement::Left,
        DockPlacement::Right,
        DockPlacement::Bottom,
    ] {
        let Some(tree) = area.layout(placement) else {
            continue;
        };
        // The centre is never folded: it is what the zones are arranged around.
        let open = placement == DockPlacement::Center || area.is_dock_open(placement);
        walk(tree.root(), area, side_of(placement), open, cx, &mut out);
    }
    out
}

impl crate::ui::app::ClaudhubApp {
    /// The three rails, and the area between them.
    ///
    /// **The rails are ours and sit outside the area**, which is the whole
    /// reason a zone can fold to nothing while its buttons stay. The dock's own
    /// affordance is a chevron in a tab bar — it names the zone next door, and
    /// it goes away with the zone it names, so what it hid could only be got
    /// back through a different group's bar. That is why `set_toggle_button_visible`
    /// is off.
    ///
    /// The padding is inside the middle column so the rails touch the window's
    /// edges: a rail set in from the side is one to be aimed at rather than one
    /// the pointer can be thrown at.
    pub(super) fn render_workspace(&mut self, cx: &mut Context<Self>) -> impl IntoElement {
        let seats = self.seats(cx);
        let hidden: std::collections::BTreeSet<String> =
            self.hidden_panels.iter().cloned().collect();
        let rails = rails::rails(&seats, &hidden);
        gpui_component::h_flex()
            .flex_1()
            .min_h_0()
            .min_w_0()
            .child(self.render_rail(&rails[Side::Left.index()], cx))
            .child(
                gpui_component::v_flex()
                    .flex_1()
                    .min_w_0()
                    .min_h_0()
                    .child(
                        gpui::div()
                            .flex_1()
                            .min_h_0()
                            .min_w_0()
                            .py(px(4.))
                            .px(px(8.))
                            .child(self.dock.clone()),
                    )
                    .child(self.render_rail(&rails[Side::Bottom.index()], cx)),
            )
            .child(self.render_rail(&rails[Side::Right.index()], cx))
    }

    /// One rail: a button per tool window that edge holds.
    ///
    /// An empty rail paints **nothing at all**, not an empty band: the bottom
    /// one is a horizontal strip under the centre, and a permanent grey line
    /// there for a window with no terminal and no run would be thirty pixels
    /// spent saying nothing.
    fn render_rail(&mut self, rail: &rails::Rail, cx: &mut Context<Self>) -> impl IntoElement {
        use gpui_component::button::{Button, ButtonVariants as _};
        use gpui_component::{h_flex, v_flex, ActiveTheme as _, Selectable as _, Sizable as _};

        let vertical = rail.side != Side::Bottom;
        let muted = cx.theme().muted_foreground;
        let buttons: Vec<_> = rail
            .buttons
            .iter()
            .map(|button| {
                let panel = button.panel;
                Button::new(("rail", panel.as_ptr() as usize))
                    .icon(crate::ui::icons::icon(button.icon))
                    .tooltip(crate::tr!(button.title))
                    .ghost()
                    .xsmall()
                    .selected(button.active)
                    // A hidden view keeps its button, muted: it is where one
                    // calls it back from, and a button that vanished would be a
                    // target that moves.
                    .when(button.dimmed, |this| this.text_color(muted))
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.press_tool(panel, window, cx);
                    }))
            })
            .collect();
        gpui::div()
            .when(!buttons.is_empty(), |this| {
                this.map(|this| {
                    if vertical {
                        this.child(
                            v_flex()
                                .h_full()
                                .flex_none()
                                .gap(px(2.))
                                .py(px(6.))
                                .px(px(4.))
                                // A rail does not scroll: one that has to be
                                // scrolled is one that can no longer be aimed
                                // at. `rails::MAX_PER_RAIL` is what keeps the
                                // table inside what an edge can show, and a
                                // test holds it there.
                                .overflow_hidden()
                                .children(buttons),
                        )
                    } else {
                        this.child(
                            h_flex()
                                .w_full()
                                .flex_none()
                                .gap(px(2.))
                                .px(px(8.))
                                .pb(px(4.))
                                .overflow_hidden()
                                .children(buttons),
                        )
                    }
                })
            })
            .into_any_element()
    }

    /// Folds the three zones away, and gives back exactly what was unfolded.
    ///
    /// What every editor calls a distraction-free mode, and the reason it is
    /// one gesture rather than three: the point is the centre, not the zones.
    /// Pressed on an already bare window it gives the last fold back — a zen
    /// one cannot leave would be a trap.
    pub(super) fn toggle_zen(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let open = Side::ALL.map(|side| self.dock.read(cx).is_dock_open(placement_of(side)));
        let folded = std::mem::take(&mut self.zen_folded);
        let (wanted, taken) = rails::zen(open, &folded);
        self.zen_folded = taken;
        for side in Side::ALL {
            if open[side.index()] != wanted[side.index()] {
                self.toggle_zone(side, window, cx);
            }
        }
        cx.notify();
    }
}
