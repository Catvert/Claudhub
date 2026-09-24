//! The home screen: every worktree as a card on a plane one pans and zooms,
//! and every open terminal beside the card of its worktree.
//!
//! It is a **screen** and not a panel, the multiplexer's reason and the
//! multiplexer's place: watching five agents means watching five terminals
//! at the same time, and a dock shows one tab of a group. It took the
//! multiplexer's place because it answers the same question — which of the
//! agents I left running has finished — and the one beside it: where each
//! worktree stands, what it changed, what it committed. A terminal read beside
//! its worktree's card says both at once, where a strip of columns said only
//! the first.
//!
//! **The zoom is the plane's, not the layout's**: what stands where is
//! decided by `ui::overview`, pure and tested, and zooming only changes how
//! close one looks. The cards' text follows because everything in them is
//! measured in `rem`, and the canvas lends each card a `rem` scaled by the
//! zoom (`Scaled`); the terminals' font follows through `set_canvas`, and
//! their grid does not move.
//!
//! **Level of detail**: below `DETAIL` a card drops its buttons and its commit
//! list. They would still be there, at a size nobody can aim at, and a card
//! seen from afar is read for its name, its branch and its agent's word.

use std::path::{Path, PathBuf};

use gpui_kit::component::{
    button::{Button, ButtonVariants as _},
    h_flex,
    menu::{DropdownMenu as _, PopupMenuItem},
    v_flex, ActiveTheme, Disableable as _, Sizable as _, WindowExt as _,
};
use gpui_kit::{
    canvas, div, point, prelude::*, px, AnyElement, App, Context, Element, Focusable as _,
    GlobalElementId, InspectorElementId, LayoutId, MouseButton, MouseDownEvent, MouseMoveEvent,
    PathBuilder, Pixels, ScrollWheelEvent, SharedString, WeakEntity, Window,
};

use super::app::ClaudhubApp;
use super::icons::icon;
use super::overview::{self, Checkout, Group, LinkKind, Node, Plan, Rect, View};
use super::terminal_view::{Canvas, Launch};
use crate::tr;

/// Below this zoom, cards are read and not handled.
const DETAIL: f32 = 0.55;
/// The commits a card lists.
const COMMITS: usize = 4;

/// Lays a child out, and paints it, under a `rem` of its own.
///
/// gpui has the mechanism (`Window::with_rem_size`) and no element for it:
/// everything in a card that is measured in `rem` — the text, the paddings,
/// the buttons of gpui-component — grows and shrinks with the plane, and what
/// is in pixels does not. The card's own box is in pixels on purpose: its
/// place and size are the plane's, already scaled.
struct Scaled {
    rem: Pixels,
    child: AnyElement,
}

impl IntoElement for Scaled {
    type Element = Self;

    fn into_element(self) -> Self::Element {
        self
    }
}

impl Element for Scaled {
    type RequestLayoutState = ();
    type PrepaintState = ();

    fn id(&self) -> Option<gpui_kit::ElementId> {
        None
    }

    fn source_location(&self) -> Option<&'static std::panic::Location<'static>> {
        None
    }

    fn request_layout(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        window: &mut Window,
        cx: &mut App,
    ) -> (LayoutId, ()) {
        let child = &mut self.child;
        let layout =
            window.with_rem_size(Some(self.rem), |window| child.request_layout(window, cx));
        (layout, ())
    }

    fn prepaint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        _: gpui_kit::Bounds<Pixels>,
        _: &mut (),
        window: &mut Window,
        cx: &mut App,
    ) {
        let child = &mut self.child;
        window.with_rem_size(Some(self.rem), |window| child.prepaint(window, cx));
    }

    fn paint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        _: gpui_kit::Bounds<Pixels>,
        _: &mut (),
        _: &mut (),
        window: &mut Window,
        cx: &mut App,
    ) {
        let child = &mut self.child;
        window.with_rem_size(Some(self.rem), |window| child.paint(window, cx));
    }
}

/// A link as the canvas paints it: its two ends on screen, its kind, and
/// whether it leads into the worktree on show.
type Segment = (overview::Link, bool);

/// What the pointer is dragging on the home screen.
#[derive(Clone, Debug)]
pub(super) enum Drag {
    /// The plane itself — the left button on bare ground, or the middle one
    /// anywhere.
    Plane(gpui_kit::Point<Pixels>),
    /// A node, by its head.
    Node(Node, gpui_kit::Point<Pixels>),
    /// A scrollbar's thumb.
    Thumb(Axis, gpui_kit::Point<Pixels>),
    /// A node, by its bottom-right corner.
    Resize(Node, gpui_kit::Point<Pixels>),
}

#[derive(Clone, Copy, Debug)]
pub(super) enum Axis {
    Horizontal,
    Vertical,
}

/// A box of the plane, placed on the canvas.
fn placed(rect: Rect) -> gpui_kit::Div {
    div()
        .absolute()
        .left(px(rect.x))
        .top(px(rect.y))
        .w(px(rect.w))
        .h(px(rect.h))
}

impl ClaudhubApp {
    /// The repositories the plane shows: the one picked in the corner, the
    /// active worktree's until one is, or all of them.
    fn overview_repos(&self) -> Vec<&crate::ui::repos::RepoState> {
        if self.overview_all {
            return self.repos.iter().collect();
        }
        let picked = self
            .overview_repo
            .clone()
            .filter(|main| self.repos.iter().any(|repo| &repo.main == main))
            .or_else(|| {
                self.active
                    .as_deref()
                    .and_then(|active| self.main_of(active))
            });
        match picked {
            Some(main) => self.repos.iter().filter(|repo| repo.main == main).collect(),
            None => self.repos.iter().take(1).collect(),
        }
    }

    /// What the plane holds, laid out.
    fn overview_plan(&self, cx: &App) -> Plan {
        let notes = &super::store::Store::global(cx).home_notes;
        let hung = |anchor: super::store::HomeAnchor| -> Vec<u64> {
            notes
                .iter()
                .filter(|note| note.anchor == anchor)
                .map(|note| note.id)
                .collect()
        };
        let groups: Vec<Group> = self
            .overview_repos()
            .into_iter()
            .map(|repo| Group {
                main: &repo.main,
                notes: hung(super::store::HomeAnchor::Repo(repo.main.clone())),
                checkouts: repo
                    .worktrees
                    .iter()
                    // A folder gone from the disk has nothing to show but its
                    // absence, which the picker already says.
                    .filter(|worktree| !worktree.prunable)
                    .map(|worktree| Checkout {
                        path: &worktree.path,
                        branch: worktree.branch.as_deref(),
                        is_main: worktree.is_main,
                        base: self
                            .outlines
                            .get(&worktree.path)
                            .and_then(|outline| outline.base.as_deref()),
                        terminals: self
                            .terminals
                            .iter()
                            .filter(|terminal| terminal.worktree == worktree.path)
                            .map(|terminal| (terminal.view.entity_id().as_u64(), terminal.size))
                            .collect(),
                        notes: hung(super::store::HomeAnchor::Worktree(worktree.path.clone())),
                    })
                    .collect(),
            })
            .collect();
        overview::plan(&groups, &self.overview_moved, &self.overview_sizes)
    }

    fn overview_size(&self) -> (f32, f32) {
        let (_, _, w, h) = self.overview_viewport.get();
        (w, h)
    }

