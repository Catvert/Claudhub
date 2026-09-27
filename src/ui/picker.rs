//! What the two pickers of the top bar share.
//!
//! `branch_picker` and `worktree_picker` are the same surface twice over — a
//! filter field, a virtualised list of headings and entries, and a second step
//! that replaces the first. What is here is everything of it but the rows
//! themselves and the second step — the kept list and what makes it stale,
//! the field, the arrows, the frame of the list — and it is here because a
//! surface written twice drifts at the first correction.
//!
//! A picker is an entity holding a [`PickerCore`], and [`Pick`] is what it
//! says of itself: its rows, where the cursor may land, what Enter does.

use std::rc::Rc;

use gpui_kit::component::{
    input::{Input, InputEvent, InputState},
    popover::PopoverState,
    v_flex, v_virtual_list, Sizable as _, VirtualListScrollHandle,
};
use gpui_kit::{
    div, prelude::*, px, AnyElement, App, Context, Entity, Hsla, KeyDownEvent, ScrollStrategy,
    SharedString, WeakEntity, Window,
};

use crate::ui::app::ClaudhubApp;
use crate::ui::icons::icon;
use crate::ui::motion::{Axes, ScrollMotion};

/// Where the keyboard cursor lands, from where it stands.
///
/// **It wraps**, unlike the review's and the search's arrows: what one is
/// walking here is a handful of names in a popover, and an arrow that stops
/// answering at the last of them reads as broken. A result list is read from the
/// top down and has the opposite rule.
///
/// **A heading is counted and not landed on.** The cursor is an index into the
/// displayed list, headings included — anything else drifts the moment a heading
/// leaves with its group — but stopping on "Locales" would read as stuck, so
/// `landable` says which rows Enter could act on and the walk steps over the
/// rest. `None` when there is nowhere to go: an empty list, or one made of
/// headings alone.
///
/// `from` may sit past the end — a cursor left over from a longer list — and is
/// wrapped like any other position rather than refused.
pub(super) fn step_cursor(
    len: usize,
    landable: impl Fn(usize) -> bool,
    from: usize,
    delta: isize,
) -> Option<usize> {
    if len == 0 {
        return None;
    }
    let mut index = from as isize;
    for _ in 0..len {
        index = (index + delta).rem_euclid(len as isize);
        if landable(index as usize) {
            return Some(index as usize);
        }
    }
    None
}

/// How tall a popover's list grows before it scrolls.
const LIST_HEIGHT: gpui_kit::Pixels = px(320.);

/// Which of the two steps is on screen: the filtered list, or one entry's
/// actions. The second **replaces** the first inside the same surface, so
/// nothing is ever nested and nothing can be clipped.
pub(super) enum Step<A> {
    List,
    Actions(A),
}

/// What both pickers hold, alike.
pub(super) struct PickerCore<P: Pick> {
    pub app: WeakEntity<ClaudhubApp>,
    pub query: Entity<InputState>,
    pub step: Step<P::Actions>,
    pub scroll: VirtualListScrollHandle,
    /// Keyboard cursor into the **displayed** list, headings included: what
    /// the arrows move is a row on screen, and a cursor counted on anything
    /// else drifts the moment a heading leaves with its group.
    pub cursor: usize,
    /// The rows on screen, kept between frames — see [`Pick::rows`].
    rows: Rc<Vec<P::Row>>,
    /// The list has to be laid out again. Set by the three things that change
    /// it: the filter, a fold, and what the application holds of it.
    pub stale: bool,
    /// What the rows were read from in the application — see
    /// [`Pick::signature`]. `None` until the application first notifies.
    signature: Option<P::Sig>,
    /// The wheel's smoothing, this view's own.
    ///
    /// The panels keep theirs on the application, keyed by the bar's id,
    /// because a panel is not an entity of its own; a picker is one, and one
    /// list is one motion — there is nothing to key. Without it the popover
    /// was the one list in the window whose wheel jumped, which reads as a
    /// different application under the same title bar.
    pub motion: ScrollMotion,
    /// The popover carrying the picker, so that a gesture can close it. Handed
    /// over by the content closure — a popover's state lives in element
    /// state, and that is the only place it is reachable from.
    pub popover: Option<Entity<PopoverState>>,
}

