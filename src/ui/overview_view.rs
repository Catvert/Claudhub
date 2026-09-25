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
use super::canvas_view::Hang;
use super::icons::icon;
use super::overview::{self, Checkout, Group, LinkKind, Node, Plan, Rect, View};
use super::terminal_view::{Canvas, Launch};
use crate::tr;

/// Below this zoom, cards are read and not handled.
pub(super) const DETAIL: f32 = 0.55;
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
    pub(super) fn overview_repos(&self) -> Vec<&crate::ui::repos::RepoState> {
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

    /// The worktree picked beside the project, while it is still one of the
    /// project's: gone from it, or with every project on show, the plane
    /// holds them all — a filter nobody can see in the corner is a plane
    /// that is missing something without saying so.
    pub(super) fn overview_picked_worktree(&self) -> Option<&Path> {
        if self.overview_all {
            return None;
        }
        let picked = self.overview_worktree.as_deref()?;
        self.overview_repos()
            .iter()
            .any(|repo| {
                repo.worktrees
                    .iter()
                    .any(|worktree| worktree.path == picked && !worktree.prunable)
            })
            .then_some(picked)
    }

    /// What the plane holds, laid out.
    fn overview_plan(&self) -> Plan {
        overview::plan(&self.overview_groups(), &self.overview_hand)
    }

    /// What the plane holds, before it is laid out.
    fn overview_groups(&self) -> Vec<Group<'_>> {
        // A worktree's nodes hang from its card, unless they say they are
        // the repository's; a repository's note versioned on several branches
        // is one note, shown once — the main checkout's copy first.
        let entries = |worktree: &Path| {
            self.canvas
                .get(worktree)
                .map(|entries| entries.as_slice())
                .unwrap_or(&[])
        };
        let repo_notes = |repo: &crate::ui::repos::RepoState| -> Vec<PathBuf> {
            let mut seen: Vec<(bool, std::ffi::OsString)> = Vec::new();
            let mut notes = Vec::new();
            let mut checkouts: Vec<_> = repo.worktrees.iter().collect();
            checkouts.sort_by_key(|worktree| !worktree.is_main);
            for worktree in checkouts {
                for entry in entries(&worktree.path) {
                    if entry.node.anchor != crate::canvas::Anchor::Repo {
                        continue;
                    }
                    let key = (
                        entry.private,
                        entry.path.file_name().unwrap_or_default().to_os_string(),
                    );
                    if !seen.contains(&key) {
                        seen.push(key);
                        notes.push(entry.path.clone());
                    }
                }
            }
            notes
        };
        let worktree_notes = |worktree: &Path| -> Vec<PathBuf> {
            entries(worktree)
                .iter()
                .filter(|entry| entry.node.anchor == crate::canvas::Anchor::Worktree)
                .map(|entry| entry.path.clone())
                .collect()
        };
        let picked = self.overview_picked_worktree();
        let groups: Vec<Group> = self
            .overview_repos()
            .into_iter()
            .map(|repo| Group {
                main: &repo.main,
                notes: repo_notes(repo),
                checkouts: repo
                    .worktrees
                    .iter()
                    // A folder gone from the disk has nothing to show but its
                    // absence, which the picker already says.
                    .filter(|worktree| !worktree.prunable)
                    // One worktree picked, the others are not on this plane;
                    // the git node and the repository's notes stay, being
                    // every worktree's.
                    .filter(|worktree| picked.is_none_or(|picked| picked == worktree.path))
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
                        notes: worktree_notes(&worktree.path),
                    })
                    .collect(),
            })
            .collect();
        groups
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
        // The places kept from the last session, once — the screen may come
        // up with the window, before any toggle has read them.
        self.load_overview_places(cx);
        let mut plan = self.overview_plan();
        // A node came or went since the last frame: what was on screen stays
        // where it stood — see `overview::hold`.
        if let Some(before) = self.overview_previous.take() {
            if self.overview_fitted && overview::nodes_changed(&before, &plan) {
                let mut hand = self.overview_hand.clone();
                let held = overview::hold(&self.overview_groups(), &mut hand, &before);
                self.overview_hand = hand;
                if !held.is_empty() {
                    self.remember_offsets(&held, cx);
                    plan = self.overview_plan();
                }
            }
        }
        self.overview_previous = Some(plan.clone());
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
        let gits: Vec<(Node, AnyElement)> = plan
            .gits
            .iter()
            .map(|git| {
                let element = Scaled {
                    rem,
                    child: placed(view.screen(git.rect))
                        .child(self.render_git_node(&git.path, view.zoom, cx))
                        .children(self.corner_unless_folded(Node::Git(git.path.clone()), cx))
                        .into_any_element(),
                }
                .into_any_element();
                (Node::Git(git.path.clone()), element)
            })
            .collect();
        let cards: Vec<(Node, AnyElement)> = plan
            .cards
            .iter()
            .map(|card| {
                let element = Scaled {
                    rem,
                    child: placed(view.screen(card.rect))
                        .child(self.render_worktree_card(&card.path, view.zoom, cx))
                        .children(self.corner_unless_folded(Node::Worktree(card.path.clone()), cx))
                        .into_any_element(),
                }
                .into_any_element();
                (Node::Worktree(card.path.clone()), element)
            })
            .collect();
        let tiles: Vec<(Node, AnyElement)> = self
            .terminals
            .iter()
            .filter_map(|terminal| {
                let id = terminal.view.entity_id();
                let rect = plan.tile(id.as_u64())?;
                let element =
                    self.render_tile(terminal, view.screen(rect), rem, view.zoom, window, cx);
                Some((Node::Terminal(id.as_u64()), element))
            })
            .collect();
        let notes: Vec<(Node, AnyElement)> = plan
            .notes
            .iter()
            .map(|note| {
                let element = Scaled {
                    rem,
                    child: placed(view.screen(note.rect))
                        .child(self.render_home_note(&note.path, view.zoom, cx))
                        .children(self.corner_unless_folded(Node::Note(note.path.clone()), cx))
                        .into_any_element(),
                }
                .into_any_element();
                (Node::Note(note.path.clone()), element)
            })
            .collect();
        // The maximised node is painted last, over a veil across the rest of
        // the plane: a node grown to the screen is a window in front, and the
        // order of kinds — terminals after notes — put a terminal over it.
        let maximized = self.overview_maximized.as_ref().map(|m| m.node.clone());
        let mut on_top = None;
        let mut nodes: Vec<AnyElement> = Vec::new();
        for (node, element) in gits.into_iter().chain(cards).chain(notes).chain(tiles) {
            if maximized.as_ref() == Some(&node) {
                on_top = Some((node, element));
            } else {
                nodes.push(element);
            }
        }
        let veil = on_top.as_ref().map(|(node, _)| {
            let node = node.clone();
            div()
                .id("overview-veil")
                .absolute()
                .inset_0()
                .bg(super::theme::gutter(cx).opacity(0.82))
                // A press beside the window gives the view back, as beside a
                // modal; and nothing under the veil takes it meanwhile.
                .occlude()
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(move |this, _, window, cx| {
                        cx.stop_propagation();
                        this.toggle_maximize(&node, window, cx);
                    }),
                )
        });
        let bars = match self.overview_maximized {
            Some(_) => Vec::new(),
            None => self.render_scrollbars(&plan, view, size, cx),
        };
        let grid = render_grid(view, cx);

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
                if this.overview_maximized.is_some() {
                    return;
                }
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
            .child(grid)
            .child(links)
            .children(nodes)
            .children(veil)
            .children(on_top.map(|(_, element)| element))
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
        // A maximised node is a window filling the screen: nothing slides
        // under it and nothing is dragged — a move would lose the very view
        // the second press gives back.
        if self.overview_maximized.is_some() {
            self.overview_drag = None;
            return;
        }
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
                let offset = self.overview_hand.moved.entry(node.clone()).or_default();
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
                        let size = self
                            .overview_hand
                            .sizes
                            .entry(node.clone())
                            .or_insert(default);
                        *size = overview::resized(*size, delta, min);
                    }
                }
                Drag::Resize(node, at)
            }
            Drag::Thumb(axis, last) => {
                let (dx, dy) = moved(last);
                let plan = self.overview_plan();
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
                let offset = self.overview_hand.moved.get(&node).copied();
                (node, offset, None)
            }
            Drag::Resize(node, _) => {
                let size = self.overview_hand.sizes.get(&node).copied();
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
                Node::Note(path) => {
                    let place = store.home_places.entry(path.clone()).or_default();
                    (&mut place.offset, &mut place.size)
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

    /// Writes the offsets of nodes moved without a hand — held in place —
    /// as a drag's are: a terminal's go with the rest of it.
    fn remember_offsets(&self, nodes: &[Node], cx: &mut Context<Self>) {
        let moved = &self.overview_hand.moved;
        super::store::Store::update_global(cx, |store| {
            for node in nodes {
                let offset = moved.get(node).copied();
                match node {
                    Node::Git(main) => {
                        store.repos.entry(main.clone()).or_default().home_offset = offset
                    }
                    Node::Worktree(path) => {
                        store.worktrees.entry(path.clone()).or_default().home_offset = offset
                    }
                    Node::Note(path) => {
                        store.home_places.entry(path.clone()).or_default().offset = offset
                    }
                    Node::Terminal(_) => {}
                }
            }
        });
    }

    /// The places remembered from an earlier session, read once.
    fn load_overview_places(&mut self, cx: &mut Context<Self>) {
        if self.overview_loaded {
            return;
        }
        self.overview_loaded = true;
        let store = super::store::Store::global(cx);
        let remembered = store
            .repos
            .iter()
            .map(|(main, repo)| {
                (
                    Node::Git(main.clone()),
                    repo.home_offset,
                    repo.home_size,
                    repo.home_collapsed,
                    false,
                )
            })
            .chain(store.worktrees.iter().map(|(path, worktree)| {
                (
                    Node::Worktree(path.clone()),
                    worktree.home_offset,
                    worktree.home_size,
                    worktree.home_collapsed,
                    worktree.home_hidden,
                )
            }))
            .chain(store.home_places.iter().map(|(path, place)| {
                (
                    Node::Note(path.clone()),
                    place.offset,
                    place.size,
                    place.collapsed,
                    place.hidden,
                )
            }))
            .collect::<Vec<_>>();
        let hand = &mut self.overview_hand;
        for (node, offset, size, collapsed, hidden) in remembered {
            if let Some(offset) = offset {
                hand.moved.insert(node.clone(), offset);
            }
            if let Some(size) = size {
                hand.sizes.insert(node.clone(), size);
            }
            if collapsed {
                hand.collapsed.insert(node.clone());
            }
            if hidden {
                hand.hidden.insert(node);
            }
        }
        self.migrate_home_notes(cx);
    }

    /// The notes of before, kept in the store, become private note files in
    /// their worktree's vault — once, and only where a vault is set: without
    /// one they stay in the store rather than be lost.
    fn migrate_home_notes(&mut self, cx: &mut Context<Self>) {
        let notes = super::store::Store::global(cx).home_notes.clone();
        if notes.is_empty() {
            return;
        }
        let stamp = chrono::Local::now().format("%Y-%m-%d-%H%M").to_string();
        let mut kept = Vec::new();
        for note in notes {
            let (worktree, anchor) = match &note.anchor {
                super::store::HomeAnchor::Repo(main) => (main.clone(), crate::canvas::Anchor::Repo),
                super::store::HomeAnchor::Worktree(path) => {
                    (path.clone(), crate::canvas::Anchor::Worktree)
                }
            };
            let Some(vault) = self.notes_dir(&worktree, cx) else {
                kept.push(note);
                continue;
            };
            let mut node =
                crate::canvas::Node::note(anchor, None, chrono::Local::now().to_rfc3339());
            node.body = note.text.clone();
            let name = format!("{stamp}-note-{}.md", note.id);
            let path = crate::wslpath::join(&crate::canvas::private_dir(&vault), &name);
            if note.offset.is_some() || note.size.is_some() || note.collapsed {
                let place = super::store::NodePlace {
                    offset: note.offset,
                    size: note.size,
                    collapsed: note.collapsed,
                    hidden: false,
                };
                super::store::Store::update_global(cx, |store| {
                    store.home_places.insert(path.clone(), place);
                });
            }
            self.git.send(crate::runtime::Cmd::WriteCanvasFile {
                worktree,
                path,
                text: crate::canvas::render(&node),
                expect: Some(crate::files::ABSENT),
            });
        }
        super::store::Store::update_global(cx, |store| store.home_notes = kept);
    }

    /// Puts every node of the projects on show back where the tree wants it,
    /// at the size it starts at, and fits the whole in view.
    ///
    /// The projects on show and not every one: resetting what one is looking
    /// at is the gesture, and another project's arrangement is not in sight
    /// to be missed.
    fn reset_overview(&mut self, cx: &mut Context<Self>) {
        let plan = self.overview_plan();
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
            .chain(plan.notes.iter().map(|note| Node::Note(note.path.clone())))
            .collect();
        // And what was taken off the plane comes back: a hidden worktree is
        // in no plan to be read from, so it is found among the repositories.
        let hidden: Vec<Node> = self
            .hidden_nodes(cx)
            .into_iter()
            .map(|(node, _)| node)
            .collect();
        let nodes: Vec<Node> = nodes.into_iter().chain(hidden).collect();
        self.overview_maximized = None;
        for node in &nodes {
            self.overview_hand.moved.remove(node);
            self.overview_hand.sizes.remove(node);
            self.overview_hand.collapsed.remove(node);
            self.overview_hand.hidden.remove(node);
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
                            repo.home_collapsed = false;
                        }
                    }
                    Node::Worktree(path) => {
                        if let Some(worktree) = store.worktrees.get_mut(path) {
                            worktree.home_offset = None;
                            worktree.home_size = None;
                            worktree.home_collapsed = false;
                            worktree.home_hidden = false;
                        }
                    }
                    Node::Note(path) => {
                        store.home_places.remove(path);
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
            .children(self.render_overview_worktree_picker(cx))
            .children(self.render_hidden_menu(cx))
            .child(self.render_skill_button(cx))
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

    /// Which of the project's worktrees the plane shows, beside the project
    /// picker: all of them by default, or one to read on its own. Only with
    /// one project on show — a worktree of which, among several — and only
    /// when there is a choice to make.
    fn render_overview_worktree_picker(&self, cx: &mut Context<Self>) -> Option<impl IntoElement> {
        if self.overview_all {
            return None;
        }
        let repo = self.overview_repos().into_iter().next()?;
        let choices: Vec<(PathBuf, SharedString)> = repo
            .worktrees
            .iter()
            .filter(|worktree| !worktree.prunable)
            .map(|worktree| {
                let label = worktree.label();
                let name = match worktree.branch.as_deref() {
                    Some(branch) if branch != label => format!("{label} · {branch}"),
                    _ => label,
                };
                (worktree.path.clone(), SharedString::from(name))
            })
            .collect();
        if choices.len() < 2 {
            return None;
        }
        let current = self.overview_picked_worktree().map(Path::to_path_buf);
        let label = current
            .as_ref()
            .and_then(|picked| choices.iter().find(|(path, _)| path == picked))
            .map(|(_, name)| name.clone())
            .unwrap_or_else(|| tr!("overview-all-worktrees"));
        let app = cx.entity().downgrade();
        Some(
            Button::new("overview-worktree")
                .ghost()
                .small()
                .icon(icon("git-branch"))
                .label(label)
                .tooltip(tr!("overview-worktree-hint"))
                .dropdown_menu(move |menu, _, _| {
                    let everything = app.clone();
                    let menu = menu.item(
                        PopupMenuItem::new(tr!("overview-all-worktrees"))
                            .checked(current.is_none())
                            .on_click(move |_, _, cx| {
                                if let Some(app) = everything.upgrade() {
                                    app.update(cx, |this, cx| this.show_worktree(None, cx));
                                }
                            }),
                    );
                    choices.iter().cloned().fold(menu, |menu, (path, name)| {
                        let app = app.clone();
                        let checked = current.as_ref() == Some(&path);
                        menu.item(PopupMenuItem::new(name).checked(checked).on_click(
                            move |_, _, cx| {
                                if let Some(app) = app.upgrade() {
                                    let path = path.clone();
                                    app.update(cx, |this, cx| this.show_worktree(Some(path), cx));
                                }
                            },
                        ))
                    })
                }),
        )
    }

    /// Shows one worktree of the project — or, with `None`, all of them —
    /// and fits it: another plane to frame, as another project is.
    fn show_worktree(&mut self, worktree: Option<PathBuf>, cx: &mut Context<Self>) {
        self.overview_worktree = worktree;
        self.give_back_maximized();
        self.overview_fitted = false;
        cx.notify();
    }

    /// Gives a maximised node back its size, the view being about to be
    /// framed anew: another plane may not hold the node, and the veil would
    /// then be gone while the screen still refused the wheel and the drags.
    fn give_back_maximized(&mut self) {
        if let Some(maximized) = self.overview_maximized.take() {
            self.set_node_size(&maximized.node, maximized.size);
        }
    }

    /// Shows one project — or, with `None`, all of them — and fits it.
    fn show_project(&mut self, main: Option<PathBuf>, cx: &mut Context<Self>) {
        self.overview_all = main.is_none();
        if main.is_some() {
            self.overview_repo = main;
        }
        // A worktree picked in one project names nothing in another.
        self.overview_worktree = None;
        self.give_back_maximized();
        self.overview_fitted = false;
        self.ask_skill_status();
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
        let zoom = view.zoom;
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
                    // An elbow: out square to the parent's side, across
                    // half way, into the child square to its own — and the
                    // two corners rounded, never wider than half a leg.
                    let points = overview::elbow(link.from, link.from_side, link.to);
                    let radius = (10. * zoom).max(3.);
                    let mut path = PathBuilder::stroke(width);
                    path.move_to(at(points[0]));
                    for corner in 1..=2 {
                        let (before, here, after) =
                            (points[corner - 1], points[corner], points[corner + 1]);
                        let length = |a: (f32, f32), b: (f32, f32)| {
                            ((b.0 - a.0).powi(2) + (b.1 - a.1).powi(2)).sqrt()
                        };
                        let (into, out) = (length(before, here), length(here, after));
                        let r = radius.min(into / 2.).min(out / 2.);
                        if r < 0.5 {
                            path.line_to(at(here));
                            continue;
                        }
                        let toward = |a: (f32, f32), b: (f32, f32), by: f32| {
                            let d = length(a, b);
                            (a.0 + (b.0 - a.0) / d * by, a.1 + (b.1 - a.1) / d * by)
                        };
                        path.line_to(at(toward(here, before, r)));
                        path.curve_to(at(toward(here, after, r)), at(here));
                    }
                    path.line_to(at(points[3]));
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
                        let plan = this.overview_plan();
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
                el.child(self.add_menu(Hang::Repo(main.to_path_buf()), cx))
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
            .child(head.when(detail, |el| {
                el.child(self.window_controls(Node::Git(main.to_path_buf()), cx))
            }))
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
            // A branch with commits its base does not have can be merged from
            // here — the card is where one sees how far ahead it is.
            .when_some(
                outline
                    .filter(|o| detail && !worktree.is_main && o.ahead_of_base > 0)
                    .and_then(|o| o.base.clone()),
                |el, base| {
                    let merge = path.to_path_buf();
                    el.child(
                        Button::new(SharedString::from(format!(
                            "overview-merge-{}",
                            path.display()
                        )))
                        .ghost()
                        .xsmall()
                        .icon(icon("git-merge"))
                        .tooltip(tr!("overview-merge", { base: base }))
                        .on_click(cx.listener(
                            move |this, _, window, cx| {
                                this.confirm_merge(&merge, window, cx);
                            },
                        )),
                    )
                },
            )
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
                        .child(self.add_menu(Hang::Worktree(path.to_path_buf()), cx)),
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
            .child(head.when(detail, |el| {
                el.child(self.window_controls(Node::Worktree(path.to_path_buf()), cx))
            }))
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
        let folded = self
            .overview_hand
            .collapsed
            .contains(&Node::Terminal(id.as_u64()));
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
                        .child(head.when(detail, |el| {
                            el.child(self.window_controls(Node::Terminal(id.as_u64()), cx))
                        }))
                        // `v_flex` and not `div`: a `div` is a block, and the
                        // terminal's `size_full` of an undefined height is zero.
                        .when(!folded, |el| {
                            el.child(v_flex().flex_1().min_h_0().child(view))
                        }),
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
                .children(self.corner_unless_folded(Node::Terminal(id.as_u64()), cx))
                .into_any_element(),
        }
        .into_any_element()
    }

    /// The `+` of a card or of the git node: what can be added under it — a
    /// terminal, an agent, a note written by hand, or a note or a diagram an
    /// agent writes from a request (`canvas_view::generate_node`).
    fn add_menu(&self, hang: Hang, cx: &mut Context<Self>) -> AnyElement {
        let app = cx.entity().downgrade();
        let worktree = match &hang {
            Hang::Repo(main) => main.clone(),
            Hang::Worktree(worktree) => worktree.clone(),
        };
        let id = SharedString::from(format!("overview-add-{hang:?}"));
        Button::new(id)
            .ghost()
            .xsmall()
            .icon(icon("plus"))
            .tooltip(tr!("overview-add"))
            .dropdown_menu(move |menu, _, cx| {
                let profiles = super::settings::Settings::global(cx)
                    .terminal
                    .agents
                    .clone();
                let shell = (app.clone(), worktree.clone());
                let menu = menu.item(
                    PopupMenuItem::new(tr!("terminal-new"))
                        .icon(icon("square-terminal"))
                        .on_click(move |_, window, cx| {
                            open_on(&shell.0, &shell.1, Launch::shell(), window, cx);
                        }),
                );
                let menu = profiles.into_iter().fold(menu, |menu, profile| {
                    let (app, worktree) = (app.clone(), worktree.clone());
                    let label = SharedString::from(profile.label().to_string());
                    menu.item(PopupMenuItem::new(label).icon(icon("bot")).on_click(
                        move |_, window, cx| {
                            open_on(&app, &worktree, Launch::agent(&profile), window, cx);
                        },
                    ))
                });
                let item = |label: SharedString,
                            glyph: &'static str,
                            kind: Option<crate::canvas::Kind>| {
                    let (app, hang) = (app.clone(), hang.clone());
                    PopupMenuItem::new(label)
                        .icon(icon(glyph))
                        .on_click(move |_, window, cx| {
                            let Some(app) = app.upgrade() else {
                                return;
                            };
                            let hang = hang.clone();
                            app.update(cx, |this, cx| match kind {
                                None => this.add_home_note(hang, cx),
                                Some(kind) => this.ask_generation(hang, kind, window, cx),
                            });
                        })
                };
                menu.separator()
                    .item(item(tr!("overview-add-note"), "sticky-note", None))
                    .item(item(
                        tr!("overview-add-note-prompt"),
                        "sparkles",
                        Some(crate::canvas::Kind::Note),
                    ))
                    .item(item(
                        tr!("overview-add-diagram-prompt"),
                        "image",
                        Some(crate::canvas::Kind::Diagram),
                    ))
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
        super::store::Store::update_global(cx, |store| store.session.home = true);
        self.overview_seen = None;
        self.load_overview_places(cx);
        let worktrees = self.repos.worktrees_in_order();
        if !worktrees.is_empty() {
            self.git.send(crate::runtime::Cmd::LoadOutlines {
                worktrees: worktrees.clone(),
            });
            self.read_canvas(worktrees, cx);
        }
        self.ask_skill_status();
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
        super::store::Store::update_global(cx, |store| store.session.home = false);
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

/// A light dotted grid under everything, moving and scaling with the plane: what
/// makes a drag read as the plane moving rather than the cards, and a zoom
/// as distance. Its step doubles when the lines would crowd — see
/// `overview::grid`.
fn render_grid(view: View, cx: &App) -> impl IntoElement {
    // Dots where lines would cross, in the text's colour barely there: lines
    // drew a sheet of graph paper over the plane, and what is wanted is only
    // enough texture for a drag to read as the plane moving.
    let color = cx.theme().foreground.opacity(0.09);
    let dot = px(2.);
    canvas(
        move |_, _, _| {},
        move |bounds, _, window, _| {
            let (w, h) = (f32::from(bounds.size.width), f32::from(bounds.size.height));
            let rows = overview::grid(view.pan.1, view.zoom, h);
            for x in overview::grid(view.pan.0, view.zoom, w) {
                for &y in &rows {
                    window.paint_quad(
                        gpui_kit::fill(
                            gpui_kit::Bounds::new(
                                point(
                                    bounds.origin.x + px(x) - dot / 2.,
                                    bounds.origin.y + px(y) - dot / 2.,
                                ),
                                gpui_kit::size(dot, dot),
                            ),
                            color,
                        )
                        .corner_radii(dot / 2.),
                    );
                }
            }
        },
    )
    .absolute()
    .size_full()
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
pub(super) fn grab(
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

impl ClaudhubApp {
    /// A node's grip, unless the node is folded to its head: a folded node
    /// has no size to drag.
    fn corner_unless_folded(
        &self,
        node: Node,
        cx: &mut Context<Self>,
    ) -> Option<gpui_kit::Stateful<gpui_kit::Div>> {
        (!self.overview_hand.collapsed.contains(&node) && self.overview_maximized.is_none())
            .then(|| corner(cx.entity().downgrade(), node, cx))
    }

    /// A window's three buttons, at the end of a node's head: fold to the
    /// head, maximise, close. The git node has no cross — it is the tree's
    /// root, and a plane without it would be a plane without a project.
    pub(super) fn window_controls(&self, node: Node, cx: &mut Context<Self>) -> impl IntoElement {
        let folded = self.overview_hand.collapsed.contains(&node);
        let maximized = self
            .overview_maximized
            .as_ref()
            .is_some_and(|maximized| maximized.node == node);
        let closable = !matches!(node, Node::Git(_));
        let key = format!("{node:?}");
        let (fold, grow, close) = (node.clone(), node.clone(), node);
        h_flex()
            .flex_none()
            .ml_1()
            .gap_0p5()
            .child(
                Button::new(SharedString::from(format!("overview-fold-{key}")))
                    .ghost()
                    .xsmall()
                    .icon(icon(if folded { "chevron-down" } else { "minus" }))
                    .tooltip(if folded {
                        tr!("overview-unfold")
                    } else {
                        tr!("overview-fold")
                    })
                    .on_click(cx.listener(move |this, _, _, cx| this.toggle_fold(&fold, cx))),
            )
            .child(
                Button::new(SharedString::from(format!("overview-grow-{key}")))
                    .ghost()
                    .xsmall()
                    .icon(icon(if maximized { "minimize" } else { "maximize" }))
                    .tooltip(if maximized {
                        tr!("overview-unmaximize")
                    } else {
                        tr!("overview-maximize")
                    })
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.toggle_maximize(&grow, window, cx)
                    })),
            )
            .when(closable, |el| {
                el.child(
                    Button::new(SharedString::from(format!("overview-close-{key}")))
                        .ghost()
                        .xsmall()
                        .icon(icon("x"))
                        .tooltip(match close {
                            Node::Worktree(_) => tr!("overview-hide"),
                            Node::Note(_) => tr!("overview-note-delete"),
                            _ => tr!("overview-close"),
                        })
                        .on_click(cx.listener(move |this, _, window, cx| {
                            this.close_node(&close, window, cx)
                        })),
                )
            })
    }

    /// Folds a node to its head, or unfolds it.
    fn toggle_fold(&mut self, node: &Node, cx: &mut Context<Self>) {
        if !self.overview_hand.collapsed.remove(node) {
            self.overview_hand.collapsed.insert(node.clone());
        }
        self.remember_folds(node, cx);
        cx.notify();
    }

    /// Makes a node take nine tenths of the screen, or gives back what it had.
    ///
    /// **It grows the node, it does not only look closer**: the node is given
    /// the room at a zoom of one (`overview::maximized_size`), so a terminal
    /// gets the lines and columns — a zoom alone would show the same eighty
    /// columns bigger. The tree makes way around it, and the view centres on
    /// it. The second press gives back the size and the view from before.
    fn toggle_maximize(&mut self, node: &Node, window: &mut Window, cx: &mut Context<Self>) {
        let view_before = match self.overview_maximized.take() {
            Some(maximized) => {
                self.set_node_size(&maximized.node, maximized.size);
                if maximized.node == *node {
                    self.overview_view = maximized.view;
                    cx.notify();
                    return;
                }
                // Another node was maximised: the view to go back to is still
                // the one before the first.
                maximized.view
            }
            None => self.overview_view,
        };
        if self.overview_hand.collapsed.remove(node) {
            self.remember_folds(node, cx);
        }
        let Some(rect) = self.overview_plan().rect(node) else {
            return;
        };
        let viewport = self.overview_size();
        let size_before = self.node_size(node);
        self.set_node_size(
            node,
            Some(overview::maximized_size(viewport, 0.9, (rect.w, rect.h))),
        );
        self.overview_maximized = Some(overview::Maximized {
            node: node.clone(),
            view: view_before,
            size: size_before,
        });
        if let Some(rect) = self.overview_plan().rect(node) {
            self.overview_view = View::focus(rect, viewport, 0.9);
        }
        // A terminal maximised is one to type in.
        if let Node::Terminal(id) = node {
            if let Some(terminal) = self
                .terminals
                .iter()
                .find(|t| t.view.entity_id().as_u64() == *id)
            {
                window.focus(&terminal.view.focus_handle(cx), cx);
            }
        }
        cx.notify();
    }

    /// The size the hand gave a node, if it gave one; a terminal always has
    /// one, being the only kind that carries its own.
    fn node_size(&self, node: &Node) -> Option<(f32, f32)> {
        match node {
            Node::Terminal(id) => self
                .terminals
                .iter()
                .find(|t| t.view.entity_id().as_u64() == *id)
                .map(|t| t.size),
            _ => self.overview_hand.sizes.get(node).copied(),
        }
    }

    /// Gives a node a size, or back the one its kind starts at.
    fn set_node_size(&mut self, node: &Node, size: Option<(f32, f32)>) {
        match node {
            Node::Terminal(id) => {
                if let Some(terminal) = self
                    .terminals
                    .iter_mut()
                    .find(|t| t.view.entity_id().as_u64() == *id)
                {
                    terminal.size = size.unwrap_or_else(|| overview::Tile::default().size());
                }
            }
            _ => match size {
                Some(size) => {
                    self.overview_hand.sizes.insert(node.clone(), size);
                }
                None => {
                    self.overview_hand.sizes.remove(node);
                }
            },
        }
    }

    /// The cross: a terminal closed, a note deleted, a worktree taken off
    /// the plane — each asked first.
    fn close_node(&mut self, node: &Node, window: &mut Window, cx: &mut Context<Self>) {
        match node {
            Node::Git(_) => {}
            Node::Note(path) => self.close_home_note(path, window, cx),
            Node::Terminal(id) => {
                let Some(terminal) = self
                    .terminals
                    .iter()
                    .find(|t| t.view.entity_id().as_u64() == *id)
                else {
                    return;
                };
                let view = terminal.view.entity_id();
                let label = terminal
                    .name
                    .clone()
                    .unwrap_or_else(|| terminal.view.read(cx).label());
                let busy = terminal.view.read(cx).busy();
                let entity = cx.entity();
                window.open_dialog(cx, move |dialog, _, _| {
                    let entity = entity.clone();
                    dialog
                        .title(tr!("overview-close-title"))
                        .child(
                            v_flex()
                                .gap_1()
                                .child(div().text_sm().child(label.clone()))
                                .when(busy, |el| {
                                    el.child(div().text_xs().child(tr!("overview-close-busy")))
                                }),
                        )
                        .overlay_closable(false)
                        .close_button(false)
                        .footer(super::dialogs::confirm())
                        .on_ok(move |_, window, cx| {
                            entity.update(cx, |this, cx| this.close_terminal(view, window, cx));
                            true
                        })
                });
                // The buttons dispatch `Confirm` and `Cancel` from the focus,
                // and the focus is still where the cross was pressed — a
                // terminal, which answers neither: OK did nothing. Deferred,
                // the dialog being painted on the next frame.
                window.defer(cx, |window, cx| window.focus_dialog(cx));
            }
            Node::Worktree(path) => {
                let node = node.clone();
                let (repo, label) = self.project_label(path);
                let name = match repo {
                    Some(repo) => format!("{repo} · {label}"),
                    None => label.to_string(),
                };
                // The main checkout is the repository itself: hidden, never
                // removed from here.
                let removable = self
                    .repos
                    .worktree(path)
                    .is_some_and(|w| !w.is_main)
                    .then(|| self.main_of(path))
                    .flatten();
                let entity = cx.entity();
                let target = path.clone();
                window.open_dialog(cx, move |dialog, _, _| {
                    let (entity, node) = (entity.clone(), node.clone());
                    let footer = match removable.clone() {
                        Some(main) => {
                            let (entity, target) = (entity.clone(), target.clone());
                            super::dialogs::choose(
                                tr!("overview-hide-button"),
                                tr!("overview-remove-worktree"),
                                move |window, cx| {
                                    // Opened once this dialog has gone: the
                                    // button dismisses the dialog on top when
                                    // it is done, which would be this one.
                                    let (entity, main, target) =
                                        (entity.clone(), main.clone(), target.clone());
                                    window.defer(cx, move |window, cx| {
                                        entity.update(cx, |this, cx| {
                                            this.confirm_remove_worktree(
                                                main.clone(),
                                                target.clone(),
                                                window,
                                                cx,
                                            );
                                        });
                                        window.defer(cx, |window, cx| window.focus_dialog(cx));
                                    });
                                },
                            )
                        }
                        None => super::dialogs::submit(tr!("overview-hide-button")),
                    };
                    dialog
                        .title(tr!("overview-close-worktree-title"))
                        .child(
                            v_flex()
                                .gap_1()
                                .child(div().text_sm().child(SharedString::from(name.clone())))
                                .child(div().text_xs().child(tr!("overview-hide-body"))),
                        )
                        .overlay_closable(false)
                        .close_button(false)
                        .footer(footer)
                        .on_ok(move |_, _, cx| {
                            entity.update(cx, |this, cx| {
                                this.overview_hand.hidden.insert(node.clone());
                                this.remember_folds(&node, cx);
                                cx.notify();
                            });
                            true
                        })
                });
                // See the terminal's: the buttons dispatch from the focus.
                window.defer(cx, |window, cx| window.focus_dialog(cx));
            }
        }
    }

    /// The nodes taken off the plane in the projects on show, with the name
    /// the « Hidden » menu gives them.
    pub(super) fn hidden_nodes(&self, cx: &App) -> Vec<(Node, SharedString)> {
        let _ = cx;
        let mut hidden = Vec::new();
        let picked = self.overview_picked_worktree();
        for repo in self.overview_repos() {
            for worktree in &repo.worktrees {
                if picked.is_some_and(|picked| picked != worktree.path) {
                    continue;
                }
                let node = Node::Worktree(worktree.path.clone());
                if self.overview_hand.hidden.contains(&node) {
                    let (repo, label) = self.project_label(&worktree.path);
                    let name = match repo {
                        Some(repo) => format!("{repo} · {label}"),
                        None => label.to_string(),
                    };
                    hidden.push((node, SharedString::from(name)));
                }
                for entry in self.canvas.get(&worktree.path).into_iter().flatten() {
                    let node = Node::Note(entry.path.clone());
                    if self.overview_hand.hidden.contains(&node)
                        && !hidden.iter().any(|(n, _)| *n == node)
                    {
                        let name = entry.node.heading().unwrap_or_else(|| {
                            entry
                                .path
                                .file_name()
                                .map(|n| n.to_string_lossy().into_owned())
                                .unwrap_or_default()
                        });
                        hidden.push((node, SharedString::from(name)));
                    }
                }
            }
        }
        hidden
    }

    /// Puts a hidden node back on the plane, and brings it into view.
    fn unhide(&mut self, node: &Node, cx: &mut Context<Self>) {
        if self.overview_hand.hidden.remove(node) {
            self.remember_folds(node, cx);
            self.overview_reveal = Some(node.clone());
            cx.notify();
        }
    }

    /// « Hidden (n) »: what was taken off the plane, one entry each to bring
    /// it back, and all of it at once. Absent while nothing is hidden.
    pub(super) fn render_hidden_menu(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let hidden = self.hidden_nodes(cx);
        if hidden.is_empty() {
            return None;
        }
        let app = cx.entity().downgrade();
        let count = hidden.len();
        Some(
            Button::new("overview-hidden")
                .ghost()
                .small()
                .icon(icon("eye-off"))
                .label(tr!("overview-hidden", { count: count }))
                .dropdown_menu(move |menu, _, _| {
                    let every: Vec<Node> = hidden.iter().map(|(node, _)| node.clone()).collect();
                    let all = app.clone();
                    let menu = hidden.iter().cloned().fold(menu, |menu, (node, name)| {
                        let app = app.clone();
                        menu.item(PopupMenuItem::new(name).icon(icon("eye")).on_click(
                            move |_, _, cx| {
                                if let Some(app) = app.upgrade() {
                                    let node = node.clone();
                                    app.update(cx, |this, cx| this.unhide(&node, cx));
                                }
                            },
                        ))
                    });
                    menu.separator()
                        .item(PopupMenuItem::new(tr!("overview-unhide-all")).on_click(
                            move |_, _, cx| {
                                if let Some(app) = all.upgrade() {
                                    let every = every.clone();
                                    app.update(cx, |this, cx| {
                                        for node in &every {
                                            this.unhide(node, cx);
                                        }
                                    });
                                }
                            },
                        ))
                })
                .into_any_element(),
        )
    }

    /// Writes what the hand folded or hid for one node back to the store.
    /// A terminal's fold goes with the rest of it, in `persist_terminals`.
    pub(super) fn remember_folds(&self, node: &Node, cx: &mut Context<Self>) {
        let folded = self.overview_hand.collapsed.contains(node);
        let hidden = self.overview_hand.hidden.contains(node);
        super::store::Store::update_global(cx, |store| match node {
            Node::Git(main) => store.repos.entry(main.clone()).or_default().home_collapsed = folded,
            Node::Worktree(path) => {
                let state = store.worktrees.entry(path.clone()).or_default();
                state.home_collapsed = folded;
                state.home_hidden = hidden;
            }
            Node::Note(path) => {
                let place = store.home_places.entry(path.clone()).or_default();
                place.collapsed = folded;
                place.hidden = hidden;
            }
            Node::Terminal(_) => {}
        });
    }

    /// A note moved between the checkout and the vault keeps its place: the
    /// hand's arrangement follows the file to its new name.
    pub(super) fn rename_node(&mut self, from: &Node, to: Node, cx: &mut Context<Self>) {
        let hand = &mut self.overview_hand;
        if let Some(offset) = hand.moved.remove(from) {
            hand.moved.insert(to.clone(), offset);
        }
        if let Some(size) = hand.sizes.remove(from) {
            hand.sizes.insert(to.clone(), size);
        }
        if hand.collapsed.remove(from) {
            hand.collapsed.insert(to.clone());
        }
        if let (Node::Note(from), Node::Note(to)) = (from, &to) {
            super::store::Store::update_global(cx, |store| {
                if let Some(place) = store.home_places.remove(from) {
                    store.home_places.insert(to.clone(), place);
                }
            });
        }
    }

    /// A node gone for good: nothing of its arrangement is kept.
    pub(super) fn forget_node(&mut self, node: &Node, cx: &mut Context<Self>) {
        let hand = &mut self.overview_hand;
        hand.moved.remove(node);
        hand.sizes.remove(node);
        hand.collapsed.remove(node);
        if let Node::Note(path) = node {
            super::store::Store::update_global(cx, |store| {
                store.home_places.remove(path);
            });
        }
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
