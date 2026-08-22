//! Wheel smoothing.
//!
//! A wheel notch is a **jump**: gpui turns a `ScrollDelta::Lines` into three
//! line heights and adds them to the offset in one go. On a list of text — a
//! diff, a tree of forty thousand files — the eye loses its place at every
//! notch, and it takes twenty of them to cross a hunk. This module replays
//! that jump as a hundred-and-sixty-millisecond transition, eased at the end.
//!
//! The idea comes from Aviary (`src/ui/motion.rs`), and it rests on one
//! inversion: **we do not stop gpui from jumping**, there is no capture phase
//! for the wheel. We let it happen, read where it landed, **put back** the
//! previous offset, and go there gradually. Hence the listener's position: on
//! a **non-scrolling** ancestor of the scrolling element, so after its internal
//! handler in the bubble phase.
//!
//! Four things not to miss:
//!
//! - **The jump is read, not recomputed.** gpui captures
//!   `window.line_height()` under the text style of the scrolling element; our
//!   listener, sitting on an ancestor, reads the ambient height. On the diff,
//!   which has neither the interface's font nor its size, the two are not the
//!   same — hence `Axis::jump`, which takes the difference with the offset
//!   written on the previous frame.
//! - **A trackpad is not a wheel.** It sends `ScrollDelta::Pixels`, already
//!   continuous and attached to the finger: smoothing them would add lag to a
//!   direct gesture. They pass through unchanged, and cancel the running
//!   transition.
//! - **A jump asked for by code wins.** `scroll_to_item` — an arrow changing
//!   hunk, `reveal_file` — writes the offset without telling us. `advance`
//!   therefore compares what it finds with what it had written: a discrepancy
//!   means somebody else has been through, and the transition is abandoned
//!   rather than pulling the view backwards.
//! - **The list changes size during the motion.** A diff arriving, a folder
//!   unfolded: the destination is re-clamped on every frame against the
//!   current bounds, starting from the visible position, so the re-framing
//!   stays continuous.
//!
//! The two axes are independent: the diff also scrolls in width, and a vertical
//! transition must not freeze a horizontal offset.

use gpui::{point, px, Pixels, Point, ScrollDelta, ScrollHandle, ScrollWheelEvent, Window};
use std::time::{Duration, Instant};

/// Below this, two positions are the same. Used to avoid restarting a
/// transition towards the destination already aimed at.
const EPSILON: f32 = 0.0001;
/// The gap past which we consider somebody else has written the offset. Half a
/// pixel more than gpui's rounding.
const SYNC_EPSILON_PX: f32 = 0.75;
/// Below this, we settle rather than animate a final half pixel.
const SNAP_PX: f32 = 0.5;
/// Short enough for the list to answer the notch, long enough for the eye to
/// follow the text instead of hunting for it.
const DURATION: Duration = Duration::from_millis(160);

/// What scrolls in a panel: one axis, or both.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Axes {
    /// The common case. A horizontal wheel is treated as vertical here,
    /// exactly as gpui does when only one axis overflows.
    Vertical,
    /// The diff, whose lines never wrap.
    Both,
}

/// The smoothing of one scroll handle.
///
/// One per panel, and filed beside the handle it animates: advancing one
/// panel's motion on another's offset would make it jump from one end to the
/// other.
pub struct ScrollMotion {
    x: Axis,
    y: Axis,
    axes: Axes,
    /// Does an offset written by somebody else win?
    ///
    /// True everywhere the view is scrolled by gpui and we only take the jump
    /// over: an offset that has moved on its own means a `scroll_to_item`, and
    /// pulling the view back would undo it. False for the built-in editor,
    /// where we own the movement from end to end and where reading back is
    /// **late** — `InputState::set_scroll_offset` is applied at the next
    /// layout, so the value read on the next frame is still the previous one,
    /// and re-syncing on it dropped every transition on its first frame.
    resync: bool,
}

impl ScrollMotion {
    pub fn new(axes: Axes) -> Self {
        Self {
            x: Axis::default(),
            y: Axis::default(),
            axes,
            resync: true,
        }
    }