    /// The home screen, in place of the whole workspace.
    pub(super) fn render_overview(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let plan = self.overview_plan(cx);
        let size = self.overview_size();
        // Everything in view the first time — and after picking another
        // project — once the canvas has a size: its first frame has none, and
        // the measure below asks for a second.
        if !self.overview_fitted && size.0 > 0. {
            self.overview_view = View::fit(plan.bounds, size);
            self.overview_fitted = true;
        }
        // The terminal under the hand is brought into view when the hand
        // moves to another one — `Ctrl+Tab`, a new terminal — and only then:
        // revealing it on every frame would pin the plane under the drag.
        let focused = self
            .terminals
            .iter()
            .find(|terminal| terminal.view.focus_handle(cx).contains_focused(window, cx))
            .map(|terminal| terminal.view.entity_id());
        if focused.is_some() && focused != self.overview_seen && size.0 > 0. {
            if let Some(rect) = focused.and_then(|id| plan.tile(id.as_u64())) {
                self.overview_view = self.overview_view.reveal(rect, size);
            }
        }
        self.overview_seen = focused;
        // A node just created — a note — is brought into view once the plan
        // has a place for it.
        if let Some(node) = self.overview_reveal.take() {
            match plan.rect(&node) {
                Some(rect) if size.0 > 0. => {
                    self.overview_view = self.overview_view.reveal(rect, size);
                }
                _ => self.overview_reveal = Some(node),
            }
        }
        let view = self.overview_view;
        // Every terminal learns the zoom and its card's grid before it paints.
        for terminal in &self.terminals {
            let (w, h) = terminal.size;
            let canvas = Canvas {
                zoom: view.zoom,
                size: (w, h - overview::HEAD),
            };
            terminal
                .view
                .update(cx, |view, _| view.set_canvas(Some(canvas)));
        }

        let rem = window.rem_size() * view.zoom;
        let measured = self.overview_viewport.clone();
        let measure = canvas(
            move |_, _, _| {},
            move |bounds, _, window, _| {
                let seen = (
                    f32::from(bounds.origin.x),
                    f32::from(bounds.origin.y),
                    f32::from(bounds.size.width),
                    f32::from(bounds.size.height),
                );
                if measured.get() != seen {
                    measured.set(seen);
                    window.refresh();
                }
            },
        )
        .absolute()
        .size_full();

        let links = self.render_links(&plan, view, cx);
        let gits: Vec<AnyElement> = plan
            .gits
            .iter()
            .map(|git| {
                Scaled {
                    rem,
                    child: placed(view.screen(git.rect))
                        .child(self.render_git_node(&git.path, view.zoom, cx))
                        .child(corner(
                            cx.entity().downgrade(),
                            Node::Git(git.path.clone()),
                            cx,
                        ))
                        .into_any_element(),
                }
                .into_any_element()
            })
            .collect();
        let cards: Vec<AnyElement> = plan
            .cards
            .iter()
            .map(|card| {
                Scaled {
                    rem,
                    child: placed(view.screen(card.rect))
                        .child(self.render_worktree_card(&card.path, view.zoom, cx))
                        .child(corner(
                            cx.entity().downgrade(),
                            Node::Worktree(card.path.clone()),
                            cx,
                        ))
                        .into_any_element(),
                }
                .into_any_element()
            })
            .collect();
        let tiles: Vec<AnyElement> = self
            .terminals
            .iter()
            .filter_map(|terminal| {
                let id = terminal.view.entity_id();
                let rect = plan.tile(id.as_u64())?;
                Some(self.render_tile(terminal, view.screen(rect), rem, view.zoom, window, cx))
            })
            .collect();
        let notes: Vec<AnyElement> = plan
            .notes
            .iter()
            .map(|note| {
                Scaled {
                    rem,
                    child: placed(view.screen(note.rect))
                        .child(self.render_home_note(note.id, view.zoom, cx))
                        .child(corner(cx.entity().downgrade(), Node::Note(note.id), cx))
                        .into_any_element(),
                }
                .into_any_element()
            })
            .collect();
        let bars = self.render_scrollbars(&plan, view, size, cx);

        let empty = plan.cards.is_empty();
        div()
            .id("overview")
            .relative()
            .flex_1()
            .min_h_0()
            .min_w_0()
            .overflow_hidden()
            .bg(super::theme::gutter(cx))
            .when(self.overview_drag.is_some(), |el| el.cursor_grabbing())
            // The middle button drags the plane from anywhere, terminals
            // included: taken in the capture phase, before a terminal pastes
            // on it — `Ctrl+Shift+V` still does, on this screen.
            .capture_any_mouse_down(cx.listener(|this, event: &MouseDownEvent, _, cx| {
                if event.button == MouseButton::Middle {
                    this.overview_drag = Some(Drag::Plane(event.position));
                    cx.stop_propagation();
                    cx.notify();
                }
            }))
            // And the left one from wherever the nodes leave bare — they stop
            // the press themselves, a card being clicked and a terminal
            // selected in.
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, event: &MouseDownEvent, _, cx| {
                    this.overview_drag = Some(Drag::Plane(event.position));
                    cx.notify();
                }),
            )
            .on_mouse_move(cx.listener(|this, event: &MouseMoveEvent, _, cx| {
                this.overview_dragged(event, cx);
            }))
            .capture_any_mouse_up(cx.listener(|this, _, _, cx| {
                this.end_overview_drag(cx);
            }))
            // The wheel pans, and zooms with the platform key — around the
            // pointer, so that what one zooms on stays under it. What reaches
            // this is what a terminal let through: a sideways notch, and
            // every notch with the platform key held.
            .on_scroll_wheel(cx.listener(|this, event: &ScrollWheelEvent, window, cx| {
                let delta = event.delta.pixel_delta(window.line_height());
                cx.stop_propagation();
                if event.modifiers.secondary() {
                    let (x, y, _, _) = this.overview_viewport.get();
                    let anchor = (
                        f32::from(event.position.x) - x,
                        f32::from(event.position.y) - y,
                    );
                    this.overview_view = this
                        .overview_view
                        .zoom_at(overview::wheel_factor(f32::from(delta.y)), anchor);
                } else {
                    this.overview_view.pan.0 += f32::from(delta.x);
                    this.overview_view.pan.1 += f32::from(delta.y);
                }
                cx.notify();
            }))
            .child(measure)
            .child(links)
            .children(gits)
            .children(cards)
            .children(notes)
            .children(tiles)
            .when(empty, |el| {
                el.child(
                    v_flex()
                        .absolute()
                        .inset_0()
                        .items_center()
                        .justify_center()
                        .text_sm()
                        .text_color(cx.theme().muted_foreground)
                        .child(tr!("overview-empty")),
                )
            })
            .children(bars)
            .child(self.render_project_picker(cx))
            .child(self.render_zoom_bar(view, cx))
    }

    /// A step of whatever drag is under way.
    fn overview_dragged(&mut self, event: &MouseMoveEvent, cx: &mut Context<Self>) {
        let Some(drag) = self.overview_drag.clone() else {
            return;
        };
        // The button came up somewhere this canvas did not see.
        if !matches!(
            event.pressed_button,
            Some(MouseButton::Left | MouseButton::Middle)
        ) {
            self.end_overview_drag(cx);
            return;
        }
        let at = event.position;
        let moved = |last: gpui_kit::Point<Pixels>| {
            let delta = at - last;
            (f32::from(delta.x), f32::from(delta.y))
        };
        self.overview_drag = Some(match drag {
            Drag::Plane(last) => {
                let (dx, dy) = moved(last);
                self.overview_view.pan.0 += dx;
                self.overview_view.pan.1 += dy;
                Drag::Plane(at)
            }
            Drag::Node(node, last) => {
                let (dx, dy) = moved(last);
                let zoom = self.overview_view.zoom;
                let offset = self.overview_moved.entry(node.clone()).or_default();
                offset.0 += dx / zoom;
                offset.1 += dy / zoom;
                Drag::Node(node, at)
            }
            Drag::Resize(node, last) => {
                let (dx, dy) = moved(last);
                let zoom = self.overview_view.zoom;
                let delta = (dx / zoom, dy / zoom);
                match &node {
                    Node::Terminal(id) => {
                        if let Some(terminal) = self
                            .terminals
                            .iter_mut()
                            .find(|t| t.view.entity_id().as_u64() == *id)
                        {
                            terminal.size =
                                overview::resized(terminal.size, delta, overview::MIN_TILE);
                        }
                    }
                    Node::Git(_) | Node::Worktree(_) | Node::Note(_) => {
                        let (default, min) = match node {
                            Node::Git(_) => (overview::GIT, overview::MIN_GIT),
                            Node::Note(_) => (overview::NOTE, overview::MIN_NOTE),
                            _ => (overview::CARD, overview::MIN_CARD),
                        };
                        let size = self.overview_sizes.entry(node.clone()).or_insert(default);
                        *size = overview::resized(*size, delta, min);
                    }
                }
                Drag::Resize(node, at)
            }
            Drag::Thumb(axis, last) => {
                let (dx, dy) = moved(last);
                let plan = self.overview_plan(cx);
                let bounds = self.overview_view.screen(plan.bounds);
                let (w, h) = self.overview_size();
                match axis {
                    Axis::Horizontal => {
                        let ratio = overview::thumb_ratio((bounds.x, bounds.right()), w);
                        self.overview_view.pan.0 -= dx * ratio;
                    }
                    Axis::Vertical => {
                        let ratio = overview::thumb_ratio((bounds.y, bounds.bottom()), h);
                        self.overview_view.pan.1 -= dy * ratio;
                    }
                }
                Drag::Thumb(axis, at)
            }
        });
        cx.notify();
    }

    /// The end of a drag: a git node or a worktree moved by hand is
    /// remembered — a terminal lives no longer than the process, so its place
    /// does not outlive it either.
    fn end_overview_drag(&mut self, cx: &mut Context<Self>) {
        let Some(drag) = self.overview_drag.take() else {
            return;
        };
        cx.notify();
        let (node, offset, size) = match drag {
            Drag::Node(node, _) => {
                let offset = self.overview_moved.get(&node).copied();
                (node, offset, None)
            }
            Drag::Resize(node, _) => {
                let size = self.overview_sizes.get(&node).copied();
                (node, None, size)
            }
            Drag::Plane(_) | Drag::Thumb(..) => return,
        };
        if offset.is_none() && size.is_none() {
            return;
        }
        super::store::Store::update_global(cx, |store| {
            let (home_offset, home_size) = match &node {
                Node::Git(main) => {
                    let repo = store.repos.entry(main.clone()).or_default();
                    (&mut repo.home_offset, &mut repo.home_size)
                }
                Node::Worktree(path) => {
                    let worktree = store.worktrees.entry(path.clone()).or_default();
                    (&mut worktree.home_offset, &mut worktree.home_size)
                }
                Node::Note(id) => {
                    let Some(note) = store.home_notes.iter_mut().find(|n| n.id == *id) else {
                        return;
                    };
                    (&mut note.offset, &mut note.size)
                }
                Node::Terminal(_) => return,
            };
            if offset.is_some() {
                *home_offset = offset;
            }
            if size.is_some() {
                *home_size = size;
            }
        });
    }

    /// The places remembered from an earlier session, read once.
    fn load_overview_places(&mut self, cx: &App) {
        if self.overview_loaded {
            return;
        }
        self.overview_loaded = true;
        let store = super::store::Store::global(cx);
        let remembered = store
            .repos
            .iter()
            .map(|(main, repo)| (Node::Git(main.clone()), repo.home_offset, repo.home_size))
            .chain(store.worktrees.iter().map(|(path, worktree)| {
                (
                    Node::Worktree(path.clone()),
                    worktree.home_offset,
                    worktree.home_size,
                )
            }))
            .chain(
                store
                    .home_notes
                    .iter()
                    .map(|note| (Node::Note(note.id), note.offset, note.size)),
            )
            .collect::<Vec<_>>();
        for (node, offset, size) in remembered {
            if let Some(offset) = offset {
                self.overview_moved.insert(node.clone(), offset);
            }
            if let Some(size) = size {
                self.overview_sizes.insert(node, size);
            }
        }
    }

    /// Puts every node of the projects on show back where the tree wants it,
    /// at the size it starts at, and fits the whole in view.
    ///
    /// The projects on show and not every one: resetting what one is looking
    /// at is the gesture, and another project's arrangement is not in sight
    /// to be missed.
    fn reset_overview(&mut self, cx: &mut Context<Self>) {
        let plan = self.overview_plan(cx);
        let nodes: Vec<Node> = plan
            .gits
            .iter()
            .map(|git| Node::Git(git.path.clone()))
            .chain(
                plan.cards
                    .iter()
                    .map(|card| Node::Worktree(card.path.clone())),
            )
            .chain(plan.tiles.iter().map(|tile| Node::Terminal(tile.id)))
            .chain(plan.notes.iter().map(|note| Node::Note(note.id)))
            .collect();
        for node in &nodes {
            self.overview_moved.remove(node);
            self.overview_sizes.remove(node);
            if let Node::Terminal(id) = node {
                if let Some(terminal) = self
                    .terminals
                    .iter_mut()
                    .find(|t| t.view.entity_id().as_u64() == *id)
                {
                    terminal.size = overview::Tile::default().size();
                }
            }
        }
        super::store::Store::update_global(cx, |store| {
            for node in &nodes {
                match node {
                    Node::Git(main) => {
                        if let Some(repo) = store.repos.get_mut(main) {
                            repo.home_offset = None;
                            repo.home_size = None;
                        }
                    }
                    Node::Worktree(path) => {
                        if let Some(worktree) = store.worktrees.get_mut(path) {
                            worktree.home_offset = None;
                            worktree.home_size = None;
                        }
                    }
                    Node::Note(id) => {
                        if let Some(note) = store.home_notes.iter_mut().find(|n| n.id == *id) {
                            note.offset = None;
                            note.size = None;
                        }
                    }
                    Node::Terminal(_) => {}
                }
            }
        });
        self.overview_fitted = false;
        cx.notify();
    }

    /// The scrollbars, along the right and bottom edges: where the view is
    /// in what there is, and a thumb one drags.
    fn render_scrollbars(
        &self,
        plan: &Plan,
        view: View,
        (w, h): (f32, f32),
        cx: &mut Context<Self>,
    ) -> Vec<AnyElement> {
        let bounds = view.screen(plan.bounds);
        let thumb_color = cx.theme().muted_foreground.opacity(0.35);
        let mut bars = Vec::new();
        if let Some((start, len)) = overview::thumb((bounds.x, bounds.right()), w) {
            bars.push(
                div()
                    .id("overview-scroll-x")
                    .absolute()
                    .bottom(px(2.))
                    .left(px(start))
                    .w(px(len.max(24.)))
                    .h(px(8.))
                    .rounded_full()
                    .bg(thumb_color)
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(|this, event: &MouseDownEvent, _, cx| {
                            this.overview_drag =
                                Some(Drag::Thumb(Axis::Horizontal, event.position));
                            cx.stop_propagation();
                        }),
                    )
                    .into_any_element(),
            );
        }
        if let Some((start, len)) = overview::thumb((bounds.y, bounds.bottom()), h) {
            bars.push(
                div()
                    .id("overview-scroll-y")
                    .absolute()
                    .right(px(2.))
                    .top(px(start))
                    .h(px(len.max(24.)))
                    .w(px(8.))
                    .rounded_full()
                    .bg(thumb_color)
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(|this, event: &MouseDownEvent, _, cx| {
                            this.overview_drag = Some(Drag::Thumb(Axis::Vertical, event.position));
                            cx.stop_propagation();
                        }),
                    )
                    .into_any_element(),
            );
        }
        bars
    }

    /// Which project the plane shows, top right: one at a time by default,
    /// so that five worktrees of one code are not read among a dozen of
    /// another's.
    fn render_project_picker(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let shown = self.overview_repos();
        let label: SharedString = if self.overview_all {
            tr!("overview-all-projects")
        } else {
            shown
                .first()
                .map(|repo| SharedString::from(repo.name.clone()))
                .unwrap_or_default()
        };
        let current = (!self.overview_all)
            .then(|| shown.first().map(|repo| repo.main.clone()))
            .flatten();
        let choices: Vec<(PathBuf, SharedString)> = self
            .repos
            .iter()
            .map(|repo| (repo.main.clone(), SharedString::from(repo.name.clone())))
            .collect();
        let all = self.overview_all;
        let app = cx.entity().downgrade();
        h_flex()
            .absolute()
            .top_3()
            .right_3()
            .gap_1()
            .p_1()
            .rounded(cx.theme().radius_lg)
            .border_1()
            .border_color(cx.theme().border)
            .bg(cx.theme().background)
            .shadow_md()
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .child(
                Button::new("overview-project")
                    .ghost()
                    .small()
                    .icon(icon("folder"))
                    .label(label)
                    .dropdown_menu(move |menu, _, _| {
                        let everything = app.clone();
                        let menu = menu.item(
                            PopupMenuItem::new(tr!("overview-all-projects"))
                                .checked(all)
                                .on_click(move |_, _, cx| {
                                    if let Some(app) = everything.upgrade() {
                                        app.update(cx, |this, cx| this.show_project(None, cx));
                                    }
                                }),
                        );
                        choices.iter().cloned().fold(menu, |menu, (main, name)| {
                            let app = app.clone();
                            let checked = current.as_ref() == Some(&main);
                            menu.item(PopupMenuItem::new(name).checked(checked).on_click(
                                move |_, _, cx| {
                                    if let Some(app) = app.upgrade() {
                                        let main = main.clone();
                                        app.update(cx, |this, cx| {
                                            this.show_project(Some(main), cx)
                                        });
                                    }
                                },
                            ))
                        })
                    }),
            )
            .child(
                Button::new("overview-reset")
                    .ghost()
                    .small()
                    .icon(icon("layout-dashboard"))
                    .label(tr!("overview-reset"))
                    .tooltip(tr!("overview-reset-hint"))
                    .on_click(cx.listener(|this, _, _, cx| this.reset_overview(cx))),
            )
    }

    /// Shows one project — or, with `None`, all of them — and fits it.
    fn show_project(&mut self, main: Option<PathBuf>, cx: &mut Context<Self>) {
        self.overview_all = main.is_none();
        if main.is_some() {
            self.overview_repo = main;
        }
        self.overview_fitted = false;
        cx.notify();
    }

    /// The lines between the nodes, under them.
    fn render_links(&self, plan: &Plan, view: View, cx: &App) -> impl IntoElement {
        let active = self.active.clone();
        let links: Vec<Segment> = plan
            .links
            .iter()
            .map(|link| {
                let on = active.as_deref() == Some(link.worktree.as_path());
                let mut link = link.clone();
                link.from = view.point(link.from);
                link.to = view.point(link.to);
                (link, on)
            })
            .collect();
        let quiet = cx.theme().border;
        let lit = cx.theme().ring;
        let width = px((1.5 * view.zoom).clamp(1., 2.5));
        canvas(
            move |_, _, _| {},
            move |bounds, _, window, _| {
                let at =
                    |(x, y): (f32, f32)| point(bounds.origin.x + px(x), bounds.origin.y + px(y));
                for (link, on) in links {
                    // What hangs from a card — a terminal, a note — is drawn
                    // finer than the tree itself: the branches are the shape,
                    // the rest is what the shape carries.
                    let width = match link.kind {
                        LinkKind::Root | LinkKind::Branch => width,
                        LinkKind::Terminal | LinkKind::Note => width * 0.6,
                    };
                    // Out of the parent through the side facing the child,
                    // and into the child through the side facing back: each
                    // end leaves square to its side, half the way across.
                    let (from, to) = (link.from, link.to);
                    let reach = ((to.0 - from.0).abs().max((to.1 - from.1).abs()) / 2.).max(12.);
                    let out = link.from_side.outward();
                    let back = link.to_side.outward();
                    let mut path = PathBuilder::stroke(width);
                    path.move_to(at(from));
                    path.cubic_bezier_to(
                        at(to),
                        at((from.0 + out.0 * reach, from.1 + out.1 * reach)),
                        at((to.0 + back.0 * reach, to.1 + back.1 * reach)),
                    );
                    if let Ok(path) = path.build() {
                        window.paint_path(path, if on { lit } else { quiet });
                    }
                }
            },
        )
        .absolute()
        .size_full()
    }

    /// The zoom's own controls, in a corner and at the window's size: the one
    /// part of the screen that does not move with the plane.
    fn render_zoom_bar(&self, view: View, cx: &mut Context<Self>) -> impl IntoElement {
        let zoom_by = |factor: f32| {
            move |this: &mut Self,
                  _: &gpui_kit::ClickEvent,
                  _: &mut Window,
                  cx: &mut Context<Self>| {
                let (w, h) = this.overview_size();
                this.overview_view = this.overview_view.zoom_at(factor, (w / 2., h / 2.));
                cx.notify();
            }
        };
        h_flex()
            .absolute()
            .bottom_3()
            .right_3()
            .gap_1()
            .p_1()
            .items_center()
            .rounded(cx.theme().radius_lg)
            .border_1()
            .border_color(cx.theme().border)
            .bg(cx.theme().background)
            .shadow_md()
            // The press is the bar's, not the plane's to start a drag with.
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .child(
                Button::new("overview-zoom-out")
                    .ghost()
                    .small()
                    .icon(icon("minus"))
                    .tooltip(tr!("overview-zoom-out"))
                    .on_click(cx.listener(zoom_by(1. / overview::ZOOM_STEP))),
            )
            .child(
                Button::new("overview-zoom-reset")
                    .ghost()
                    .small()
                    .label(SharedString::from(format!("{:.0} %", view.zoom * 100.)))
                    .tooltip(tr!("overview-zoom-reset"))
                    .on_click(cx.listener(|this, _, _, cx| {
                        let (w, h) = this.overview_size();
                        let factor = 1. / this.overview_view.zoom;
                        this.overview_view = this.overview_view.zoom_at(factor, (w / 2., h / 2.));
                        cx.notify();
                    })),
            )
            .child(
                Button::new("overview-zoom-in")
                    .ghost()
                    .small()
                    .icon(icon("plus"))
                    .tooltip(tr!("overview-zoom-in"))
                    .on_click(cx.listener(zoom_by(overview::ZOOM_STEP))),
            )
            .child(
                Button::new("overview-fit")
                    .ghost()
                    .small()
                    .icon(icon("maximize"))
                    .tooltip(tr!("overview-fit"))
                    .on_click(cx.listener(|this, _, _, cx| {
                        let plan = this.overview_plan(cx);
                        this.overview_view = View::fit(plan.bounds, this.overview_size());
                        cx.notify();
                    })),
            )
    }

    /// A repository's git node: its branches, and a fetch.
    fn render_git_node(&self, main: &Path, zoom: f32, cx: &mut Context<Self>) -> AnyElement {
        let Some(repo) = self.repos.iter().find(|repo| repo.main == main) else {
            return div().into_any_element();
        };
        let detail = zoom >= DETAIL;
        let theme = cx.theme().clone();
        let muted = theme.muted_foreground;
        let local = repo
            .branches
            .iter()
            .filter(|b| b.kind == crate::git::BranchKind::Local)
            .count();
        let remote = repo.branches.len() - local;
        // The branches no worktree holds: what one could open next. The list
        // is newest first, as `branch::list` reads it.
        let idle: Vec<&crate::git::Branch> = repo
            .branches
            .iter()
            .filter(|b| b.kind == crate::git::BranchKind::Local && b.checked_out_at.is_none())
            .take(3)
            .collect();
        let fetch = main.to_path_buf();
        let open = main.to_path_buf();
        let head = h_flex()
            .flex_none()
            .h(px(overview::HEAD * zoom))
            .px_2()
            .gap_1p5()
            .items_center()
            .bg(theme.secondary)
            .cursor_grab()
            .on_mouse_down(
                MouseButton::Left,
                grab(cx.entity().downgrade(), Node::Git(main.to_path_buf())),
            )
            .child(icon("git-merge").text_color(muted))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .font_weight(gpui_kit::FontWeight::SEMIBOLD)
                    .child(SharedString::from(repo.name.clone())),
            )
            .when(detail, |el| {
                el.child(note_button(
                    cx.entity().downgrade(),
                    super::store::HomeAnchor::Repo(main.to_path_buf()),
                ))
                .child(
                    Button::new(SharedString::from(format!(
                        "overview-fetch-{}",
                        main.display()
                    )))
                    .ghost()
                    .xsmall()
                    .icon(icon("refresh-cw"))
                    .tooltip(tr!("overview-fetch"))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.git.send(crate::runtime::Cmd::Fetch {
                            worktree: fetch.clone(),
                        });
                        cx.notify();
                    })),
                )
            });
        v_flex()
            .id(SharedString::from(format!(
                "overview-git-{}",
                main.display()
            )))
            .size_full()
            .overflow_hidden()
            .rounded(theme.radius_lg)
            .border_1()
            .border_color(theme.border)
            .bg(theme.background)
            .text_sm()
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
            // Twice goes to work in the main checkout, as a card's does.
            .on_click(
                cx.listener(move |this, event: &gpui_kit::ClickEvent, window, cx| {
                    if event.click_count() >= 2 {
                        this.work_in_worktree(&open, window, cx);
                    }
                }),
            )
            .child(head)
            .child(
                v_flex()
                    .flex_1()
                    .min_h_0()
                    .p_2()
                    .gap_1()
                    .child(
                        div()
                            .text_xs()
                            .text_color(muted)
                            .truncate()
                            .child(SharedString::from(main.display().to_string())),
                    )
                    .child(
                        h_flex()
                            .gap_1()
                            .items_center()
                            .text_xs()
                            .child(icon("git-branch").text_color(muted))
                            .child(tr!("overview-branches", { local: local, remote: remote })),
                    )
                    .when(detail && !idle.is_empty(), |el| {
                        el.child(
                            div()
                                .text_xs()
                                .text_color(muted)
                                .child(tr!("overview-idle-branches")),
                        )
                        .children(idle.into_iter().map(|branch| {
                            h_flex()
                                .gap_1p5()
                                .text_xs()
                                .child(
                                    div()
                                        .flex_1()
                                        .min_w_0()
                                        .truncate()
                                        .child(SharedString::from(branch.name.clone())),
                                )
                                .child(
                                    div()
                                        .flex_none()
                                        .text_color(muted)
                                        .child(SharedString::from(branch.date.clone())),
                                )
                        }))
                    }),
            )
            .into_any_element()
    }

    /// A worktree's card: who works in it, what it changed, what it added.
    fn render_worktree_card(&self, path: &Path, zoom: f32, cx: &mut Context<Self>) -> AnyElement {
        let Some(worktree) = self.repos.worktree(path) else {
            return div().into_any_element();
        };
        let detail = zoom >= DETAIL;
        let theme = cx.theme().clone();
        let muted = theme.muted_foreground;
        let (repo, label) = self.project_label(path);
        let is_active = self.active.as_deref() == Some(path);
        let agent = self.agents.get(path).cloned();
        let summary = self.summaries.get(path).copied();
        let outline = self.outlines.get(path);
        let go = path.to_path_buf();

        let head = h_flex()
            .flex_none()
            .h(px(overview::HEAD * zoom))
            .px_2()
            .gap_1p5()
            .items_center()
            .bg(theme.secondary)
            .cursor_grab()
            .on_mouse_down(
                MouseButton::Left,
                grab(cx.entity().downgrade(), Node::Worktree(path.to_path_buf())),
            )
            .children(
                agent
                    .as_ref()
                    .map(|agent| super::topbar::agent_dot(agent, cx)),
            )
            .children(repo.map(|repo| div().flex_none().text_xs().text_color(muted).child(repo)))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .font_weight(gpui_kit::FontWeight::SEMIBOLD)
                    .child(label),
            )
            .when(worktree.is_main, |el| {
                el.child(
                    div()
                        .flex_none()
                        .text_xs()
                        .text_color(muted)
                        .child(tr!("overview-main")),
                )
            })
            .when(detail, |el| {
                el.child(note_button(
                    cx.entity().downgrade(),
                    super::store::HomeAnchor::Worktree(path.to_path_buf()),
                ))
            })
            .when(detail && summary.is_some_and(|s| !s.is_empty()), |el| {
                let commit = path.to_path_buf();
                el.child(
                    Button::new(SharedString::from(format!(
                        "overview-commit-{}",
                        path.display()
                    )))
                    .ghost()
                    .xsmall()
                    .icon(icon("git-commit-horizontal"))
                    .tooltip(tr!("overview-commit"))
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.open_commit_sheet(&commit, window, cx);
                    })),
                )
            })
            .when(detail, |el| {
                // The menu's trigger has no click of its own to stop, and the
                // card's would take the press for "select this one".
                el.child(
                    div()
                        .id(SharedString::from(format!(
                            "overview-new-{}",
                            path.display()
                        )))
                        .on_click(|_, _, cx| cx.stop_propagation())
                        .child(self.new_terminal_button(path, cx)),
                )
                .child(
                    Button::new(SharedString::from(format!(
                        "overview-go-{}",
                        path.display()
                    )))
                    .ghost()
                    .xsmall()
                    .icon(icon("external-link"))
                    .tooltip(tr!("overview-open-worktree"))
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.work_in_worktree(&go, window, cx);
                    })),
                )
            });

        let branch = worktree
            .branch
            .clone()
            .map(SharedString::from)
            .unwrap_or_else(|| tr!("overview-detached"));
        let upstream = outline
            .and_then(|o| o.upstream)
            .and_then(|(ahead, behind)| {
                let mut parts = Vec::new();
                if ahead > 0 {
                    parts.push(format!("↑{ahead}"));
                }
                if behind > 0 {
                    parts.push(format!("↓{behind}"));
                }
                (!parts.is_empty()).then(|| SharedString::from(parts.join(" ")))
            });
        let branch_row = h_flex()
            .gap_1()
            .items_center()
            .text_xs()
            .child(icon("git-branch").text_color(muted))
            .child(div().min_w_0().truncate().child(branch))
            .children(upstream.map(|text| div().flex_none().text_color(muted).child(text)));
        let changes = match summary.filter(|summary| !summary.is_empty()) {
            Some(summary) => h_flex()
                .gap_1()
                .items_center()
                .text_xs()
                .child(
                    div()
                        .text_color(muted)
                        .child(tr!("home-files", { count: summary.files })),
                )
                .child(super::topbar::volume_on(summary, None, cx))
                .into_any_element(),
            None => div()
                .text_xs()
                .text_color(muted)
                .child(tr!("home-clean"))
                .into_any_element(),
        };
        // The agent's word, when it said one: finished, or what it waits on.
        let word = agent
            .as_ref()
            .filter(|agent| super::topbar::activity_word(agent).is_some())
            .map(|agent| super::topbar::activity_badge(agent, px(10_000.), cx));

        let commits = outline.filter(|_| detail).map(|outline| {
            let title = match (&outline.base, outline.ahead_of_base) {
                (Some(base), ahead) if ahead > 0 => {
                    tr!("overview-ahead", { count: ahead, base: base })
                }
                _ => tr!("overview-recent"),
            };
            let now = chrono::Utc::now().timestamp();
            v_flex()
                .gap_0p5()
                .min_h_0()
                .overflow_hidden()
                .child(div().text_xs().text_color(muted).child(title))
                .children(outline.commits.iter().take(COMMITS).map(|commit| {
                    h_flex()
                        .gap_1p5()
                        .text_xs()
                        .child(
                            div()
                                .flex_none()
                                .font_family(theme.mono_font_family.clone())
                                .text_color(muted)
                                .child(SharedString::from(commit.short.clone())),
                        )
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .truncate()
                                .child(SharedString::from(commit.subject.clone())),
                        )
                        .child(
                            div()
                                .flex_none()
                                .text_color(muted)
                                .child(ago(now, commit.at)),
                        )
                }))
        });

        let open = path.to_path_buf();
        v_flex()
            .id(SharedString::from(format!(
                "overview-card-{}",
                path.display()
            )))
            .size_full()
            .overflow_hidden()
            .rounded(theme.radius_lg)
            .border_1()
            .border_color(if is_active { theme.ring } else { theme.border })
            .bg(theme.background)
            .text_sm()
            .cursor_pointer()
            // A card is clicked, not dragged: the press stays here.
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
            // A click makes it the worktree on show — its card and its links
            // light up — and a second goes to work in it. Leaving the screen
            // is the one gesture here that loses the view, and a click that
            // missed the bare plane it meant to drag is not one to pay for it.
            .on_click(
                cx.listener(move |this, event: &gpui_kit::ClickEvent, window, cx| {
                    if event.click_count() >= 2 {
                        this.work_in_worktree(&open, window, cx);
                    } else if this.active.as_deref() != Some(open.as_path()) {
                        this.select_worktree(open.clone(), window, cx);
                    }
                }),
            )
            .child(head)
            .child(
                v_flex()
                    .flex_1()
                    .min_h_0()
                    .p_2()
                    .gap_1p5()
                    .child(branch_row)
                    .child(changes)
                    .children(word)
                    .children(commits),
            )
            .into_any_element()
    }

    /// A terminal's card: its head, and the terminal itself.
    fn render_tile(
        &self,
        terminal: &super::terminal_view::OpenTerminal,
        rect: Rect,
        rem: Pixels,
        zoom: f32,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let theme = cx.theme().clone();
        let view = terminal.view.clone();
        let id = view.entity_id();
        let focused = view.focus_handle(cx).contains_focused(window, cx);
        let label = terminal
            .name
            .clone()
            .unwrap_or_else(|| view.read(cx).label());
        let detail = zoom >= DETAIL;
        let worktree = terminal.worktree.clone();
        let go = terminal.worktree.clone();
        let agent = self.agents.get(&terminal.worktree).cloned();
        let head = h_flex()
            .id(("overview-tile-head", id))
            .flex_none()
            .h(px(overview::HEAD * zoom))
            .w_full()
            .px_2()
            .gap_1()
            .items_center()
            .bg(theme.secondary)
            .text_xs()
            .cursor_grab()
            .on_mouse_down(
                MouseButton::Left,
                grab(cx.entity().downgrade(), Node::Terminal(id.as_u64())),
            )
            // The head selects: clicking the terminal's own surface is a click
            // the program receives.
            .on_click(cx.listener(move |this, _, window, cx| {
                this.focus_tile(id, &worktree, window, cx);
            }))
            .child(icon("square-terminal").text_color(theme.muted_foreground))
            .child(div().flex_1().min_w_0().truncate().child(label))
            .children(
                agent
                    .as_ref()
                    .map(|agent| super::topbar::activity_badge(agent, px(10_000.), cx)),
            )
            .when(detail, |el| {
                el.child(
                    Button::new(("overview-tile-size", id))
                        .ghost()
                        .xsmall()
                        .label(overview::Tile::nearest(terminal.size).label())
                        .tooltip(tr!("overview-tile-size"))
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.cycle_tile(id, cx);
                        })),
                )
                .child(
                    Button::new(("overview-tile-go", id))
                        .ghost()
                        .xsmall()
                        .icon(icon("external-link"))
                        .tooltip(tr!("overview-open-worktree"))
                        .on_click(cx.listener(move |this, _, window, cx| {
                            this.work_in_terminal(id, &go, window, cx);
                        })),
                )
            });
        Scaled {
            rem,
            child: placed(rect)
                // Selecting text in a terminal is a drag the plane must not
                // take for its own.
                .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                .child(
                    v_flex()
                        .size_full()
                        .rounded(theme.radius_lg)
                        .overflow_hidden()
                        .bg(theme.background)
                        .child(head)
                        // `v_flex` and not `div`: a `div` is a block, and the
                        // terminal's `size_full` of an undefined height is zero.
                        .child(v_flex().flex_1().min_h_0().child(view)),
                )
                // The outline on top rather than as the box's border: a
                // border takes its pixel from the terminal's room, and the
                // grid is worked out from the card's size.
                .child(
                    div()
                        .absolute()
                        .inset_0()
                        .rounded(theme.radius_lg)
                        .border_1()
                        .border_color(if focused { theme.ring } else { theme.border }),
                )
                .child(corner(
                    cx.entity().downgrade(),
                    Node::Terminal(id.as_u64()),
                    cx,
                ))
                .into_any_element(),
        }
        .into_any_element()
    }

    /// A `+` on a worktree's card: a shell, or one of the agent profiles.
    fn new_terminal_button(&self, path: &Path, cx: &mut Context<Self>) -> AnyElement {
        let app = cx.entity().downgrade();
        let path = path.to_path_buf();
        let id = SharedString::from(format!("overview-new-{}", path.display()));
        if super::settings::Settings::global(cx)
            .terminal
            .agents
            .is_empty()
        {
            return Button::new(id)
                .ghost()
                .xsmall()
                .icon(icon("plus"))
                .tooltip(tr!("terminal-new"))
                .on_click(move |_, window, cx| {
                    open_on(&app, &path, Launch::shell(), window, cx);
                })
                .into_any_element();
        }
        Button::new(id)
            .ghost()
            .xsmall()
            .icon(icon("plus"))
            .tooltip(tr!("terminal-new"))
            .dropdown_menu(move |menu, _, cx| {
                let profiles = super::settings::Settings::global(cx)
                    .terminal
                    .agents
                    .clone();
                let shell = (app.clone(), path.clone());
                let menu = menu.item(
                    PopupMenuItem::new(tr!("terminal-new"))
                        .icon(icon("plus"))
                        .on_click(move |_, window, cx| {
                            open_on(&shell.0, &shell.1, Launch::shell(), window, cx);
                        }),
                );
                profiles.into_iter().fold(menu, |menu, profile| {
                    let (app, path) = (app.clone(), path.clone());
                    let label = SharedString::from(profile.label().to_string());
                    menu.item(PopupMenuItem::new(label).icon(icon("bot")).on_click(
                        move |_, window, cx| {
                            open_on(&app, &path, Launch::agent(&profile), window, cx);
                        },
                    ))
                })
            })
            .into_any_element()
    }

    /// Turns the home screen on and off.
    ///
    /// The keyboard is handed over both ways: what had the focus is no longer
    /// painted after either switch, and a focus on something the window does
    /// not show is a window where no key does anything. Coming in, the
    /// terminal one was reading keeps it if it is there; failing that the last
    /// one of the checkout on show.
    pub(super) fn toggle_overview(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.overview {
            self.leave_overview(cx);
            self.focus_handle(cx).focus(window, cx);
            return;
        }
        self.overview = true;
        self.overview_seen = None;
        self.load_overview_places(cx);
        let worktrees = self.repos.worktrees_in_order();
        if !worktrees.is_empty() {
            self.git
                .send(crate::runtime::Cmd::LoadOutlines { worktrees });
        }
        let focused = self
            .terminals
            .iter()
            .any(|terminal| terminal.view.focus_handle(cx).contains_focused(window, cx));
        if !focused {
            let landing = self
                .active
                .clone()
                .and_then(|worktree| self.terminals_of(&worktree).last().map(|t| t.view.clone()));
            if let Some(view) = landing {
                window.focus(&view.focus_handle(cx), cx);
            }
        }
        cx.notify();
    }

    /// Leaves the home screen, and gives the terminals back their own size.
    pub(super) fn leave_overview(&mut self, cx: &mut Context<Self>) {
        if !self.overview {
            return;
        }
        self.overview = false;
        self.overview_drag = None;
        for terminal in &self.terminals {
            terminal.view.update(cx, |view, _| view.set_canvas(None));
        }
        cx.notify();
    }

    /// Gives a terminal's card its next preset size.
    fn cycle_tile(&mut self, view: gpui_kit::EntityId, cx: &mut Context<Self>) {
        if let Some(terminal) = self
            .terminals
            .iter_mut()
            .find(|terminal| terminal.view.entity_id() == view)
        {
            terminal.size = overview::Tile::nearest(terminal.size).next().size();
            cx.notify();
        }
    }

    /// Hands the keyboard to one terminal, and files where it came from.
    ///
    /// The panel is activated too, though nothing shows it while the screen
    /// is up: leaving then lands on the terminal one was last reading. And the
    /// worktree follows for the same reason — the window comes back looking at
    /// the project one was watching.
    fn focus_tile(
        &mut self,
        view: gpui_kit::EntityId,
        worktree: &Path,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(terminal) = self
            .terminals
            .iter()
            .find(|terminal| terminal.view.entity_id() == view)
        else {
            return;
        };
        let (view, panel) = (terminal.view.clone(), terminal.panel.clone());
        super::panels::TerminalPanel::activate(&panel, window, cx);
        window.focus(&view.focus_handle(cx), cx);
        if self.active.as_deref() != Some(worktree) {
            self.select_worktree(worktree.to_path_buf(), window, cx);
        }
        cx.notify();
    }

    /// Goes to work in a terminal's worktree, with that terminal in front.
    fn work_in_terminal(
        &mut self,
        view: gpui_kit::EntityId,
        worktree: &Path,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.work_in_worktree(worktree, window, cx);
        self.focus_tile(view, worktree, window, cx);
    }
}

