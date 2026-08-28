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

use gpui::{prelude::*, px, App, Context, Entity, Window};
use gpui_component::dock::{
    BasePanelView, DockArea, DockLayout, DockPlacement, PanelInfo, PanelState,
};

use crate::ui::rails::{self, Anchor, Half, Side};

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

/// One half of one edge, in the table's order, followed by the plugin panels
/// that asked for it.
fn tools_of(anchor: Anchor, window: &mut Window, cx: &mut Context<DockArea>) -> DockLayout {
    let mut group = DockLayout::tabs();
    for tool in rails::TOOLS.iter().filter(|tool| tool.home == anchor) {
        if let Some(view) = build(tool.panel, window, cx) {
            group = group.panel_view(view, cx);
        }
    }
    for name in plugin_panels(Some(anchor)) {
        if let Some(view) = build(name, window, cx) {
            group = group.panel_view(view, cx);
        }
    }
    group
}

/// A whole edge: its halves, stacked, or a single group where it has one.
///
/// **Two groups and not two tabs**: the halves are on screen together, which is
/// what lets the file list and the tests be read at once without either being a
/// tab of the other. It is the arrangement the review's column already had, now
/// said once for every edge.
fn edge(side: Side, window: &mut Window, cx: &mut Context<DockArea>) -> DockLayout {
    let halves = side.halves();
    if halves.len() == 1 {
        return tools_of(Anchor::new(side, halves[0]), window, cx);
    }
    let mut split = DockLayout::v_split();
    for (ix, half) in halves.iter().enumerate() {
        let group = tools_of(Anchor::new(side, *half), window, cx);
        // The **bottom** half is the one given a size, never the top: two fixed
        // sizes adding up to the region's height overflow it, and it is the top
        // that should take what is left over. A **height**, which is not the
        // zone's width — the split inside a side zone runs down.
        let size = (ix > 0).then(|| side.half_size());
        split = split.child(group, size);
    }
    split
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
fn plugin_panels(anchor: Option<Anchor>) -> Vec<&'static str> {
    // Where a manifest's `place` lands is `rails`' answer and not a second one
    // here: the rail and the layout have to agree about a plugin's home, and
    // two readings of one field are two readings to keep in step.
    crate::ui::plugin_view::manifests()
        .iter()
        .flat_map(|manifest| manifest.panels.iter())
        .filter(|spec| Anchor::of_place(spec.place) == anchor)
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
        let tools = edge(side, window, cx);
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
        anchor: Option<Anchor>,
        open: bool,
        cx: &App,
        out: &mut Vec<rails::Seat>,
    ) {
        match node.kind() {
            PaneRef::Tabs { panels, active_ix } => {
                // **What the group draws**, and not simply its active index:
                // the displayed panel is the active one *if it is visible*, and
                // the first visible one otherwise. Reading the index alone left
                // a rail saying one view was on screen while another was.
                let displayed = panels
                    .get(active_ix)
                    .filter(|id| area.panel(**id).is_some_and(|panel| panel.visible(cx)))
                    .or_else(|| {
                        panels
                            .iter()
                            .find(|id| area.panel(**id).is_some_and(|panel| panel.visible(cx)))
                    })
                    .copied();
                for id in panels.iter() {
                    let Some(panel) = area.panel(*id) else {
                        continue;
                    };
                    out.push(rails::Seat {
                        panel: panel.panel_name(cx).to_string(),
                        anchor,
                        shown: displayed == Some(*id),
                        open,
                        visible: panel.visible(cx),
                    });
                }
            }
            PaneRef::Split { children, .. } => {
                for (ix, child) in children.iter().enumerate() {
                    // **The half is read off the tree and nowhere else.** A
                    // region split in two is its two halves, in order; deeper
                    // than that — a user's own split inside one of them — keeps
                    // the half it is in. What decides is position, which is what
                    // the layout wrote and what a drag rewrites.
                    let anchor = anchor.map(|anchor| match children.len() {
                        1 => anchor,
                        _ if ix == 0 => Anchor::new(anchor.side, Half::Top),
                        _ => Anchor::new(anchor.side, Half::Bottom),
                    });
                    walk(child, area, anchor, open, cx, out);
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
        // The top half until a split says otherwise — an edge holding one group
        // has no second half to be in.
        let anchor = side_of(placement).map(|side| Anchor::new(side, Half::Top));
        walk(tree.root(), area, anchor, open, cx, &mut out);
    }
    out
}

/// Where a tool window should be inserted to land on one anchor.
///
/// `None` when the region does not exist yet: the caller then adds the panel to
/// the region itself, which the area makes on the way.
fn target_for(area: &DockArea, anchor: Anchor) -> Option<gpui_component::dock::InsertTarget> {
    use gpui_component::dock::{InsertTarget, PaneRef};

    let tree = area.layout(placement_of(anchor.side))?;
    let root = tree.root();
    let join = |node| InsertTarget::Tabs {
        node,
        ix: None,
        // What one has just sent somewhere is what one wants to look at.
        activate: true,
    };
    match root.kind() {
        // Already two halves — or more, the user having split one further. The
        // first slot is the top, the last is the bottom.
        PaneRef::Split { children, .. } if children.len() > 1 => {
            let child = match anchor.half {
                Half::Top => children.first(),
                Half::Bottom => children.last(),
            }?;
            Some(join(first_group(child)?))
        }
        // One group, and no way to tell which half it stands for: it is read as
        // the top, so the bottom is a slot to be made below it.
        _ => {
            let node = first_group(root).unwrap_or_else(|| root.id());
            match anchor.half {
                Half::Top => Some(join(node)),
                Half::Bottom => Some(InsertTarget::Split {
                    node,
                    placement: gpui_base::Placement::Bottom,
                    size: Some(anchor.side.half_size()),
                }),
            }
        }
    }
}

/// Sends a panel to one anchor, wherever the dock holds it now.
///
/// It is a **move** and not an add when the dock already has the panel: adding
/// one it holds would register the same id twice, and what the caller means is
/// "put this there", not "make another".
pub fn move_to(
    dock: &Entity<DockArea>,
    panel: &str,
    anchor: Anchor,
    window: &mut Window,
    cx: &mut App,
) {
    dock.update(cx, |area: &mut DockArea, cx: &mut Context<DockArea>| {
        let id = seat_id(area, panel, cx);
        // The handle the dock already holds, so a move keeps the panel's
        // identity. Building a second one for a panel already in the tree would
        // put the same view in twice, under two ids.
        let held = id.and_then(|id| area.panel(id).cloned());
        match (id, target_for(area, anchor)) {
            (Some(id), Some(target)) => area.move_panel(id, target, window, cx),
            // Out of the tree, or the edge does not exist yet: the add makes
            // the region on the way, and the move that follows puts it in the
            // half asked for — a fresh region has one group, which is its top.
            _ => {
                let Some(handle) = held.or_else(|| build(panel, window, cx)) else {
                    return;
                };
                let id = handle.panel_id(cx);
                let size = Some(anchor.side.default_size());
                area.add_panel_view(handle, placement_of(anchor.side), size, window, cx);
                if anchor.half == Half::Bottom {
                    if let Some(target) = target_for(area, anchor) {
                        area.move_panel(id, target, window, cx);
                    }
                }
            }
        }
    });
}

/// The dock's id for a panel of this name, if it holds one.
///
/// **By name**, as everything else here: a layout read back from `layout.json`
/// was never built by us, so nothing outside the tree holds its entity.
fn seat_id(area: &DockArea, panel: &str, cx: &App) -> Option<gpui_component::dock::PanelId> {
    use gpui_component::dock::{PaneNode, PaneRef};

    fn walk(
        node: &PaneNode,
        area: &DockArea,
        panel: &str,
        cx: &App,
    ) -> Option<gpui_component::dock::PanelId> {
        match node.kind() {
            PaneRef::Tabs { panels, .. } => panels.iter().copied().find(|id| {
                area.panel(*id)
                    .is_some_and(|held| held.panel_name(cx) == panel)
            }),
            PaneRef::Split { children, .. } => children
                .iter()
                .find_map(|child| walk(child, area, panel, cx)),
            PaneRef::Tiles { .. } => None,
        }
    }

    [
        DockPlacement::Center,
        DockPlacement::Left,
        DockPlacement::Right,
        DockPlacement::Bottom,
    ]
    .into_iter()
    .find_map(|placement| walk(area.layout(placement)?.root(), area, panel, cx))
}

/// The first tab group of a subtree, in depth order.
fn first_group(node: &gpui_component::dock::PaneNode) -> Option<gpui_component::dock::NodeId> {
    use gpui_component::dock::PaneRef;
    match node.kind() {
        PaneRef::Tabs { .. } => Some(node.id()),
        PaneRef::Split { children, .. } => children.iter().find_map(first_group),
        PaneRef::Tiles { .. } => None,
    }
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
        // **A bare flex row and not `h_flex`**: that helper centres its
        // children (`items_center`), so the middle column would take the height
        // of its content — and its content is a `flex_1` resolving against an
        // undefined height, which is zero. The whole window came up empty, rails
        // and nothing else. A row stretches its children by default, which is
        // what a column between two rails needs.
        gpui::div()
            .flex()
            .flex_row()
            .flex_1()
            .min_h_0()
            .min_w_0()
            .child(self.render_rail(&rails[Side::Left.index()], cx))
            .child(
                gpui::div()
                    .flex_1()
                    .min_h_0()
                    .min_w_0()
                    .py(px(4.))
                    .px(px(8.))
                    .child(self.dock.clone()),
            )
            .child(self.render_rail(&rails[Side::Right.index()], cx))
    }

    /// The bottom edge's buttons, for the status bar to carry.
    ///
    /// **Merged with the status bar rather than a strip of its own.** The two
    /// would have followed each other — thirty pixels between them to hold a
    /// handful of icons, two grey bands stacked under the window where the dock
    /// fights for every line — and they say the same kind of thing anyway. It
    /// is the observation that moved the screen picker down here in the first
    /// place, and it outlived the picker.
    ///
    /// No background of its own: the bar has one.
    pub(super) fn render_bottom_tools(&mut self, cx: &mut Context<Self>) -> gpui::AnyElement {
        let seats = self.seats(cx);
        let hidden: std::collections::BTreeSet<String> =
            self.hidden_panels.iter().cloned().collect();
        let rail = rails::rails(&seats, &hidden)
            .into_iter()
            .nth(Side::Bottom.index())
            .expect("three rails");
        let buttons = self.rail_buttons(&rail.top, cx);
        if buttons.is_empty() {
            return gpui::Empty.into_any_element();
        }
        gpui_component::h_flex()
            .flex_none()
            .gap(px(2.))
            .children(buttons)
            .into_any_element()
    }

    /// One rail: a button per tool window that edge holds.
    ///
    /// An empty rail paints **nothing at all**, not an empty band: the bottom
    /// one is a horizontal strip under the centre, and a permanent grey line
    /// there for a window with no terminal and no run would be thirty pixels
    /// spent saying nothing.
    /// The buttons of one rail, shared by the two edges and the status bar.
    fn rail_buttons(
        &mut self,
        buttons: &[rails::Button],
        cx: &mut Context<Self>,
    ) -> Vec<gpui::AnyElement> {
        use gpui_component::button::{Button, ButtonVariants as _};
        use gpui_component::menu::ContextMenuExt as _;
        use gpui_component::Sizable as _;

        let seats = self.seats(cx);
        let app = cx.entity();
        buttons
            .iter()
            .map(|button| {
                let panel = button.panel;
                Button::new(gpui::SharedString::from(panel))
                    .icon(crate::ui::icons::icon(button.icon))
                    .tooltip(button.title.text())
                    .small()
                    // **Solid against ghost**, the polarity of both ends of the
                    // status bar: the "selected" state of a ghost button is a
                    // background a few percent from the ground, invisible on
                    // half the themes — and "what is on screen" is exactly the
                    // question a rail has to answer without being looked for.
                    .map(|this| {
                        if button.active {
                            this.primary()
                        } else {
                            this.ghost()
                        }
                    })
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.press_tool(panel, window, cx);
                    }))
                    // **Right click moves it.** Dragging works and is what one
                    // reaches for with a panel already on screen; a tool window
                    // one has folded away has no title to take hold of, and
                    // sending it to the other edge would mean opening it,
                    // dragging it, and folding it again.
                    .context_menu({
                        let app = app.clone();
                        let targets = rails::moves(panel, &seats);
                        move |menu, _window, _cx| {
                            targets.iter().fold(menu, |menu, anchor| {
                                let (app, anchor) = (app.clone(), *anchor);
                                menu.item(
                                    gpui_component::menu::PopupMenuItem::new(crate::tr!(
                                        anchor.label()
                                    ))
                                    .on_click(
                                        move |_, window, cx| {
                                            app.update(cx, |app, cx| {
                                                app.move_tool(panel, anchor, window, cx)
                                            });
                                        },
                                    ),
                                )
                            })
                        }
                    })
                    .into_any_element()
            })
            .collect()
    }

    /// One side rail: two runs of buttons against the window's edge.
    ///
    /// The top half is pinned to the start and the bottom half pushed to the
    /// end, which is what makes the two legible as two: a single run of nine
    /// buttons says nothing about which of them can be on screen together.
    fn render_rail(&mut self, rail: &rails::Rail, cx: &mut Context<Self>) -> gpui::AnyElement {
        use gpui_component::{v_flex, ActiveTheme as _};

        if rail.is_empty() {
            // Nothing to show, nothing painted — not an empty band.
            return gpui::Empty.into_any_element();
        }
        let top = self.rail_buttons(&rail.top, cx);
        let bottom = self.rail_buttons(&rail.bottom, cx);
        let run =
            |buttons: Vec<gpui::AnyElement>| v_flex().flex_none().gap(px(2.)).children(buttons);
        // The flex is returned directly rather than wrapped: a `div()` is a
        // **block**, where a child's `h_full` resolves against an undefined
        // height.
        v_flex()
            .h_full()
            .flex_none()
            .justify_between()
            // Against the window's ground and not the cards': a rail is
            // furniture at the edge, and painting it in the tone the cards sit
            // on would make it read as one more card.
            .bg(cx.theme().title_bar)
            .py(px(6.))
            .px(px(4.))
            // A rail does not scroll: one that has to be scrolled is one that
            // can no longer be aimed at. `rails::MAX_PER_RAIL` keeps the table
            // inside what an edge can show.
            .overflow_hidden()
            .child(run(top))
            .child(run(bottom))
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