    /// A motion for a surface we drive ourselves. See `resync`.
    pub fn owned(axes: Axes) -> Self {
        Self {
            resync: false,
            ..Self::new(axes)
        }
    }

    /// Aims `delta` further than where the view is, for a surface whose wheel
    /// we have taken **before** it scrolled rather than after.
    ///
    /// `on_wheel` takes over a jump already applied; here nothing has moved, so
    /// we hand it the position the jump would have reached and the jump itself.
    /// Same arithmetic, opposite starting point.
    pub fn push(
        &mut self,
        from: Point<Pixels>,
        delta: Point<Pixels>,
        max: Point<Pixels>,
    ) -> Point<Pixels> {
        let now = Instant::now();
        let (dx, dy) = self.split(delta);
        let x = if dx == 0. {
            f32::from(from.x)
        } else {
            self.x
                .on_wheel(f32::from(from.x) + dx, dx, f32::from(max.x), now)
        };
        let y = if dy == 0. {
            f32::from(from.y)
        } else {
            self.y
                .on_wheel(f32::from(from.y) + dy, dy, f32::from(max.y), now)
        };
        point(px(x), px(y))
    }

    /// Advances the transition by one frame and writes the resulting offset.
    ///
    /// To be called once per panel render, before or after building its content
    /// — the element only reads the offset at layout time.
    pub fn advance(&mut self, handle: &ScrollHandle, window: &Window) {
        if let Some(offset) = self.advance_at(handle.offset(), handle.max_offset(), window) {
            handle.set_offset(offset);
        }
    }

    /// The same thing over an offset given by hand, for a surface that is not a
    /// `ScrollHandle`.
    ///
    /// The built-in editor is the only one: its offset is reachable through
    /// `InputState` alone, which reads and writes it but is not a handle.
    /// `None` when there is nothing to write — the transition is over, or
    /// somebody else has moved the view.
    pub fn advance_at(
        &mut self,
        actual: Point<Pixels>,
        max: Point<Pixels>,
        window: &Window,
    ) -> Option<Point<Pixels>> {
        let now = Instant::now();
        let resync = self.resync;
        let (x, moving_x) = self
            .x
            .advance(f32::from(actual.x), f32::from(max.x), now, resync);
        let (y, moving_y) = self
            .y
            .advance(f32::from(actual.y), f32::from(max.y), now, resync);
        if moving_x || moving_y {
            window.request_animation_frame();
        }
        (x != f32::from(actual.x) || y != f32::from(actual.y)).then(|| point(px(x), px(y)))
    }

    /// Takes over the jump gpui has just applied and replays it as a transition.
    ///
    /// Returns true when the view has to be notified to paint the first frame;
    /// `advance` asks for the following ones.
    pub fn on_wheel(
        &mut self,
        handle: &ScrollHandle,
        event: &ScrollWheelEvent,
        window: &Window,
    ) -> bool {
        match self.wheel_at(handle.offset(), handle.max_offset(), event, window) {
            Some(offset) => {
                handle.set_offset(offset);
                true
            }
            None => false,
        }
    }

    /// The same thing over an offset given by hand. See `advance_at`.
    pub fn wheel_at(
        &mut self,
        jumped: Point<Pixels>,
        max: Point<Pixels>,
        event: &ScrollWheelEvent,
        window: &Window,
    ) -> Option<Point<Pixels>> {
        let delta = match event.delta {
            // The finger is already gradual: smoothing it would add lag.
            ScrollDelta::Pixels(_) => {
                self.cancel();
                return None;
            }
            ScrollDelta::Lines(_) => event.delta.pixel_delta(window.line_height()),
        };
        let (dx, dy) = self.split(delta);
        if dx == 0. && dy == 0. {
            return None;
        }

        let now = Instant::now();
        // The axis this notch does not touch keeps the position `advance` wrote
        // for it: reading it back here is handing it back unchanged.
        let x = if dx == 0. {
            f32::from(jumped.x)
        } else {
            self.x
                .on_wheel(f32::from(jumped.x), dx, f32::from(max.x), now)
        };
        let y = if dy == 0. {
            f32::from(jumped.y)
        } else {
            self.y
                .on_wheel(f32::from(jumped.y), dy, f32::from(max.y), now)
        };
        Some(point(px(x), px(y)))
    }