/// The grip in a node's bottom-right corner: dragged, it resizes the node.
///
/// In pixels and not in `rem`, alone of what a node carries: a grip that
/// shrank with the plane would be a target nobody can hit at a distance, and
/// resizing from afar is when one wants it.
fn corner(app: WeakEntity<ClaudhubApp>, node: Node, cx: &App) -> gpui_kit::Stateful<gpui_kit::Div> {
    let color = cx.theme().muted_foreground.opacity(0.6);
    let id = SharedString::from(format!("overview-corner-{node:?}"));
    div()
        .id(id)
        .absolute()
        .right_0()
        .bottom_0()
        .size(px(14.))
        .cursor(gpui_kit::CursorStyle::ResizeUpLeftDownRight)
        .child(
            div()
                .absolute()
                .right(px(3.))
                .bottom(px(3.))
                .size(px(7.))
                .border_r_2()
                .border_b_2()
                .border_color(color),
        )
        .on_mouse_down(MouseButton::Left, move |event, _, cx| {
            cx.stop_propagation();
            if let Some(app) = app.upgrade() {
                let node = node.clone();
                app.update(cx, |this, _| {
                    this.overview_drag = Some(Drag::Resize(node, event.position));
                });
            }
        })
}

/// A node's head takes the node along: the press starts the drag, and stops
/// there, the plane's own drag being what a bare press means.
fn grab(
    app: WeakEntity<ClaudhubApp>,
    node: Node,
) -> impl Fn(&MouseDownEvent, &mut Window, &mut App) + 'static {
    move |event, _, cx| {
        cx.stop_propagation();
        if let Some(app) = app.upgrade() {
            app.update(cx, |this, _| {
                this.overview_drag = Some(Drag::Node(node.clone(), event.position));
            });
        }
    }
}

