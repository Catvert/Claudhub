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
    /// itself, and adding a third on top would make two in the same place. The
    /// motion has nothing to do with the bar — it is the wheel listener of a
    /// non-scrolling ancestor, and that is all we need here.
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
        div()
            .id(gpui::ElementId::Name(id.clone()))
            .size_full()
            .min_h_0()
            .min_w_0()
            .child(content)
            .on_scroll_wheel(cx.listener(
                move |this, event: &gpui::ScrollWheelEvent, window, cx| {
                    if this.motion(id.clone(), axes).on_wheel(&base, event, window) {
                        cx.notify();
                    }
                },
            ))
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