    /// Drops the running transition, ahead of a direct gesture.
    pub fn cancel(&mut self) {
        self.x.cancel();
        self.y.cancel();
    }

    /// Splits a delta across the axes **the way gpui does**, otherwise the
    /// smoothing would move the view differently from the jump it replaces.
    ///
    /// On a single-axis panel, a horizontal wheel falls back to vertical; on a
    /// two-axis panel, only the dominant component gets through
    /// (`allow_concurrent_scroll` is false by default).
    fn split(&self, delta: Point<Pixels>) -> (f32, f32) {
        match self.axes {
            Axes::Vertical => {
                let dy = if delta.y == px(0.) { delta.x } else { delta.y };
                (0., f32::from(dy))
            }
            Axes::Both if delta.x.abs() > delta.y.abs() => (f32::from(delta.x), 0.),
            Axes::Both => (0., f32::from(delta.y)),
        }
    }
}

/// One axis, with nothing of gpui inside: that is what makes it testable.
#[derive(Default)]
struct Axis {
    motion: Option<Motion>,
    /// The last offset written. It serves to recognise a jump from elsewhere,
    /// and to recover the position from before gpui's even when that one was
    /// clamped against an edge.
    last: Option<f32>,
}

impl Axis {
    /// Returns the offset to write, and whether one more frame is needed.
    fn advance(&mut self, actual: f32, max: f32, now: Instant, resync: bool) -> (f32, bool) {
        let Some(mut motion) = self.motion.take() else {
            // Idle, the offset is read back whatever the mode: between two
            // transitions it is the caret, a click or a search that moves the
            // view, and the next notch has to start from there.
            self.last = Some(actual);
            return (actual, false);
        };
        // Somebody else has written the offset: they win.
        if resync
            && self
                .last
                .is_some_and(|expected| (actual - expected).abs() > SYNC_EPSILON_PX)
        {
            self.last = Some(actual);
            return (actual, false);
        }

        let mut sample = motion.sample_at(now);
        let target = motion.target.clamp(-max, 0.);
        // The list has changed size: we start again from the visible position
        // towards the re-clamped destination, so the motion stays continuous.
        if (target - motion.target).abs() > EPSILON {
            motion = Motion::between(sample.value.clamp(-max, 0.), target, now);
            sample = motion.sample_at(now);
        }

        if !sample.running || (target - sample.value).abs() <= SNAP_PX {
            self.last = Some(target);
            return (target, false);
        }
        let current = sample.value.clamp(-max, 0.);
        self.last = Some(current);
        self.motion = Some(motion);
        (current, true)
    }

    /// Returns the offset from before the jump, and aims at the one after.
    fn on_wheel(&mut self, jumped: f32, delta: f32, max: f32, now: Instant) -> f32 {
        let clamp = |value: f32| value.clamp(-max, 0.);
        let jump = self.jump(jumped, delta);
        let (current, target) = match self.motion.take() {
            // A notch during a transition extends the destination.
            Some(motion) => (motion.sample_at(now).value, motion.target + jump),
            // The position before is the one we had written, and the jump the
            // one gpui has just added to it.
            None => (jumped - jump, jumped),
        };
        let current = clamp(current);
        let target = clamp(target);
        if (target - current).abs() <= SNAP_PX {
            self.last = Some(target);
            return target;
        }
        self.last = Some(current);
        self.motion = Some(Motion::between(current, target, now));
        current
    }