/// The `+ note` of a git node's or a card's head.
fn note_button(app: WeakEntity<ClaudhubApp>, anchor: super::store::HomeAnchor) -> Button {
    let id = SharedString::from(format!("overview-add-note-{anchor:?}"));
    Button::new(id)
        .ghost()
        .xsmall()
        .icon(icon("sticky-note"))
        .tooltip(tr!("overview-note-add"))
        .on_click(move |_, window, cx| {
            if let Some(app) = app.upgrade() {
                let anchor = anchor.clone();
                app.update(cx, |this, cx| this.add_home_note(anchor, window, cx));
            }
        })
}

impl ClaudhubApp {
    /// Writes a new note, hung from a git node or a card, and opens it for
    /// writing — an empty note shown as rendered Markdown is a blank card
    /// that says nothing of what to do with it.
    fn add_home_note(
        &mut self,
        anchor: super::store::HomeAnchor,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let id = super::store::Store::global(cx)
            .home_notes
            .iter()
            .map(|note| note.id)
            .max()
            .map_or(1, |id| id + 1);
        super::store::Store::update_global(cx, |store| {
            store.home_notes.push(super::store::HomeNote {
                id,
                anchor,
                text: String::new(),
                offset: None,
                size: None,
            });
        });
        self.overview_reveal = Some(Node::Note(id));
        self.edit_home_note(id, window, cx);
    }

