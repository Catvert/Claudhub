//! How often the window renders, and what the root's render costs — pure,
//! tested.
//!
//! gpui redraws the whole window whenever an entity read during the last
//! draw notifies, and re-runs the render of every view that is not cached:
//! a slow screen is, most of the time, one rendered too often rather than
//! once too slowly. What this counts is the first thing to ask of it, and
//! the journal is where it is read — at `info` when the window renders at
//! a pace no still screen needs, at `debug` otherwise.

use std::time::{Duration, Instant};

/// The span a report covers.
const SPAN: Duration = Duration::from_secs(10);

/// Renders a second from which the report is worth reading unasked.
const BUSY_RATE: f32 = 20.;

/// The renders counted since `since`.
#[derive(Debug)]
pub(crate) struct FrameRate {
    since: Instant,
    frames: u32,
    spent: Duration,
    slowest: Duration,
}

/// What a span said.
#[derive(Debug, PartialEq)]
pub(crate) struct Report {
    pub frames: u32,
    pub span: Duration,
    pub spent: Duration,
    pub slowest: Duration,
}

impl FrameRate {
    pub(crate) fn new(now: Instant) -> Self {
        Self {
            since: now,
            frames: 0,
            spent: Duration::ZERO,
            slowest: Duration::ZERO,
        }
    }

    /// Counts a render that took `spent`, and hands back the span it closes.
    pub(crate) fn record(&mut self, now: Instant, spent: Duration) -> Option<Report> {
        self.frames += 1;
        self.spent += spent;
        self.slowest = self.slowest.max(spent);
        let span = now.saturating_duration_since(self.since);
        if span < SPAN {
            return None;
        }
        let report = Report {
            frames: self.frames,
            span,
            spent: self.spent,
            slowest: self.slowest,
        };
        *self = Self::new(now);
        Some(report)
    }
}

impl Report {
    pub(crate) fn per_second(&self) -> f32 {
        self.frames as f32 / self.span.as_secs_f32().max(f32::EPSILON)
    }

    /// The average render, in milliseconds.
    pub(crate) fn average_ms(&self) -> f32 {
        self.spent.as_secs_f32() * 1000. / self.frames.max(1) as f32
    }

    pub(crate) fn busy(&self) -> bool {
        self.per_second() >= BUSY_RATE
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_span_reports_once_it_is_over_and_starts_again() {
        let start = Instant::now();
        let mut rate = FrameRate::new(start);
        let ms = Duration::from_millis;
        assert_eq!(rate.record(start + ms(100), ms(4)), None);
        assert_eq!(rate.record(start + ms(5_000), ms(8)), None);
        let report = rate.record(start + ms(10_000), ms(6)).expect("a report");
        assert_eq!(report.frames, 3);
        assert_eq!(report.spent, ms(18));
        assert_eq!(report.slowest, ms(8));
        assert!((report.average_ms() - 6.).abs() < 1e-3);
        assert!((report.per_second() - 0.3).abs() < 1e-3);
        assert!(!report.busy());
        // The next span counts from the report on.
        assert_eq!(rate.record(start + ms(19_000), ms(1)), None);
        let next = rate.record(start + ms(20_000), ms(1)).expect("a report");
        assert_eq!(next.frames, 2);
    }

    #[test]
    fn a_window_drawn_like_an_animation_is_busy() {
        let start = Instant::now();
        let mut rate = FrameRate::new(start);
        let mut report = None;
        for frame in 1..=600 {
            report = rate.record(
                start + Duration::from_micros(frame * 16_667),
                Duration::from_millis(3),
            );
        }
        let report = report.expect("ten seconds at sixty a second");
        assert!(report.busy());
        assert!((report.per_second() - 60.).abs() < 0.5);
    }
}