    /// The jump gpui has just applied, **read** rather than recomputed.
    ///
    /// `delta` is what this notch would be worth to us, and it is not what it
    /// was worth to gpui: gpui captures `window.line_height()` under the **text
    /// style of the scrolling element**, whereas our listener is on an ancestor
    /// and reads the ambient line height. The diff having neither the
    /// interface's font nor its size, three lines apart make two or three
    /// pixels.
    ///
    /// That gap is invisible in the middle of a sixty-pixel jump — it only
    /// shifts where the transition starts. **At an edge it is the whole
    /// movement**: the destination there is clamped to the position already
    /// held, the origin is not, and the view stepped back three pixels only to
    /// return over a hundred and sixty milliseconds, on every notch.
    ///
    /// The difference with the offset written on the previous frame, on the
    /// other hand, is exact. We only trust it if it looks like the expected
    /// jump: an offset written by somebody else since our last render would
    /// make that note false, and the view would teleport to it.
    fn jump(&self, jumped: f32, delta: f32) -> f32 {
        let Some(last) = self.last else { return delta };
        let observed = jumped - last;
        let plausible = observed * delta > 0. && observed.abs() <= delta.abs() * 2.;
        if plausible {
            observed
        } else {
            delta
        }
    }

    fn cancel(&mut self) {
        self.motion = None;
        self.last = None;
    }
}

struct Motion {
    from: f32,
    target: f32,
    started_at: Instant,
}

struct Sample {
    value: f32,
    running: bool,
}

impl Motion {
    fn between(from: f32, target: f32, now: Instant) -> Self {
        Self {
            from,
            target,
            started_at: now,
        }
    }

    fn sample_at(&self, now: Instant) -> Sample {
        let elapsed = now.saturating_duration_since(self.started_at);
        if elapsed >= DURATION {
            return Sample {
                value: self.target,
                running: false,
            };
        }
        let progress = elapsed.as_secs_f32() / DURATION.as_secs_f32();
        Sample {
            value: self.from + (self.target - self.from) * ease_out_cubic(progress),
            running: true,
        }
    }
}

/// Immediate start, gentle arrival. It is the curve that makes the list feel
/// like it answers the notch rather than a timer.
fn ease_out_cubic(progress: f32) -> f32 {
    1. - (1. - progress.clamp(0., 1.)).powi(3)
}

#[cfg(test)]
mod tests {
    use super::*;

    const MAX: f32 = 1000.;

    /// The heart of the trick: gpui's jump is given back, and the position
    /// aimed at is where it had landed.
    #[test]
    fn a_notch_gives_back_the_jump_and_aims_at_it() {
        let now = Instant::now();
        let mut axis = Axis::default();
        // gpui went from 0 to -60 before we were called.
        let written = axis.on_wheel(-60., -60., MAX, now);
        assert_eq!(written, 0., "the view stays where the eye left it");

        let (middle, moving) = axis.advance(0., MAX, now + Duration::from_millis(80), true);
        assert!(moving, "the transition asks for one more frame");
        assert!(middle < 0. && middle > -60.);

        let (end, moving) = axis.advance(middle, MAX, now + DURATION, true);
        assert_eq!(end, -60.);
        assert!(!moving);
    }

    #[test]
    fn a_second_notch_extends_the_destination() {
        let now = Instant::now();
        let mut axis = Axis::default();
        axis.on_wheel(-60., -60., MAX, now);
        let middle = now + Duration::from_millis(80);
        let (position, _) = axis.advance(0., MAX, middle, true);
        // gpui jumps again, from the position we have just written.
        axis.on_wheel(position - 60., -60., MAX, middle);
        let (end, _) = axis.advance(position, MAX, middle + DURATION, true);
        assert_eq!(end, -120., "the two notches add up");
    }

    /// The built-in editor writes its offset **late** — `set_scroll_offset`
    /// lands at the next layout — so the value read back a frame later is still
    /// the previous one. Taken for somebody else's writing, it killed every
    /// transition on its first frame, and the editor did not move at all.
    #[test]
    fn a_surface_we_drive_ourselves_ignores_an_offset_read_back_late() {
        let now = Instant::now();
        let mut axis = Axis::default();
        axis.on_wheel(-60., -60., MAX, now);
        // Our write has not landed yet: the surface still says zero.
        let (position, moving) = axis.advance(0., MAX, now + Duration::from_millis(80), false);
        assert!(moving, "the transition carries on regardless");
        assert!(position < 0. && position > -60.);
        // The same reading, with the re-sync on, gives the transition up.
        let mut other = Axis::default();
        other.on_wheel(-60., -60., MAX, now);
        other.advance(0., MAX, now + Duration::from_millis(40), true);
        let (_, moving) = other.advance(0., MAX, now + Duration::from_millis(80), true);
        assert!(!moving, "here the offset read back is taken for a jump");
    }