    /// Opens a note for writing: a field of the window's editor, whose every
    /// change goes back to the store — there is no "save" to forget.
    fn edit_home_note(&mut self, id: u64, window: &mut Window, cx: &mut Context<Self>) {
        if let Some((editor, _)) = self.note_editors.get(&id) {
            super::dialogs::focus_field(editor, window, cx);
            return;
        }
        let text = super::store::Store::global(cx)
            .home_notes
            .iter()
            .find(|note| note.id == id)
            .map(|note| note.text.clone())
            .unwrap_or_default();
        let editor = cx.new(|cx| super::surface::plain_editor(window, cx));
        editor.update(cx, |editor, cx| editor.set_value(text, window, cx));
        let subscription = cx.subscribe(
            &editor,
            move |_, editor, event: &gpui_kit::component::input::InputEvent, cx| {
                if !matches!(event, gpui_kit::component::input::InputEvent::Change) {
                    return;
                }
                let text = editor.read(cx).value().to_string();
                super::store::Store::update_global_if(cx, |store| {
                    match store.home_notes.iter_mut().find(|note| note.id == id) {
                        Some(note) if note.text != text => {
                            note.text = text;
                            true
                        }
                        _ => false,
                    }
                });
            },
        );
        super::dialogs::focus_field(&editor, window, cx);
        self.note_editors.insert(id, (editor, subscription));
        cx.notify();
    }

