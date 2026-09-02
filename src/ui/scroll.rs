//! Scrollbars, and wheel smoothing.
//!
//! A virtualised list says nothing about where you are: `uniform_list` paints
//! only its entries, and nothing in the window distinguishes "three lines
//! left" from "three thousand left". An agent-review diff routinely runs to
//! several thousand, and a Laravel project's explorer to forty thousand: the
//! bar is the only positional cue such a list has.
//!
//! It sits **over** the content rather than beside it: it is positioned
//! absolutely, hence the container's `relative`. Reserving a column for it
//! would cut sixteen pixels of usable width from every panel, and half of them
//! have nothing to scroll most of the time.
//!
//! Three details, all observed on screen and none guessed:
//!
//! - **`min_h_0` and `min_w_0`.** The container is a flex item, whose minimum
//!   size defaults to its content's: without them it takes the height of the
//!   tree's eight thousand rows and the width of the longest file name. The
//!   list itself stays the right size — it is the bar that paints three
//!   hundred pixels to the right of the panel, out of sight.
//! - **`overflow_hidden`**, for the same reason downstream: what overflows the
//!   container must not cover the neighbouring panel.
//! - **gpui-component's `scrollbar()` rather than a bare `Scrollbar` child.**
//!   The extension wraps the bar in an absolute layer pinned to all four
//!   edges; placed bare, it gets no usable bounds and paints nothing at all.
//!   This is the only point in this list that does not follow from the layout.
//!
//! The bar's container is also the right place for the wheel listener: it does
//! not scroll itself, so its handler runs after the list's — which is what
//! `ui::motion` expects, since it takes over a jump already applied. A single
//! key names the bar and keys the motion, so no panel can animate another's
//! offset.
//!
//! The id is **given to the container**, and it is distinct per call: the layer
//! `scrollbar()` installs is always called `scrollbar_layer`, and without an
//! identified parent the panels would share the state — hover, drag — of one
//! and the same bar.

use gpui::{div, prelude::*, AnyElement, Context, SharedString, Stateful, Window};
use gpui_component::scroll::{ScrollableElement, ScrollbarAxis, ScrollbarHandle};

use crate::ui::app::ClaudhubApp;
use crate::ui::motion::{Axes, ScrollMotion};

/// A vertical bar, **without** wheel smoothing.
///
/// For what is not a panel: the help window is built inside a dialog closure,
/// which only receives an `App` and therefore cannot subscribe to the wheel
/// through `cx.listener`.
///
/// A view that *can* — either picker — wraps this in [`smooth_wheel`] and gets
/// the same motion as a panel: the bar and the listener are two halves, and
/// only the second one needs a context.
pub fn vertical<H: ScrollbarHandle + Clone>(
    id: impl Into<SharedString>,
    handle: &H,
    content: impl IntoElement,
) -> Stateful<gpui::Div> {
    wrap(
        id,
        handle,
        ScrollbarAxis::Vertical,
        content.into_any_element(),
    )
}

/// A bar on both axes, **without** wheel smoothing.
///
/// The only panel that wants one is the diff: its lines never wrap, so it
/// overflows in width too, and its wheel is already taken by zoom — it drives
/// its own motion.
pub fn both<H: ScrollbarHandle + Clone>(
    id: impl Into<SharedString>,
    handle: &H,
    content: impl IntoElement,
) -> Stateful<gpui::Div> {
    wrap(id, handle, ScrollbarAxis::Both, content.into_any_element())
}

/// The wheel listener a **view of its own** installs on its own bar.
///
/// The panels keep their smoothing on the application, keyed by the bar's id,
/// because a panel is not an entity: `ClaudhubApp::scrolled` is that path. A
/// view that *is* one — either picker — holds a `ScrollMotion` in a field
/// instead, and there is nothing to key: one list, one motion. This is the same
/// harness written against it, and the placement is the same for the same
/// reason — the bar's container does not scroll itself, so this runs **after**
/// the list's own handler, in the bubble phase, which is what
/// `ScrollMotion::on_wheel` expects: it takes over a jump already applied.
///
/// The caller advances the motion itself, from its `render`: it has the `&mut
/// self` this cannot reach — reading the entity back out of its own render is a
/// reentrant borrow.
pub fn smooth_wheel<V: gpui::Render>(
    element: Stateful<gpui::Div>,
    base: gpui::ScrollHandle,
    motion: impl Fn(&mut V) -> &mut ScrollMotion + 'static,
    cx: &mut Context<V>,
) -> Stateful<gpui::Div> {
    element.on_scroll_wheel(
        cx.listener(move |view, event: &gpui::ScrollWheelEvent, window, cx| {
            if motion(view).on_wheel(&base, event, window) {
                cx.notify();
            }
        }),
    )
}

