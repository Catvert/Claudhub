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
use std::rc::Rc;

use gpui_kit::component::{
    button::{Button, ButtonVariants as _},
    checkbox::Checkbox,
    h_flex,
    menu::{DropdownMenu as _, PopupMenuItem},
    popover::Popover,
    v_flex, ActiveTheme, Disableable as _, Selectable as _, Sizable as _, WindowExt as _,
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

/// How often a flowing link moves: thirty frames a second is motion to the
/// eye, and half what the display would ask — each one paints the screen.
const FLOW_FRAME: std::time::Duration = std::time::Duration::from_millis(33);
/// How often a waiting link pulses, when nothing flows: a breath is slow,
/// and a question can wait for hours — at half the frames.
const PULSE_FRAME: std::time::Duration = std::time::Duration::from_millis(66);
/// A worktree's column: a terminal's eighty columns, and a little.
const COLUMN_WORKTREE: f32 = 640.;
/// A repository's column: its git node and its notes.
const COLUMN_REPO: f32 = 320.;
/// A waiting link's breath, in seconds.
const PULSE_PERIOD: f32 = 1.6;

/// When the links started flowing, once: their phase is read off the clock,
/// so a frame late is a step longer and not a slower march.
static FLOW_EPOCH: std::sync::OnceLock<std::time::Instant> = std::sync::OnceLock::new();

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

/// A link as the canvas paints it: its two ends on screen, its kind,
/// whether it leads into the worktree on show, and what the agent at its
/// end is doing.
type Segment = (overview::Link, bool, overview::Doing);

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
    /// A node in a column, by its bottom edge: its height only.
    Height(Node, gpui_kit::Point<Pixels>),
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
    /// The repositories the plane shows: the ones ticked in the corner, the
    /// active worktree's until one is, or all of them.
    pub(super) fn overview_repos(&self) -> Vec<&crate::ui::repos::RepoState> {
        if self.overview_all {
            return self.repos.iter().collect();
        }
        let ticked: Vec<&crate::ui::repos::RepoState> = self
            .repos
            .iter()
            .filter(|repo| self.overview_projects.contains(&repo.main))
            .collect();
        if !ticked.is_empty() {
            return ticked;
        }
        let followed = self
            .active
            .as_deref()
            .and_then(|active| self.main_of(active));
        match followed {
            Some(main) => self.repos.iter().filter(|repo| repo.main == main).collect(),
            None => self.repos.iter().take(1).collect(),
        }
    }

    /// The worktrees of a repository the plane shows: the ticked ones, or
    /// all of them when none is — a folder gone from the disk never, having
    /// nothing to show but its absence, which the picker already says.
    pub(super) fn overview_worktrees_of(&self, repo: &crate::ui::repos::RepoState) -> Vec<PathBuf> {
        let live: Vec<PathBuf> = repo
            .worktrees
            .iter()
            .filter(|worktree| !worktree.prunable)
            .map(|worktree| worktree.path.clone())
            .collect();
        overview::shown_of(&self.overview_worktrees, &live)
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
        let groups: Vec<Group> = self
            .overview_repos()
            .into_iter()
            .map(|repo| (repo, self.overview_worktrees_of(repo)))
            .map(|(repo, shown)| Group {
                main: &repo.main,
                notes: repo_notes(repo),
                checkouts: repo
                    .worktrees
                    .iter()
                    // The worktrees not ticked are not on this plane; the git
                    // node and the repository's notes stay, being every
                    // worktree's.
                    .filter(|worktree| shown.contains(&worktree.path))
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
    ) -> AnyElement {
        // The places kept from the last session, once — the screen may come
        // up with the window, before any toggle has read them.
        self.load_overview_places(cx);
        if self.overview_columns {
            return self.render_overview_columns(window, cx);
        }
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

        // An agent at work flows along the link to it, one that waits
        // pulses, and both need frames nobody else asks for. Under a
        // maximised node the links are under the veil: nothing to move.
        let at_work = self.overview_at_work(&plan, cx);
        self.overview_flow_frame =
            (self.overview_maximized.is_none() && !at_work.is_empty()).then(|| {
                if at_work.flows() {
                    FLOW_FRAME
                } else {
                    PULSE_FRAME
                }
            });
        if self.overview_flow_frame.is_some() {
            self.tick_flow(cx);
        }
        let links = self.render_links(&plan, view, &at_work, cx);
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
                let doing = at_work
                    .terminals
                    .get(&id.as_u64())
                    .copied()
                    .unwrap_or(overview::Doing::Rest);
                let element = self.render_tile(
                    terminal,
                    Some(view.screen(rect)),
                    view.zoom,
                    doing,
                    window,
                    cx,
                );
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
            .child(
                div()
                    .absolute()
                    .top_3()
                    .right_3()
                    .child(self.render_overview_toolbar(cx)),
            )
            .child(self.render_zoom_bar(view, cx))
            .into_any_element()
    }

    /// The home screen as columns: one per worktree, its card on top, its
    /// terminals under it sharing the height, then its notes — and before a
    /// project's worktrees, a narrower one for the repository: its git node
    /// and its notes. The same nodes as the plane, painted by the same
    /// functions, only placed by a layout instead of a hand.
    ///
    /// **What the plane's links said, the boxes say**: an agent at work or
    /// waiting dresses its terminal's outline, and — with no agent terminal
    /// on show — its card's.
    fn render_overview_columns(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let plan = self.overview_plan();
        let at_work = self.overview_at_work(&plan, cx);
        self.overview_flow_frame = (!at_work.is_empty()).then(|| {
            if at_work.flows() {
                FLOW_FRAME
            } else {
                PULSE_FRAME
            }
        });
        if self.overview_flow_frame.is_some() {
            self.tick_flow(cx);
        }
        // The terminals measure the box they are given, as in the dock.
        for terminal in &self.terminals {
            terminal.view.update(cx, |view, _| view.set_canvas(None));
        }
        let hidden = self.overview_hand.hidden.clone();
        // What the columns hold, owned: the plane's groups, in the plane's
        // order — a worktree under the one it branched from comes after it.
        type Checkouts = Vec<(PathBuf, Vec<u64>, Vec<PathBuf>)>;
        let projects: Vec<(PathBuf, Vec<PathBuf>, Checkouts)> = self
            .overview_groups()
            .into_iter()
            .map(|group| {
                let mut checkouts: Checkouts = group
                    .checkouts
                    .iter()
                    .filter(|checkout| {
                        !hidden.contains(&Node::Worktree(checkout.path.to_path_buf()))
                    })
                    .map(|checkout| {
                        (
                            checkout.path.to_path_buf(),
                            checkout.terminals.iter().map(|(id, _)| *id).collect(),
                            checkout
                                .notes
                                .iter()
                                .filter(|note| !hidden.contains(&Node::Note((*note).clone())))
                                .cloned()
                                .collect(),
                        )
                    })
                    .collect();
                checkouts.sort_by_key(|(path, _, _)| {
                    plan.cards
                        .iter()
                        .position(|card| &card.path == path)
                        .unwrap_or(usize::MAX)
                });
                let notes = group
                    .notes
                    .iter()
                    .filter(|note| !hidden.contains(&Node::Note((*note).clone())))
                    .cloned()
                    .collect();
                (group.main.to_path_buf(), notes, checkouts)
            })
            .collect();

        // Each column keeps its scroll from one frame to the next; the ones
        // gone take theirs with them.
        let keys: Vec<String> = projects
            .iter()
            .flat_map(|(main, _, checkouts)| {
                std::iter::once(column_key(&Node::Git(main.clone()))).chain(
                    checkouts
                        .iter()
                        .map(|(path, _, _)| column_key(&Node::Worktree(path.clone()))),
                )
            })
            .collect();
        self.overview_column_scrolls
            .retain(|key, _| keys.contains(key));
        for key in &keys {
            self.overview_column_scrolls.entry(key.clone()).or_default();
        }
        let scroll_of = |node: Node| {
            let key = column_key(&node);
            let scroll = self.overview_column_scrolls[&key].clone();
            (key, scroll)
        };

        let body: AnyElement = match self.overview_zoomed.clone() {
            Some(node) => div()
                .flex_1()
                .min_h_0()
                .p_3()
                .child(
                    v_flex()
                        .size_full()
                        .child(self.column_node(&node, false, &at_work, window, cx)),
                )
                .into_any_element(),
            None if projects.is_empty() => v_flex()
                .flex_1()
                .items_center()
                .justify_center()
                .text_sm()
                .text_color(cx.theme().muted_foreground)
                .child(tr!("overview-empty"))
                .into_any_element(),
            None => {
                let mut columns: Vec<AnyElement> = Vec::new();
                for (main, notes, checkouts) in &projects {
                    let nodes: Vec<AnyElement> = std::iter::once(Node::Git(main.clone()))
                        .chain(notes.iter().cloned().map(Node::Note))
                        .map(|node| self.column_node(&node, true, &at_work, window, cx))
                        .collect();
                    let (key, scroll) = scroll_of(Node::Git(main.clone()));
                    columns.push(column(&key, COLUMN_REPO, &scroll, nodes));
                    for (path, terminals, notes) in checkouts {
                        let nodes: Vec<AnyElement> = std::iter::once(Node::Worktree(path.clone()))
                            .chain(terminals.iter().copied().map(Node::Terminal))
                            .chain(notes.iter().cloned().map(Node::Note))
                            .map(|node| self.column_node(&node, true, &at_work, window, cx))
                            .collect();
                        let (key, scroll) = scroll_of(Node::Worktree(path.clone()));
                        columns.push(column(&key, COLUMN_WORKTREE, &scroll, nodes));
                    }
                }
                let row = h_flex()
                    .id("overview-columns")
                    .track_scroll(&self.overview_columns_scroll)
                    .size_full()
                    .p_3()
                    .gap_3()
                    .items_start()
                    .overflow_x_scroll()
                    .children(columns);
                div()
                    .flex_1()
                    .min_h_0()
                    .child(super::scroll::both(
                        "overview-columns-bar",
                        &self.overview_columns_scroll,
                        row,
                    ))
                    .into_any_element()
            }
        };
        v_flex()
            .id("overview")
            .flex_1()
            .min_h_0()
            .min_w_0()
            .overflow_hidden()
            .bg(super::theme::gutter(cx))
            // A node's height is dragged from its bottom edge, and the drag
            // is followed from here — the pointer leaves the edge at once.
            .on_mouse_move(cx.listener(|this, event: &MouseMoveEvent, _, cx| {
                this.overview_dragged(event, cx);
            }))
            .capture_any_mouse_up(cx.listener(|this, _, _, cx| {
                this.end_overview_drag(cx);
            }))
            .child(
                h_flex()
                    .flex_none()
                    .justify_end()
                    .px_3()
                    .pt_3()
                    .child(self.render_overview_toolbar(cx)),
            )
            .child(body)
            .into_any_element()
    }

    /// One node in a column — or, `in_column` false, filling the columns.
    ///
    /// Each node has its height, and the column scrolls when they add up to
    /// more than the screen: a terminal its own — a column's width is not a
    /// card's — the others the one the hand gave them on the plane, or the
    /// one they start at there. Folded, any node is its head. Unfolded, its
    /// bottom edge is a grip for its height.
    fn column_node(
        &self,
        node: &Node,
        in_column: bool,
        at_work: &overview::AtWork,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let folded = self.overview_hand.collapsed.contains(node);
        let kept = |start: (f32, f32)| {
            if folded {
                overview::HEAD
            } else {
                self.overview_hand
                    .sizes
                    .get(node)
                    .map(|size| size.1)
                    .unwrap_or(start.1)
            }
        };
        let grip = (in_column && !folded).then(|| height_grip(node.clone(), cx));
        let boxed = |height: f32, child: AnyElement| {
            div()
                .relative()
                .w_full()
                .when(!in_column, |el| el.size_full())
                .when(in_column, |el| el.flex_none().h(px(height)))
                .child(child)
                .children(grip)
                .into_any_element()
        };
        match node {
            Node::Git(main) => boxed(kept(overview::GIT), self.render_git_node(main, 1., cx)),
            Node::Note(path) => boxed(kept(overview::NOTE), self.render_home_note(path, 1., cx)),
            Node::Worktree(path) => {
                let doing = at_work
                    .cards
                    .get(path)
                    .copied()
                    .unwrap_or(overview::Doing::Rest);
                let card = div()
                    .relative()
                    .size_full()
                    .child(self.render_worktree_card(path, 1., cx))
                    .when(doing != overview::Doing::Rest, |el| {
                        el.child(tile_outline(doing, false, 1., cx.theme()))
                    })
                    .into_any_element();
                boxed(kept(overview::CARD), card)
            }
            Node::Terminal(id) => {
                let Some(terminal) = self
                    .terminals
                    .iter()
                    .find(|terminal| terminal.view.entity_id().as_u64() == *id)
                else {
                    return div().into_any_element();
                };
                let doing = at_work
                    .terminals
                    .get(id)
                    .copied()
                    .unwrap_or(overview::Doing::Rest);
                let height = if folded {
                    overview::HEAD
                } else {
                    terminal.column_height
                };
                let tile = self.render_tile(terminal, None, 1., doing, window, cx);
                boxed(height, tile)
            }
        }
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
                        let (default, min) = overview::start_and_least(&node);
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
            Drag::Height(node, last) => {
                let (_, dy) = moved(last);
                match &node {
                    Node::Terminal(id) => {
                        if let Some(terminal) = self
                            .terminals
                            .iter_mut()
                            .find(|t| t.view.entity_id().as_u64() == *id)
                        {
                            terminal.column_height =
                                (terminal.column_height + dy).max(overview::MIN_COLUMN_TILE);
                        }
                    }
                    Node::Git(_) | Node::Worktree(_) | Node::Note(_) => {
                        // The plane's own size, height only: what one gives a
                        // note here is what it has there.
                        let (default, min) = overview::start_and_least(&node);
                        let size = self
                            .overview_hand
                            .sizes
                            .entry(node.clone())
                            .or_insert(default);
                        size.1 = (size.1 + dy).max(min.1);
                    }
                }
                Drag::Height(node, at)
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
            // A terminal's height lives no longer than the terminal.
            Drag::Height(Node::Terminal(_), _) => return,
            Drag::Height(node, _) => {
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

    /// Which projects the plane shows, top right: one at a time by default,
    /// so that five worktrees of one code are not read among a dozen of
    /// another's — and as many as one ticks.
    fn render_overview_toolbar(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let shown: Vec<PathBuf> = self
            .overview_repos()
            .iter()
            .map(|repo| repo.main.clone())
            .collect();
        let label: SharedString = match (self.overview_all, self.overview_repos().as_slice()) {
            (true, _) => tr!("overview-all-projects"),
            (false, [one]) => SharedString::from(one.name.clone()),
            (false, many) => tr!("overview-n-projects", { count: many.len() }),
        };
        let app = cx.entity().downgrade();
        let everything = app.clone();
        let rows: Vec<Pick> = self
            .repos
            .iter()
            .map(|repo| {
                let app = app.clone();
                let main = repo.main.clone();
                Pick::Item {
                    name: SharedString::from(repo.name.clone()),
                    ticked: shown.contains(&repo.main),
                    press: Rc::new(move |cx: &mut App| {
                        if let Some(app) = app.upgrade() {
                            app.update(cx, |this, cx| this.press_project(&main, cx));
                        }
                    }),
                }
            })
            .collect();
        let all = Pick::Item {
            name: tr!("overview-all-projects"),
            ticked: self.overview_all,
            press: Rc::new(move |cx: &mut App| {
                if let Some(app) = everything.upgrade() {
                    app.update(cx, |this, cx| this.press_all_projects(cx));
                }
            }),
        };
        let columns = self.overview_columns;
        h_flex()
            .gap_1()
            .p_1()
            .rounded(cx.theme().radius_lg)
            .border_1()
            .border_color(cx.theme().border)
            .bg(cx.theme().background)
            .shadow_md()
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
            // The two ways to look at the same things, as tabs: the plane,
            // and the columns.
            .child(
                Button::new("overview-mode-canvas")
                    .ghost()
                    .small()
                    .icon(icon("grid-3x3"))
                    .label(tr!("overview-mode-canvas"))
                    .tooltip(tr!("overview-mode-hint"))
                    .selected(!columns)
                    .on_click(cx.listener(|this, _, _, cx| this.set_overview_columns(false, cx))),
            )
            .child(
                Button::new("overview-mode-columns")
                    .ghost()
                    .small()
                    .icon(icon("square-kanban"))
                    .label(tr!("overview-mode-columns"))
                    .tooltip(tr!("overview-mode-hint"))
                    .selected(columns)
                    .on_click(cx.listener(|this, _, _, cx| this.set_overview_columns(true, cx))),
            )
            .child(div().mx_0p5().w(px(1.)).h(px(18.)).bg(cx.theme().border))
            .child(pick_list(
                "overview-project",
                "folder",
                label,
                tr!("overview-projects-hint"),
                all,
                rows,
            ))
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

    /// Which worktrees of the projects on show the plane shows, beside the
    /// project picker: all of them by default, or the ones ticked — per
    /// project, one where nothing is ticked showing all of its own. Only
    /// when there is a choice to make.
    fn render_overview_worktree_picker(&self, cx: &mut Context<Self>) -> Option<impl IntoElement> {
        let repos = self.overview_repos();
        let headed = repos.len() > 1;
        let app = cx.entity().downgrade();
        let mut rows: Vec<Pick> = Vec::new();
        let (mut choices, mut shown_count, mut filtered) = (0, 0, false);
        let mut only: Option<SharedString> = None;
        for repo in &repos {
            let shown = self.overview_worktrees_of(repo);
            let live = repo.worktrees.iter().filter(|worktree| !worktree.prunable);
            if headed {
                rows.push(Pick::Heading(SharedString::from(repo.name.clone())));
            }
            for worktree in live {
                let label = worktree.label();
                let name = SharedString::from(match worktree.branch.as_deref() {
                    Some(branch) if branch != label => format!("{label} · {branch}"),
                    _ => label,
                });
                let ticked = shown.contains(&worktree.path);
                choices += 1;
                if ticked {
                    shown_count += 1;
                    only = Some(name.clone());
                } else {
                    filtered = true;
                }
                let (app, main, path) = (app.clone(), repo.main.clone(), worktree.path.clone());
                rows.push(Pick::Item {
                    name,
                    ticked,
                    press: Rc::new(move |cx: &mut App| {
                        if let Some(app) = app.upgrade() {
                            app.update(cx, |this, cx| this.press_worktree(&main, &path, cx));
                        }
                    }),
                });
            }
        }
        if choices < 2 {
            return None;
        }
        let label = match (filtered, shown_count) {
            (false, _) => tr!("overview-all-worktrees"),
            (true, 1) => only.unwrap_or_default(),
            (true, count) => tr!("overview-n-worktrees", { count: count }),
        };
        let all = Pick::Item {
            name: tr!("overview-all-worktrees"),
            ticked: !filtered,
            press: Rc::new(move |cx: &mut App| {
                if let Some(app) = app.upgrade() {
                    app.update(cx, |this, cx| this.show_all_worktrees(cx));
                }
            }),
        };
        Some(pick_list(
            "overview-worktree",
            "git-branch",
            label,
            tr!("overview-worktree-hint"),
            all,
            rows,
        ))
    }

    /// « All projects » pressed: every project, or — pressed again — back to
    /// the active worktree's alone.
    fn press_all_projects(&mut self, cx: &mut Context<Self>) {
        self.overview_all = !self.overview_all;
        self.overview_projects.clear();
        self.reframe(cx);
        self.ask_skill_status();
    }

    /// A project pressed: on the plane if it was not, off if it was — never
    /// the last one. Ticking every one is « all projects ».
    fn press_project(&mut self, main: &Path, cx: &mut Context<Self>) {
        let shown: Vec<PathBuf> = self
            .overview_repos()
            .iter()
            .map(|repo| repo.main.clone())
            .collect();
        let Some(ticked) = overview::toggle(&shown, main) else {
            return;
        };
        self.overview_all = ticked.len() == self.repos.iter().count();
        self.overview_projects = if self.overview_all {
            Vec::new()
        } else {
            ticked
        };
        self.reframe(cx);
        self.ask_skill_status();
    }

    /// A worktree pressed: on the plane if it was not, off if it was —
    /// never its project's last one. Ticking all of a project's is the same
    /// as ticking none, and is kept as none: a worktree created afterwards
    /// then comes on the plane.
    fn press_worktree(&mut self, main: &Path, path: &Path, cx: &mut Context<Self>) {
        let Some(repo) = self.repos.iter().find(|repo| repo.main == main) else {
            return;
        };
        let live: Vec<PathBuf> = repo
            .worktrees
            .iter()
            .filter(|worktree| !worktree.prunable)
            .map(|worktree| worktree.path.clone())
            .collect();
        let shown = overview::shown_of(&self.overview_worktrees, &live);
        let Some(ticked) = overview::toggle(&shown, path) else {
            return;
        };
        self.overview_worktrees
            .retain(|picked| !live.contains(picked));
        if ticked.len() < live.len() {
            self.overview_worktrees.extend(ticked);
        }
        self.reframe(cx);
    }

    /// Every worktree of the projects on show.
    fn show_all_worktrees(&mut self, cx: &mut Context<Self>) {
        self.overview_worktrees.clear();
        self.reframe(cx);
    }

    /// Switches the home screen between its plane and its columns, and
    /// remembers which.
    fn set_overview_columns(&mut self, columns: bool, cx: &mut Context<Self>) {
        if self.overview_columns == columns {
            return;
        }
        self.overview_columns = columns;
        self.overview_drag = None;
        super::store::Store::update_global(cx, |store| store.session.home_columns = columns);
        // The terminals measure their own box in the columns, and are told
        // their card's grid again on the plane's next frame.
        for terminal in &self.terminals {
            terminal.view.update(cx, |view, _| view.set_canvas(None));
        }
        self.reframe(cx);
    }

    /// Another plane to frame, as after picking another project.
    fn reframe(&mut self, cx: &mut Context<Self>) {
        self.overview_zoomed = None;
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

    /// Where an agent is at work on the plane — see `overview::at_work`.
    ///
    /// **Claude's own status first** (`busy`, `waiting`, `idle`, written
    /// for its pid): the processor cannot tell a turn under way from a prompt
    /// being typed, both burn it. Then the hooks' word for the terminal's
    /// session, then — for another agent, or a Claude that writes no status —
    /// the guess.
    fn overview_at_work(&self, plan: &Plan, cx: &App) -> overview::AtWork {
        use overview::Doing;
        let claude_says = |status: &str| match status {
            "busy" => Doing::Working,
            "waiting" => Doing::Waiting,
            _ => Doing::Rest,
        };
        let heard = |activity: &crate::agent::Activity| match activity {
            crate::agent::Activity::Working => Doing::Working,
            crate::agent::Activity::Waiting(_) => Doing::Waiting,
            crate::agent::Activity::Idle | crate::agent::Activity::Finished => Doing::Rest,
        };
        let status = |pid: Option<u32>, session: Option<&str>| {
            self.claude_processes
                .iter()
                .find(|process| {
                    pid == Some(process.pid) || session == Some(process.session.as_str())
                })
                .and_then(|process| process.status.as_deref())
                .map(claude_says)
        };
        let tiles: Vec<overview::AgentTile> = self
            .terminals
            .iter()
            .filter(|terminal| plan.tile(terminal.view.entity_id().as_u64()).is_some())
            .map(|terminal| {
                // The pty's child is Claude itself when the tab was launched
                // on it; typed at a prompt, it is the pid read under the
                // shell.
                let pid = terminal
                    .typed
                    .as_ref()
                    .map(|(_, _, pid)| *pid)
                    .or_else(|| terminal.view.read(cx).child());
                let session = terminal.session.as_deref();
                let claude = status(pid, session);
                overview::AgentTile {
                    id: terminal.view.entity_id().as_u64(),
                    worktree: &terminal.worktree,
                    agent: claude.is_some()
                        || matches!(
                            terminal.relaunch,
                            Some(crate::ui::store::Relaunch::Agent { .. })
                        )
                        || terminal.typed.is_some(),
                    word: claude.or_else(|| {
                        session
                            .and_then(|session| self.agents.session(session))
                            .map(heard)
                    }),
                }
            })
            .collect();
        let cards: Vec<PathBuf> = plan.cards.iter().map(|card| card.path.clone()).collect();
        let worktrees: std::collections::HashMap<&Path, Doing> = plan
            .cards
            .iter()
            .map(|card| {
                let path = card.path.as_path();
                // The Claudes of this worktree that say how they are: the
                // loudest of them is the answer, all idle too — the guess
                // only speaks where none of them does.
                let said: Vec<Doing> = self
                    .claude_processes
                    .iter()
                    .filter(|process| {
                        crate::agent::owning_worktree(&cards, &process.cwd).as_deref() == Some(path)
                    })
                    .filter_map(|process| process.status.as_deref())
                    .map(claude_says)
                    .collect();
                let doing = if said.is_empty() {
                    self.agents
                        .get(path)
                        .map(|state| heard(&state.activity))
                        .unwrap_or(Doing::Rest)
                } else if said.contains(&Doing::Waiting) {
                    Doing::Waiting
                } else if said.contains(&Doing::Working) {
                    Doing::Working
                } else {
                    Doing::Rest
                };
                (path, doing)
            })
            .collect();
        overview::at_work(&tiles, &worktrees)
    }

    /// Asks for the frames a link moves by, for as long as one moves and the
    /// screen is up — the last frame that saw none ends it. The spacing is
    /// read again at every one: a pulse wants fewer than a flow.
    fn tick_flow(&mut self, cx: &mut Context<Self>) {
        if self.overview_flow_ticking {
            return;
        }
        self.overview_flow_ticking = true;
        cx.spawn(async move |this, cx| {
            let mut frame = FLOW_FRAME;
            loop {
                cx.background_executor().timer(frame).await;
                let next = this
                    .update(cx, |this, cx| {
                        let next = this.overview_flow_frame.filter(|_| this.overview);
                        if next.is_some() {
                            cx.notify();
                        } else {
                            this.overview_flow_ticking = false;
                        }
                        next
                    })
                    .ok()
                    .flatten();
                match next {
                    Some(next) => frame = next,
                    None => break,
                }
            }
        })
        .detach();
    }

    /// The lines between the nodes, under them.
    ///
    /// **A link to an agent at work flows**: dashes march from the card to
    /// the terminal and a comet runs down the line, in the colour the badge
    /// gives work, over a glow. **One to an agent that waits pulses**, in the
    /// colour the badge gives a question: the line breathes, and a beacon
    /// pings where it meets the terminal — nothing travels, nothing is under
    /// way. The phase is read off a clock and not counted in frames, the
    /// frames being ours to skip.
    fn render_links(
        &self,
        plan: &Plan,
        view: View,
        at_work: &overview::AtWork,
        cx: &App,
    ) -> impl IntoElement {
        let active = self.active.clone();
        let links: Vec<Segment> = plan
            .links
            .iter()
            .map(|link| {
                let on = active.as_deref() == Some(link.worktree.as_path());
                let doing = match &link.child {
                    Node::Terminal(id) => at_work.terminals.get(id).copied(),
                    Node::Worktree(path) => at_work.cards.get(path).copied(),
                    Node::Git(_) | Node::Note(_) => None,
                };
                let mut link = link.clone();
                link.from = view.point(link.from);
                link.to = view.point(link.to);
                (link, on, doing.unwrap_or(overview::Doing::Rest))
            })
            .collect();
        let quiet = cx.theme().border;
        let lit = cx.theme().ring;
        let work = cx.theme().warning;
        let asks = cx.theme().danger;
        let spark = gpui_kit::Hsla {
            l: (work.l + 0.25).min(0.95),
            ..work
        };
        let width = px((1.5 * view.zoom).clamp(1., 2.5));
        let zoom = view.zoom;
        let seconds = flow_seconds();
        canvas(
            move |_, _, _| {},
            move |bounds, _, window, _| {
                let at =
                    |(x, y): (f32, f32)| point(bounds.origin.x + px(x), bounds.origin.y + px(y));
                for (link, on, doing) in links {
                    let flows = doing != overview::Doing::Rest;
                    // What hangs from a card — a terminal, a note — is drawn
                    // finer than the tree itself: the branches are the shape,
                    // the rest is what the shape carries. Work is the
                    // exception: it is what one looks for.
                    let width = match link.kind {
                        LinkKind::Root | LinkKind::Branch => width,
                        LinkKind::Terminal | LinkKind::Note if flows => width,
                        LinkKind::Terminal | LinkKind::Note => width * 0.6,
                    };
                    // An elbow: out square to the parent's side, across
                    // half way, into the child square to its own — and the
                    // two corners rounded, never wider than half a leg.
                    let points = overview::elbow(link.from, link.from_side, link.to);
                    if !flows {
                        if let Some(path) = trace(&points, zoom, width, None, &at) {
                            window.paint_path(path, if on { lit } else { quiet });
                        }
                        continue;
                    }
                    if doing == overview::Doing::Waiting {
                        let breath = overview::breath(seconds, PULSE_PERIOD);
                        if let Some(path) = trace(&points, zoom, width * 4., None, &at) {
                            window.paint_path(path, asks.opacity(0.06 + 0.16 * breath));
                        }
                        if let Some(path) = trace(&points, zoom, width, None, &at) {
                            window.paint_path(path, asks.opacity(0.5 + 0.5 * breath));
                        }
                        // The beacon: a dot on the terminal's edge, and a ring
                        // leaving it and fading, once a breath.
                        let end = at(points[3]);
                        let dot = (3. * zoom).clamp(2.5, 4.5);
                        let ping = (seconds / PULSE_PERIOD).fract();
                        let ring = dot + ping * (10. * zoom).clamp(6., 14.);
                        window.paint_quad(disc(end, ring, asks.opacity(0.45 * (1. - ping))));
                        window.paint_quad(disc(end, dot, asks));
                        continue;
                    }
                    let length = overview::length(&points);
                    // A glow under the line, the line itself dimmed, then
                    // what moves along it.
                    if let Some(path) = trace(&points, zoom, width * 4., None, &at) {
                        window.paint_path(path, work.opacity(0.14));
                    }
                    if let Some(path) = trace(&points, zoom, width, None, &at) {
                        window.paint_path(path, work.opacity(0.35));
                    }
                    let (dash, gap) = ((7. * zoom).max(4.), (9. * zoom).max(5.));
                    let marching = overview::marching(length, seconds * 30. * zoom, dash, gap);
                    let dashes = overview::dash_array(&marching);
                    if let Some(path) = trace(&points, zoom, width, Some(&dashes), &at) {
                        window.paint_path(path, work);
                    }
                    let tail = (36. * zoom).max(16.);
                    if let Some(stretch) = overview::comet(length, seconds * 160. * zoom, tail) {
                        let dashes = overview::dash_array(&[stretch]);
                        if let Some(path) = trace(&points, zoom, width * 2., Some(&dashes), &at) {
                            window.paint_path(path, spark);
                        }
                    }
                }
            },
        )
        .absolute()
        .size_full()
    }
}

/// Where the moving links are in their motion: seconds since they first
/// moved, read off the clock.
fn flow_seconds() -> f32 {
    FLOW_EPOCH
        .get_or_init(std::time::Instant::now)
        .elapsed()
        .as_secs_f64()
        // A jump every ten minutes, where hours of f32 would stutter.
        .rem_euclid(600.) as f32
}

/// A terminal's outline, over it: the theme's line at rest — the ring when
/// it has the focus — and **the link's own dress when its agent works or
/// waits**, so that the link and the box it leads to read as one signal:
/// dashes and a comet going round over a glow, or a breathing line.
fn tile_outline(
    doing: overview::Doing,
    focused: bool,
    zoom: f32,
    theme: &gpui_kit::component::Theme,
) -> AnyElement {
    let radius = f32::from(theme.radius_lg);
    if doing == overview::Doing::Rest {
        return div()
            .absolute()
            .inset_0()
            .rounded(theme.radius_lg)
            .border_1()
            .border_color(if focused { theme.ring } else { theme.border })
            .into_any_element();
    }
    let (work, asks) = (theme.warning, theme.danger);
    let spark = gpui_kit::Hsla {
        l: (work.l + 0.25).min(0.95),
        ..work
    };
    let width = (1.5 * zoom).clamp(1., 2.5);
    let seconds = flow_seconds();
    canvas(
        move |_, _, _| {},
        move |bounds, _, window, _| {
            // Inside the box by half a line, so that nothing of the stroke
            // falls outside what the node covers.
            let inset = width / 2.;
            let (x, y) = (
                f32::from(bounds.origin.x) + inset,
                f32::from(bounds.origin.y) + inset,
            );
            let (w, h) = (
                f32::from(bounds.size.width) - width,
                f32::from(bounds.size.height) - width,
            );
            if w <= 0. || h <= 0. {
                return;
            }
            let r = radius.min(w / 2.).min(h / 2.);
            let length = overview::outline_length(w, h, r);
            let stroke =
                |line: f32, dashes: Option<&[f32]>| outline(x, y, w, h, r, px(line), dashes);
            match doing {
                overview::Doing::Waiting => {
                    let breath = overview::breath(seconds, PULSE_PERIOD);
                    if let Some(path) = stroke(width * 4., None) {
                        window.paint_path(path, asks.opacity(0.06 + 0.16 * breath));
                    }
                    if let Some(path) = stroke(width, None) {
                        window.paint_path(path, asks.opacity(0.5 + 0.5 * breath));
                    }
                }
                _ => {
                    if let Some(path) = stroke(width * 4., None) {
                        window.paint_path(path, work.opacity(0.14));
                    }
                    if let Some(path) = stroke(width, None) {
                        window.paint_path(path, work.opacity(0.35));
                    }
                    let (dash, gap) = ((7. * zoom).max(4.), (9. * zoom).max(5.));
                    let marching =
                        overview::marching_round(length, seconds * 30. * zoom, dash, gap);
                    if let Some(path) = stroke(width, Some(&overview::dash_array(&marching))) {
                        window.paint_path(path, work);
                    }
                    let tail = (36. * zoom).max(16.);
                    let comet = overview::comet_round(length, seconds * 160. * zoom, tail);
                    if let Some(path) = stroke(width * 2., Some(&overview::dash_array(&comet))) {
                        window.paint_path(path, spark);
                    }
                }
            }
        },
    )
    .absolute()
    .inset_0()
    .into_any_element()
}

/// A rounded rectangle's outline as a stroke, clockwise from the end of the
/// top-left corner — all of it, or the stretches a dash array lights. The
/// corners are quarter circles, as `overview::outline_length` measures them.
fn outline(
    x: f32,
    y: f32,
    w: f32,
    h: f32,
    r: f32,
    width: Pixels,
    dashes: Option<&[f32]>,
) -> Option<gpui_kit::Path<Pixels>> {
    let mut path = PathBuilder::stroke(width);
    if let Some(dashes) = dashes {
        if dashes.is_empty() {
            return None;
        }
        let dashes: Vec<Pixels> = dashes.iter().map(|&d| px(d)).collect();
        path = path.dash_array(&dashes);
    }
    let at = |px_: f32, py: f32| point(px(px_), px(py));
    // A quarter circle as a cubic: the control points this far along the
    // tangents.
    let k = 0.552_284_8 * r;
    let (right, bottom) = (x + w, y + h);
    path.move_to(at(x + r, y));
    path.line_to(at(right - r, y));
    path.cubic_bezier_to(at(right, y + r), at(right - r + k, y), at(right, y + r - k));
    path.line_to(at(right, bottom - r));
    path.cubic_bezier_to(
        at(right - r, bottom),
        at(right, bottom - r + k),
        at(right - r + k, bottom),
    );
    path.line_to(at(x + r, bottom));
    path.cubic_bezier_to(
        at(x, bottom - r),
        at(x + r - k, bottom),
        at(x, bottom - r + k),
    );
    path.line_to(at(x, y + r));
    path.cubic_bezier_to(at(x + r, y), at(x, y + r - k), at(x + r - k, y));
    path.build().ok()
}

/// What names a column — its element id and its scroll — by the node at
/// its head.
///
/// **By the node and not the path**: a repository's column and its main
/// checkout's have the same one, and sharing a scroll between them was a
/// bar that showed and never moved — the repository's column, with nothing
/// to scroll, clamped the offset back to nothing at every frame.
fn column_key(head: &Node) -> String {
    match head {
        Node::Git(main) => format!("repo:{}", main.display()),
        Node::Worktree(path) => format!("worktree:{}", path.display()),
        other => format!("{other:?}"),
    }
}

/// A column of nodes, as tall as the screen, scrolling on its own when they
/// are more — with a bar one can take, the wheel over a terminal being the
/// terminal's. The bar has a gutter of its own at the column's right, so it
/// never lies over a terminal: `width` is what the nodes get.
fn column(
    key: &str,
    width: f32,
    scroll: &gpui_kit::ScrollHandle,
    nodes: Vec<AnyElement>,
) -> AnyElement {
    let id = format!("overview-column-{key}");
    let gutter = super::theme::scroll_gutter();
    let list = v_flex()
        .id(SharedString::from(format!("{id}-list")))
        .track_scroll(scroll)
        .size_full()
        .gap_2()
        // Room under the last node for its grip.
        .pb_2()
        .pr(gutter)
        .overflow_y_scroll()
        .children(nodes);
    div()
        .flex_none()
        .w(px(width) + gutter)
        .h_full()
        .child(super::scroll::vertical(id, scroll, list))
        .into_any_element()
}

/// A node's bottom edge in a column, taken to give it another height: a
/// strip in the gap under it, lit under the pointer.
fn height_grip(node: Node, cx: &Context<ClaudhubApp>) -> gpui_kit::Stateful<gpui_kit::Div> {
    let lit = cx.theme().ring.opacity(0.6);
    let app = cx.entity().downgrade();
    div()
        .id(SharedString::from(format!("overview-height-{node:?}")))
        .absolute()
        .left_2()
        .right_2()
        .bottom(px(-7.))
        .h(px(6.))
        .rounded_full()
        .cursor(gpui_kit::CursorStyle::ResizeUpDown)
        .hover(move |style| style.bg(lit))
        .on_mouse_down(MouseButton::Left, move |event, _, cx| {
            cx.stop_propagation();
            if let Some(app) = app.upgrade() {
                let node = node.clone();
                app.update(cx, |this, _| {
                    this.overview_drag = Some(Drag::Height(node, event.position));
                });
            }
        })
}

/// A row of a ticked list.
enum Pick {
    /// A project's name over its worktrees, when several are on show.
    Heading(SharedString),
    Item {
        name: SharedString,
        ticked: bool,
        press: Rc<dyn Fn(&mut App)>,
    },
}

/// A button opening a list of boxes to tick, the « all » one first.
///
/// A popover and not a menu: a menu closes on every press, and ticking three
/// projects would take three openings. Its content is built again on every
/// frame from what the application says, so a press reads back at once.
fn pick_list(
    id: &'static str,
    glyph: &'static str,
    label: SharedString,
    hint: SharedString,
    all: Pick,
    rows: Vec<Pick>,
) -> impl IntoElement {
    let rows = Rc::new(rows);
    let all = Rc::new(all);
    Popover::new(id)
        .anchor(gpui_kit::Anchor::TopRight)
        .trigger(
            Button::new(SharedString::from(format!("{id}-trigger")))
                .ghost()
                .small()
                .icon(icon(glyph))
                .label(label)
                .tooltip(hint),
        )
        .content(move |_, _, cx| {
            let row = |index: usize, pick: &Pick, cx: &App| match pick {
                Pick::Heading(name) => div()
                    .pt_1()
                    .px_1()
                    .text_xs()
                    .text_color(cx.theme().muted_foreground)
                    .child(name.clone())
                    .into_any_element(),
                Pick::Item {
                    name,
                    ticked,
                    press,
                } => {
                    let press = press.clone();
                    div()
                        .px_1()
                        .py_0p5()
                        .child(
                            Checkbox::new(SharedString::from(format!("{id}-{index}")))
                                .label(name.clone())
                                .checked(*ticked)
                                .on_click(move |_, _, cx| press(cx)),
                        )
                        .into_any_element()
                }
            };
            v_flex()
                .id(SharedString::from(format!("{id}-list")))
                .min_w(px(220.))
                .max_w(px(420.))
                .max_h(px(420.))
                .overflow_y_scroll()
                .p_1()
                .gap_0p5()
                .text_sm()
                .child(row(usize::MAX, &all, cx))
                .child(div().my_0p5().h(px(1.)).bg(cx.theme().border))
                .children(
                    rows.iter()
                        .enumerate()
                        .map(|(index, pick)| row(index, pick, cx)),
                )
        })
}

/// A filled circle of `radius` around `centre`.
fn disc(
    centre: gpui_kit::Point<Pixels>,
    radius: f32,
    color: gpui_kit::Hsla,
) -> gpui_kit::PaintQuad {
    let side = px(radius * 2.);
    gpui_kit::fill(
        gpui_kit::Bounds::centered_at(centre, gpui_kit::size(side, side)),
        color,
    )
    .corner_radii(px(radius))
}

/// A link's elbow as a stroke, its two corners rounded — all of it, or the
/// stretches a dash array lights; `None` when there is nothing to paint.
fn trace(
    points: &[(f32, f32); 4],
    zoom: f32,
    width: Pixels,
    dashes: Option<&[f32]>,
    at: &impl Fn((f32, f32)) -> gpui_kit::Point<Pixels>,
) -> Option<gpui_kit::Path<Pixels>> {
    let mut path = PathBuilder::stroke(width);
    if let Some(dashes) = dashes {
        if dashes.is_empty() {
            return None;
        }
        let dashes: Vec<Pixels> = dashes.iter().map(|&d| px(d)).collect();
        path = path.dash_array(&dashes);
    }
    let radius = (10. * zoom).max(3.);
    let length = |a: (f32, f32), b: (f32, f32)| ((b.0 - a.0).powi(2) + (b.1 - a.1).powi(2)).sqrt();
    path.move_to(at(points[0]));
    for corner in 1..=2 {
        let (before, here, after) = (points[corner - 1], points[corner], points[corner + 1]);
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
    path.build().ok()
}

impl ClaudhubApp {
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
        rect: Option<Rect>,
        zoom: f32,
        doing: overview::Doing,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        // The plane's `rem`, as every node is laid out under.
        let rem = window.rem_size() * zoom;
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
            // The preset sizes are the plane's: a column gives its terminals
            // the room it has.
            .when(detail && rect.is_some(), |el| {
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
        let frame = match rect {
            Some(rect) => placed(rect),
            None => div().relative().size_full(),
        };
        Scaled {
            rem,
            child: frame
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
                .child(tile_outline(doing, focused, zoom, &theme))
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
                // The columns place their nodes themselves: nothing to drag,
                // and a drag left pending would move the node on the plane.
                if !this.overview_columns {
                    this.overview_drag = Some(Drag::Node(node.clone(), event.position));
                }
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
        (!self.overview_columns
            && !self.overview_hand.collapsed.contains(&node)
            && self.overview_maximized.is_none())
        .then(|| corner(cx.entity().downgrade(), node, cx))
    }

    /// A window's three buttons, at the end of a node's head: fold to the
    /// head, maximise, close. The git node has no cross — it is the tree's
    /// root, and a plane without it would be a plane without a project.
    pub(super) fn window_controls(&self, node: Node, cx: &mut Context<Self>) -> impl IntoElement {
        let folded = self.overview_hand.collapsed.contains(&node);
        let maximized = if self.overview_columns {
            self.overview_zoomed.as_ref() == Some(&node)
        } else {
            self.overview_maximized
                .as_ref()
                .is_some_and(|maximized| maximized.node == node)
        };
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
        // In the columns, the node fills them, and the second press gives
        // them back: there is no size of its own to grow.
        if self.overview_columns {
            self.overview_zoomed = match &self.overview_zoomed {
                Some(zoomed) if zoomed == node => None,
                _ => Some(node.clone()),
            };
            cx.notify();
            return;
        }
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
        for repo in self.overview_repos() {
            let shown = self.overview_worktrees_of(repo);
            for worktree in &repo.worktrees {
                if !shown.contains(&worktree.path) {
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