    /// Back to the rendered note. What was typed is already in the store.
    fn done_home_note(&mut self, id: u64, window: &mut Window, cx: &mut Context<Self>) {
        self.note_editors.remove(&id);
        self.focus_handle(cx).focus(window, cx);
        cx.notify();
    }

    /// Deletes a note, once asked: it is text someone wrote, and nothing
    /// else keeps a copy of it.
    fn delete_home_note(&mut self, id: u64, window: &mut Window, cx: &mut Context<Self>) {
        let entity = cx.entity();
        window.open_dialog(cx, move |dialog, _, _| {
            let entity = entity.clone();
            dialog
                .title(tr!("overview-note-delete-title"))
                .child(div().text_sm().child(tr!("overview-note-delete-body")))
                .overlay_closable(false)
                .close_button(false)
                .footer(super::dialogs::confirm())
                .on_ok(move |_, _, cx| {
                    entity.update(cx, |this, cx| {
                        this.note_editors.remove(&id);
                        this.overview_moved.remove(&Node::Note(id));
                        this.overview_sizes.remove(&Node::Note(id));
                        super::store::Store::update_global(cx, |store| {
                            store.home_notes.retain(|note| note.id != id);
                        });
                        cx.notify();
                    });
                    true
                })
        });
    }

    /// A note: rendered Markdown, or the field it is written in.
    fn render_home_note(&self, id: u64, zoom: f32, cx: &mut Context<Self>) -> AnyElement {
        let Some(note) = super::store::Store::global(cx)
            .home_notes
            .iter()
            .find(|note| note.id == id)
            .cloned()
        else {
            return div().into_any_element();
        };
        let theme = cx.theme().clone();
        let muted = theme.muted_foreground;
        let detail = zoom >= DETAIL;
        let editor = self.note_editors.get(&id).map(|(editor, _)| editor.clone());
        let editing = editor.is_some();
        // Its first line, bare of Markdown's marks: what the note is about.
        let title = note
            .text
            .lines()
            .map(|line| line.trim_start_matches(['#', '>', '-', '*', ' ']).trim())
            .find(|line| !line.is_empty())
            .map(|line| SharedString::from(line.to_string()))
            .unwrap_or_else(|| tr!("overview-note"));
        let app = cx.entity().downgrade();
        let head = h_flex()
            .flex_none()
            .h(px(overview::HEAD * zoom))
            .px_2()
            .gap_1p5()
            .items_center()
            .bg(theme.warning.opacity(0.18))
            .cursor_grab()
            .on_mouse_down(MouseButton::Left, grab(app.clone(), Node::Note(id)))
            .child(icon("sticky-note").text_color(muted))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .font_weight(gpui_kit::FontWeight::SEMIBOLD)
                    .child(title),
            )
            .when(detail, |el| {
                el.child(
                    Button::new(("overview-note-edit", id))
                        .ghost()
                        .xsmall()
                        .icon(icon(if editing { "check" } else { "pencil" }))
                        .tooltip(if editing {
                            tr!("overview-note-done")
                        } else {
                            tr!("overview-note-edit")
                        })
                        .on_click(cx.listener(move |this, _, window, cx| {
                            if editing {
                                this.done_home_note(id, window, cx);
                            } else {
                                this.edit_home_note(id, window, cx);
                            }
                        })),
                )
                .child(
                    Button::new(("overview-note-delete", id))
                        .ghost()
                        .xsmall()
                        .icon(icon("trash-2"))
                        .tooltip(tr!("overview-note-delete"))
                        .on_click(cx.listener(move |this, _, window, cx| {
                            this.delete_home_note(id, window, cx);
                        })),
                )
            });
        let body = match editor {
            Some(editor) => v_flex()
                .flex_1()
                .min_h_0()
                .p_1()
                .child(
                    gpui_kit::component::input::Editor::new(&editor)
                        .text_sm()
                        .h_full(),
                )
                .into_any_element(),
            None => div()
                .id(("overview-note-body", id))
                .flex_1()
                .min_h_0()
                .p_2()
                .overflow_y_scroll()
                .text_sm()
                .cursor_text()
                // Twice to write in it, the gesture of every sticky note.
                .on_click(
                    cx.listener(move |this, event: &gpui_kit::ClickEvent, window, cx| {
                        if event.click_count() >= 2 {
                            this.edit_home_note(id, window, cx);
                        }
                    }),
                )
                .map(|el| {
                    if note.text.trim().is_empty() {
                        el.text_color(muted).child(tr!("overview-note-empty"))
                    } else {
                        el.child(gpui_kit::component::text::TextView::markdown(
                            ("overview-note-text", id),
                            note.text.clone(),
                        ))
                    }
                })
                .into_any_element(),
        };
        v_flex()
            .size_full()
            .overflow_hidden()
            .rounded(theme.radius_lg)
            .border_1()
            .border_color(theme.warning.opacity(0.45))
            .bg(theme.background)
            // A note is written in and read, not dragged by its body.
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .child(head)
            .child(body)
            .into_any_element()
    }
}