fn wrap<H: ScrollbarHandle + Clone>(
    id: impl Into<SharedString>,
    handle: &H,
    axis: ScrollbarAxis,
    content: AnyElement,
) -> Stateful<gpui::Div> {
    div()
        .id(gpui::ElementId::Name(id.into()))
        .relative()
        .size_full()
        .min_h_0()
        .min_w_0()
        .overflow_hidden()
        .child(content)
        .scrollbar(handle, axis)
}

/// What we can pull the handle gpui actually animates out of.
///
/// `UniformListScrollHandle` is not a handle: it is a list state containing
/// one, and that inner handle is what the wheel moves.
pub trait Scrollable: ScrollbarHandle + Clone {
    fn base(&self) -> gpui::ScrollHandle;
}

impl Scrollable for gpui::ScrollHandle {
    fn base(&self) -> gpui::ScrollHandle {
        self.clone()
    }
}

impl Scrollable for gpui::UniformListScrollHandle {
    fn base(&self) -> gpui::ScrollHandle {
        self.0.borrow().base_handle.clone()
    }
}

impl Scrollable for gpui_component::VirtualListScrollHandle {
    fn base(&self) -> gpui::ScrollHandle {
        self.base_handle().clone()
    }
}

/// Is this notch ours to smooth?
///
/// Only a wheel — a trackpad sends `ScrollDelta::Pixels`, which is the finger
/// itself — only the vertical axis, and only while the view has somewhere to
/// go: at the edge the event has to bubble, exactly as the mask lets it.
fn takes_over(
    handle: &gpui::ScrollHandle,
    event: &gpui::ScrollWheelEvent,
    window: &Window,
) -> bool {
    if !matches!(event.delta, gpui::ScrollDelta::Lines(_)) {
        return false;
    }
    let delta = event.delta.pixel_delta(window.line_height());
    if delta.y == gpui::px(0.) || delta.x.abs() > delta.y.abs() {
        return false;
    }
    let max = handle.max_offset().y.max(gpui::px(0.));
    let at = handle.offset().y.clamp(-max, gpui::px(0.));
    (at + delta.y).clamp(-max, gpui::px(0.)) != at
}

impl ClaudhubApp {
    /// A panel's smoothing, created on its first wheel event.
    ///
    /// The key is **the bar's id**: one value for both, and there is no way to
    /// animate one panel's motion on another's offset — which would make it
    /// jump from one end to the other.
    pub(super) fn motion(&mut self, id: SharedString, axes: Axes) -> &mut ScrollMotion {
        self.motions
            .entry(id)
            .or_insert_with(|| ScrollMotion::new(axes))
    }

    /// The smoothing of a surface we scroll ourselves, the built-in editor's.
    ///
    /// It differs on one point — an offset read back late does not cancel the
    /// transition — and that point is the whole difference between a smoothed
    /// editor and one that does not move at all. See `ScrollMotion::resync`.
    pub(super) fn owned_motion(&mut self, id: SharedString, axes: Axes) -> &mut ScrollMotion {
        self.motions
            .entry(id)
            .or_insert_with(|| ScrollMotion::owned(axes))
    }