impl<P: Pick> PickerCore<P> {
    /// The half of a picker's constructor the two share: the field, and what
    /// tells the kept list to let go.
    pub(super) fn new(
        owner: &Entity<ClaudhubApp>,
        query: Entity<InputState>,
        cx: &mut Context<P>,
    ) -> Self {
        // Typing filters: the list is laid out again, once, and a frame is
        // asked for.
        cx.subscribe(&query, |this: &mut P, _, _event: &InputEvent, cx| {
            this.core_mut().stale = true;
            cx.notify();
        })
        .detach();
        // The list is a projection of the application, and it moves under it
        // — a fetch, a worktree created, an agent that starts working. But the
        // application notifies on every hover and every frame of an animation,
        // and the branch list lives on in a tool window: only a change of what
        // the rows are read from lets the kept list go.
        cx.observe(owner, |this: &mut P, owner, cx| {
            let app = owner.read(cx);
            let signature = P::signature(app, cx);
            let core = this.core_mut();
            if core.signature.as_ref() != Some(&signature) {
                core.signature = Some(signature);
                core.stale = true;
            }
        })
        .detach();
        Self {
            app: owner.downgrade(),
            query,
            step: Step::List,
            scroll: VirtualListScrollHandle::new(),
            cursor: 0,
            rows: Rc::new(Vec::new()),
            stale: true,
            signature: None,
            motion: ScrollMotion::new(Axes::Vertical),
            popover: None,
        }
    }
}

/// What makes a picker of a list: its rows, where the cursor may land, and
/// what Enter does. The rest — the kept list, the field, the arrows, the
/// frame of the list — is [`PickerCore`] and the provided methods.
pub(super) trait Pick: Render + Sized + 'static {
    type Row: Clone + 'static;
    /// What the second step is about.
    type Actions;
    /// What the rows are read from in the application.
    type Sig: PartialEq + 'static;

    fn core(&self) -> &PickerCore<Self>;
    fn core_mut(&mut self) -> &mut PickerCore<Self>;

    /// What the rows are read from in the application, beyond the filter and
    /// the folds. Compared on every notification of the application: it has
    /// to be cheap, and it is only an **equality** that spares laying the
    /// list out again — anything it leaves out is a list that stops following.
    fn signature(app: &ClaudhubApp, cx: &App) -> Self::Sig;

    /// The rows, laid out from scratch.
    fn build_rows(&self, cx: &App) -> Vec<Self::Row>;

    /// Whether the cursor may stop on a row: a heading is counted, not landed
    /// on — see [`step_cursor`].
    fn landable(row: &Self::Row) -> bool;

    /// What Enter does on the row under the cursor.
    fn enter(&mut self, row: Self::Row, window: &mut Window, cx: &mut Context<Self>);

    /// Ends the gesture the picker was standing in: a popover is dismissed.
    fn close(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(popover) = self.core().popover.clone() {
            popover.update(cx, |state, cx| state.dismiss(window, cx));
        }
    }

    /// The rows on screen, headings included.
    ///
    /// **Kept between frames.** They were laid out again on every frame —
    /// every entry lowercased for the filter, every row's text cloned — for a
    /// list that only moves when the filter, a fold or the application's
    /// reading of it does. Those three are what set `stale`.
    fn rows(&mut self, cx: &App) -> Rc<Vec<Self::Row>> {
        if self.core().stale {
            let rows = Rc::new(self.build_rows(cx));
            let core = self.core_mut();
            core.rows = rows;
            core.stale = false;
        }
        self.core().rows.clone()
    }

    /// Puts the picker back where it opens: the whole list, nothing typed.
    ///
    /// A filter left over from last time is the one thing a picker must not
    /// reopen with — it reads as a list that has lost its entries.
    fn reset(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let core = self.core_mut();
        core.step = Step::List;
        core.cursor = 0;
        core.stale = true;
        core.query
            .clone()
            .update(cx, |input, cx| input.set_value("", window, cx));
        cx.notify();
    }

    /// Runs `f` on the application, then closes: every action here is the end
    /// of the gesture the picker was opened for.
    fn act(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
        f: impl FnOnce(&mut ClaudhubApp, &mut Window, &mut Context<ClaudhubApp>),
    ) {
        if let Some(app) = self.core().app.upgrade() {
            app.update(cx, |this, cx| f(this, window, cx));
        }
        self.close(window, cx);
    }

    /// Moves the keyboard cursor, stepping over what it cannot land on.
    fn step_cursor(&mut self, delta: isize, cx: &mut Context<Self>) {
        let rows = self.rows(cx);
        let Some(next) = step_cursor(
            rows.len(),
            |ix| Self::landable(&rows[ix]),
            self.core().cursor,
            delta,
        ) else {
            return;
        };
        let core = self.core_mut();
        core.cursor = next;
        core.scroll.scroll_to_item(next, ScrollStrategy::Top);
        cx.notify();
    }

    /// The arrows and `Enter`, taken **before** the field sees them.
    ///
    /// In capture phase and on an ancestor of the input: a single-line
    /// `InputState` binds Up and Down to the ends of its text, so left to
    /// bubble they would never reach the list. Escape is deliberately
    /// untouched — it belongs to the popover, which is what one expects it to
    /// close.
    fn on_key(&mut self, event: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        if !matches!(self.core().step, Step::List) {
            return;
        }
        match event.keystroke.key.as_str() {
            "down" => {
                cx.stop_propagation();
                self.step_cursor(1, cx);
            }
            "up" => {
                cx.stop_propagation();
                self.step_cursor(-1, cx);
            }
            "enter" => {
                cx.stop_propagation();
                let rows = self.rows(cx);
                if let Some(row) = rows.get(self.core().cursor).cloned() {
                    self.enter(row, window, cx);
                }
            }
            _ => {}
        }
    }
}