/// The commit sheet: a worktree's changes to tick, and the commit box of the
/// Changes panel under them — the same field, the same buttons, the same
/// draft, so that what is started here is finished there and back.
///
/// **An entity and not a closure**, `SettingsForm`'s reason: `open_dialog`
/// keeps a `Fn` called back from the root's render, where reading the
/// application is a panic, and a child's render comes after.
pub(super) struct CommitSheet {
    app: WeakEntity<ClaudhubApp>,
}

impl Render for CommitSheet {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let Some(app) = self.app.upgrade() else {
            return div().into_any_element();
        };
        app.update(cx, |app, cx| {
            app.render_commit_sheet(window, cx).into_any_element()
        })
    }
}

impl ClaudhubApp {
    /// Opens the commit sheet on a worktree, which becomes the one on show:
    /// the commit box speaks of the active worktree, and the card one pressed
    /// is the one the sheet is about.
    fn open_commit_sheet(&mut self, worktree: &Path, window: &mut Window, cx: &mut Context<Self>) {
        if self.active.as_deref() != Some(worktree) {
            self.select_worktree(worktree.to_path_buf(), window, cx);
        }
        let (repo, label) = self.project_label(worktree);
        let name = match repo {
            Some(repo) => format!("{repo} · {label}"),
            None => label.to_string(),
        };
        let title = tr!("overview-commit-title", { name: name });
        let app = cx.entity().downgrade();
        let sheet = cx.new(|_| CommitSheet { app: app.clone() });
        self.commit_sheet = true;
        window.open_dialog(cx, move |dialog, _, _| {
            let app = app.clone();
            dialog
                .title(title.clone())
                .w(px(640.))
                .child(sheet.clone())
                .on_close(move |_, _, cx| {
                    if let Some(app) = app.upgrade() {
                        app.update(cx, |this, _| this.commit_sheet = false);
                    }
                })
        });
        super::dialogs::focus_field(&self.commit_input, window, cx);
        cx.notify();
    }