    /// Smoothing alone, for what already paints its own bar.
    ///
    /// The SQL console's result table is such a case: it installs both its bars
    /// itself, and adding a third on top would make two in the same place.
    ///
    /// **And the notch is taken before the table, not after.** This is the one
    /// place where the inversion `ui::motion` rests on does not hold:
    /// gpui-component's table covers itself with a `ScrollableMask`, which
    /// handles the wheel in the **capture** phase and consumes it, so a
    /// listener on an ancestor — the bubble phase — was never called and the
    /// grid alone jumped three rows a notch. We therefore register our own
    /// capture listener **before** the table paints (capture runs in paint
    /// order, so the first child registered runs first), consume the event,
    /// and push the motion ourselves: same arithmetic, opposite starting point
    /// (`ScrollMotion::push`, the built-in editor's path).
    ///
    /// What we deliberately leave to the mask: a trackpad, already gradual and
    /// attached to the finger, and anything horizontal — the grid scrolls in
    /// width too, and its axis lock is worth more there than our smoothing. At
    /// the vertical edge the event is left to bubble as the mask would.
    ///
    /// The id stays the motion's key, as for `scrolled`: two panels cannot
    /// animate the same offset.
    pub(super) fn smoothed<H: Scrollable>(
        &mut self,
        id: impl Into<SharedString>,
        handle: &H,
        axes: Axes,
        window: &Window,
        content: impl IntoElement,
        cx: &Context<Self>,
    ) -> Stateful<gpui::Div> {
        let id: SharedString = id.into();
        let base = handle.base();
        self.motion(id.clone(), axes).advance(&base, window);
        let entity = cx.entity();
        div()
            .id(gpui::ElementId::Name(id.clone()))
            .relative()
            .size_full()
            .min_h_0()
            .min_w_0()
            .child(
                // A hitbox and not the bare bounds: a window listener sees no
                // hierarchy, so a rectangle alone cannot tell that something is
                // painted over this panel. A popover's `occlude()` cuts the hit
                // test short before a hitbox inserted here, which is what stops
                // a panel from taking the wheel of a list hanging above it.
                gpui::canvas(
                    |bounds, window, _cx| {
                        window.insert_hitbox(bounds, gpui::HitboxBehavior::Normal)
                    },
                    move |_, hitbox: gpui::Hitbox, window, _cx| {
                        window.on_mouse_event({
                            let id = id.clone();
                            let base = base.clone();
                            move |event: &gpui::ScrollWheelEvent, phase, window, cx| {
                                if phase != gpui::DispatchPhase::Capture
                                    || !hitbox.should_handle_scroll(window)
                                    || !takes_over(&base, event, window)
                                {
                                    return;
                                }
                                cx.stop_propagation();
                                entity.update(cx, |this, cx| {
                                    let delta = event.delta.pixel_delta(window.line_height());
                                    let next = this.motion(id.clone(), axes).push(
                                        base.offset(),
                                        delta,
                                        base.max_offset(),
                                    );
                                    base.set_offset(next);
                                    cx.notify();
                                });
                            }
                        });
                    },
                )
                .absolute()
                .inset_0(),
            )
            .child(content)
    }

    /// Scrolling content, its bar, and its wheel smoothing.
    ///
    /// The listener sits on the bar's container, which does not scroll itself:
    /// it therefore runs **after** the list's, in the bubble phase, which is
    /// exactly what `ScrollMotion::on_wheel` expects — it takes over a jump
    /// already applied.
    pub(super) fn scrolled<H: Scrollable>(
        &mut self,
        id: impl Into<SharedString>,
        handle: &H,
        axes: Axes,
        window: &Window,
        // The content **before** the context: it is built with a `&mut
        // Context`, and an argument already borrowing it in shared mode would
        // prevent that — arguments evaluate left to right.
        content: impl IntoElement,
        cx: &Context<Self>,
    ) -> Stateful<gpui::Div> {
        let id: SharedString = id.into();
        let base = handle.base();
        self.motion(id.clone(), axes).advance(&base, window);
        let axis = match axes {
            Axes::Vertical => ScrollbarAxis::Vertical,
            Axes::Both => ScrollbarAxis::Both,
            Axes::Horizontal => ScrollbarAxis::Horizontal,
        };
        wrap(id.clone(), handle, axis, content.into_any_element()).on_scroll_wheel(cx.listener(
            move |this, event: &gpui::ScrollWheelEvent, window, cx| {
                if this.motion(id.clone(), axes).on_wheel(&base, event, window) {
                    cx.notify();
                }
            },
        ))
    }
}