/// The list step's frame, the same in both pickers.
pub(super) struct ListFrame {
    /// Names the scrolled area `<id>-list` and the list `<id>-rows`.
    pub id: &'static str,
    /// Each row's height: a heading is not as tall as an entry, so the list
    /// is a `v_virtual_list` and not a `uniform_list` — the same swap the
    /// diff's wrapping and the merge view make, and for the same reason.
    pub sizes: Vec<gpui_kit::Size<gpui_kit::Pixels>>,
    /// What an empty list says.
    pub empty: SharedString,
    pub muted: Hsla,
    /// A popover is as tall as what it holds, up to [`LIST_HEIGHT`]; a zone
    /// is as tall as it was dragged, and the list is what takes what is left
    /// over once the field and the footer have had theirs.
    pub docked: bool,
    pub footer: AnyElement,
}

/// The filter field, the list — or what an empty one says — and the footer.
/// `build` paints the row at an index; it runs for every visible row on every
/// frame.
pub(super) fn render_list<P: Pick>(
    this: &mut P,
    frame: ListFrame,
    build: impl Fn(usize, &mut App) -> AnyElement + 'static,
    window: &Window,
    cx: &mut Context<P>,
) -> AnyElement {
    // The transition, one step per frame. It asks for the next frame itself
    // for as long as it is moving.
    let core = this.core_mut();
    let base = crate::ui::scroll::Scrollable::base(&core.scroll);
    core.motion.advance(&base, window);
    let docked = frame.docked;
    let count = frame.sizes.len();
    let core = this.core();
    v_flex()
        .w_full()
        .min_h_0()
        .when(docked, |el| el.flex_1())
        .child(
            div().w_full().px_1().py_1().child(
                Input::new(&core.query)
                    .xsmall()
                    // What the field does is filter, and a bare box above a
                    // list reads as somewhere to type a name. The glyph is the
                    // one the panels' own `Ctrl+F` wears.
                    .prefix(icon("search").xsmall().text_color(frame.muted)),
            ),
        )
        .child(if count == 0 {
            div()
                .w_full()
                .p_3()
                .text_sm()
                .text_color(frame.muted)
                .when(docked, |el| el.flex_1())
                .child(frame.empty)
                .into_any_element()
        } else {
            crate::ui::scroll::smooth_wheel(
                crate::ui::scroll::vertical(
                    format!("{}-list", frame.id),
                    &core.scroll,
                    v_virtual_list(
                        cx.entity(),
                        SharedString::from(format!("{}-rows", frame.id)),
                        Rc::new(frame.sizes),
                        move |_, range, _window, cx| {
                            range.map(|ix| build(ix, cx)).collect::<Vec<_>>()
                        },
                    )
                    .size_full()
                    .track_scroll(&core.scroll),
                ),
                base,
                |this: &mut P| &mut this.core_mut().motion,
                cx,
            )
            .when(docked, |el| el.flex_1().min_h_0())
            .when(!docked, |el| el.h(LIST_HEIGHT))
            .into_any_element()
        })
        .child(frame.footer)
        .into_any_element()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A heading is a row on screen and therefore counted, but it is not
    /// somewhere one can land.
    #[test]
    fn the_cursor_steps_over_what_it_cannot_land_on() {
        // 0 heading, 1 entry, 2 entry, 3 heading, 4 entry
        let landable = |ix: usize| ix != 0 && ix != 3;
        assert_eq!(step_cursor(5, landable, 1, 1), Some(2));
        assert_eq!(step_cursor(5, landable, 2, 1), Some(4));
        assert_eq!(step_cursor(5, landable, 4, -1), Some(2));
        assert_eq!(step_cursor(5, landable, 1, -1), Some(4));
    }

    /// It wraps: what one walks here is a handful of names, and an arrow that
    /// stops answering at the last of them reads as broken.
    #[test]
    fn the_cursor_wraps_at_both_ends() {
        assert_eq!(step_cursor(3, |_| true, 2, 1), Some(0));
        assert_eq!(step_cursor(3, |_| true, 0, -1), Some(2));
    }

    /// Nowhere to go: an empty list, or one made of headings alone.
    #[test]
    fn a_list_with_nowhere_to_land_moves_nothing() {
        assert_eq!(step_cursor(0, |_| true, 0, 1), None);
        assert_eq!(step_cursor(4, |_| false, 0, 1), None);
    }

    /// A cursor left over from a longer list is wrapped, not refused: the list
    /// is rebuilt by a filter, and the arrow that follows must still move.
    #[test]
    fn a_cursor_past_the_end_still_moves() {
        assert_eq!(step_cursor(3, |_| true, 9, 1), Some(1));
    }
}