    /// A surface whose wheel we take **before** it scrolls: nothing has moved,
    /// so the destination is what the notch asks for from where we are.
    #[test]
    fn a_wheel_taken_before_the_jump_aims_from_where_the_view_is() {
        let mut motion = ScrollMotion::owned(Axes::Vertical);
        let from = point(px(0.), px(-100.));
        let written = motion.push(from, point(px(0.), px(-60.)), point(px(0.), px(MAX)));
        assert_eq!(written, from, "the view does not move on the first frame");
        let (end, _) = motion
            .y
            .advance(-100., MAX, Instant::now() + DURATION, false);
        assert_eq!(end, -160., "and lands one notch further");
    }

    /// `scroll_to_item` writes the offset without telling us; pulling it back
    /// to the transition's position would undo the arrow key just pressed.
    #[test]
    fn a_programmatic_jump_wins_over_a_running_transition() {
        let now = Instant::now();
        let mut axis = Axis::default();
        axis.on_wheel(-60., -60., MAX, now);
        let (position, moving) = axis.advance(-900., MAX, now + Duration::from_millis(40), true);
        assert_eq!(position, -900., "the requested position is kept");
        assert!(!moving);
        assert!(axis.motion.is_none());
    }

    /// Near an edge, gpui clamps nothing: it adds its jump to the offset and
    /// layout is what will bring it back inside the bounds. The destination is
    /// therefore clamped, the origin is not.
    #[test]
    fn a_jump_past_the_edge_stops_at_it_without_starting_from_it() {
        let now = Instant::now();
        // We are ten pixels from the bottom, one notch asks for sixty.
        let mut axis = Axis {
            last: Some(-990.),
            ..Default::default()
        };
        let written = axis.on_wheel(-1050., -60., MAX, now);
        assert_eq!(written, -990.);
        let (end, _) = axis.advance(written, MAX, now + DURATION, true);
        assert_eq!(end, -MAX, "the destination is clamped, not the origin");
    }

    /// The edge, and the notch too many.
    ///
    /// The line height we read is not the one gpui used: three lines apart make
    /// two or three pixels. They are invisible in the middle of a sixty-pixel
    /// jump, but here they *are* the movement — the view stepped back that much
    /// before returning, on every notch.
    #[test]
    fn a_notch_against_the_edge_leaves_the_view_where_it_is() {
        let now = Instant::now();
        let mut axis = Axis {
            last: Some(-MAX),
            ..Default::default()
        };
        // gpui jumped sixty pixels; our line height counts sixty-three.
        let written = axis.on_wheel(-MAX - 60., -63., MAX, now);
        assert_eq!(written, -MAX, "the view does not move by a pixel");
        assert!(axis.motion.is_none(), "there is nothing to animate");
    }

    /// The trackpad is already continuous, and a single-axis panel receives a
    /// horizontal wheel as a vertical one.
    #[test]
    fn a_wheel_is_split_the_way_gpui_splits_it() {
        let vertical = ScrollMotion::new(Axes::Vertical);
        assert_eq!(vertical.split(point(px(-30.), px(0.))), (0., -30.));
        assert_eq!(vertical.split(point(px(-30.), px(-60.))), (0., -60.));

        let both = ScrollMotion::new(Axes::Both);
        assert_eq!(both.split(point(px(-30.), px(0.))), (-30., 0.));
        assert_eq!(
            both.split(point(px(-30.), px(-60.))),
            (0., -60.),
            "only the dominant component gets through"
        );
    }
}