    fn render_commit_sheet(
        &mut self,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let Some(worktree) = self.active.clone() else {
            return div().into_any_element();
        };
        let files: Vec<crate::git::FileStatus> = self
            .review
            .get(&worktree)
            .map(|review| review.status.files.clone())
            .unwrap_or_default();
        let staged = files.iter().filter(|file| file.is_staged()).count();
        let muted = cx.theme().muted_foreground;
        let entity = cx.entity();
        let list = v_flex()
            .id("commit-sheet-files")
            .max_h(px(300.))
            .overflow_y_scroll()
            .gap_0p5()
            .when(files.is_empty(), |el| {
                el.child(div().text_sm().text_color(muted).child(tr!("home-clean")))
            })
            .children(files.into_iter().enumerate().map(|(index, file)| {
                // Ticked when all of it is in the index; a press puts the rest
                // in, or takes all of it out — the Changes panel's box.
                let ticked = file.is_staged() && !file.is_unstaged();
                let conflicted = file.is_conflicted();
                let (worktree, path, entity) =
                    (worktree.clone(), file.path.clone(), entity.clone());
                h_flex()
                    .gap_2()
                    .items_center()
                    .text_sm()
                    .child(
                        gpui_kit::component::checkbox::Checkbox::new(("commit-sheet-stage", index))
                            .checked(ticked)
                            .disabled(conflicted)
                            .on_click(move |_, _, cx| {
                                let (worktree, path) = (worktree.clone(), path.clone());
                                entity.update(cx, |this, cx| {
                                    this.set_staged(worktree, vec![path], !ticked, cx)
                                });
                            }),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .when(conflicted, |el| el.text_color(cx.theme().danger))
                            .child(SharedString::from(file.path.display().to_string())),
                    )
            }));
        v_flex()
            .gap_2()
            .child(list)
            .child(self.render_commit_box(staged > 0, staged, cx))
            .into_any_element()
    }
}

/// "3 h ago", in the window's words.
fn ago(now: i64, at: i64) -> SharedString {
    use super::base_select::Ago;
    match super::base_select::ago(now, at) {
        Ago::JustNow => tr!("when-just-now"),
        Ago::Minutes(n) => tr!("when-minutes", { n: n }),
        Ago::Hours(n) => tr!("when-hours", { n: n }),
        Ago::Earlier => chrono::DateTime::from_timestamp(at, 0)
            .map(|at| {
                SharedString::from(at.with_timezone(&chrono::Local).format("%d/%m").to_string())
            })
            .unwrap_or_default(),
    }
}

/// Opens a terminal on a worktree, from a card's `+`.
///
/// **The focus is given a second time, deferred**: a menu that closes hands
/// the focus back to what had it before it opened, after this ran — the
/// multiplexer's race, and `dialogs::focus_field`'s answer. The plane then
/// sees the focus move, and brings the new terminal into view.
fn open_on(
    app: &WeakEntity<ClaudhubApp>,
    worktree: &Path,
    launch: Launch,
    window: &mut Window,
    cx: &mut App,
) {
    let Some(app) = app.upgrade() else {
        return;
    };
    let opened = app.update(cx, |app, cx| {
        app.open_terminal(worktree, launch, window, cx);
        app.terminals.last().map(|terminal| terminal.view.clone())
    });
    if let Some(view) = opened {
        super::dialogs::focus_field(&view, window, cx);
    }
}
