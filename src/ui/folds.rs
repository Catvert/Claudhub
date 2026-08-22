//! Which folds a fold level closes.
//!
//! `zM` and `zR` are plain — everything, nothing — but `zm` and `zr` move by
//! **one level**, and a level is a depth of nesting: a method inside a class is
//! one deeper than the class. The editor's fold candidates come from the
//! grammar as flat line ranges, so the depth has to be read back out of them,
//! and that reading is the only part of this that can be got wrong quietly —
//! hence a module of its own, with no gpui in it.

/// A fold candidate, as the editor names it: the line it starts on and the line
/// it ends on, both counted in the buffer.
pub type Range = (usize, usize);

/// How many candidates strictly contain this one.
///
/// Strictly: a range does not contain itself, and two ranges that start on the
/// same line are not nested — the grammar gives one fold per line at most, so
/// that case does not arise, and treating it as nesting would count a fold as
/// its own parent.
fn depth(ranges: &[Range], of: Range) -> usize {
    ranges
        .iter()
        .filter(|(start, end)| *start < of.0 && *end >= of.1)
        .count()
}

/// The deepest nesting the document has, or `None` when nothing folds.
///
/// It is the ceiling `zr` walks back up to: one more than this and every fold
/// is open, which is what `zR` does in one step.
pub fn max_depth(ranges: &[Range]) -> Option<usize> {
    ranges.iter().map(|r| depth(ranges, *r)).max()
}

/// The folds to close at `level`: everything nested that deep or deeper.
///
/// Level 0 closes the outermost folds and therefore hides everything inside
/// them, which is `zM`. A level past the deepest nesting closes nothing, which
/// is `zR`.
pub fn at_level(ranges: &[Range], level: usize) -> Vec<usize> {
    ranges
        .iter()
        .filter(|r| depth(ranges, **r) >= level)
        .map(|(start, _)| *start)
        .collect()
}

/// The first line at or after (or at or before, going up) `row` that a closed
/// fold does not hide.
///
/// A closed fold hides what is **between** its two lines: the line it starts on
/// carries the marker and the one it ends on closes it, and both stay on
/// screen. Folds nest, so the answer is looked for again from where the last
/// one left off — stepping out of an inner fold can land in an outer one.
fn shown(closed: &[Range], row: usize, down: bool) -> usize {
    let mut row = row;
    // Each pass leaves the fold it found, so the row only ever moves away from
    // where it started; the bound is a belt on top of that.
    for _ in 0..=closed.len() {
        let Some((start, end)) = closed
            .iter()
            .find(|(start, end)| *start < row && row < *end)
            .copied()
        else {
            break;
        };
        row = if down { end } else { start };
    }
    row
}

/// Where `j` and `k` land, `delta` lines away, with what the folds hide skipped.
///
/// This is the whole of what a fold changes for a motion, and getting it wrong
/// is silent: the caret walks into lines nobody can see, taking the block cursor
/// with it. `last` is the last line of the buffer, which is where going down
/// stops.
pub fn step(closed: &[Range], row: usize, delta: isize, last: usize) -> usize {
    let down = delta > 0;
    let mut row = row.min(last);
    for _ in 0..delta.unsigned_abs() {
        let next = if down {
            if row >= last {
                break;
            }
            row + 1
        } else {
            if row == 0 {
                break;
            }
            row - 1
        };
        row = shown(closed, next, down).min(last);
    }
    row
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A class holding two methods, one of which holds a loop.
    fn php() -> Vec<Range> {
        vec![(0, 20), (2, 8), (4, 6), (10, 18)]
    }

    #[test]
    fn nesting_is_read_back_out_of_the_ranges() {
        let ranges = php();
        assert_eq!(depth(&ranges, (0, 20)), 0, "the class");
        assert_eq!(depth(&ranges, (2, 8)), 1, "a method");
        assert_eq!(depth(&ranges, (4, 6)), 2, "the loop inside it");
        assert_eq!(max_depth(&ranges), Some(2));
        assert_eq!(max_depth(&[]), None);
    }

    #[test]
    fn a_level_closes_everything_that_deep_and_deeper() {
        let ranges = php();
        assert_eq!(at_level(&ranges, 0), vec![0, 2, 4, 10], "zM closes all");
        assert_eq!(at_level(&ranges, 1), vec![2, 4, 10], "the class stays open");
        assert_eq!(at_level(&ranges, 2), vec![4]);
        assert!(at_level(&ranges, 3).is_empty(), "past the deepest: zR");
    }

    /// A markdown title folded shut: the lines under it are gone, and `j` has to
    /// step over them rather than into them.
    #[test]
    fn a_step_lands_on_a_line_that_is_shown() {
        let closed = vec![(2usize, 8usize)];
        assert_eq!(step(&closed, 1, 1, 20), 2, "onto the fold's own line");
        assert_eq!(step(&closed, 2, 1, 20), 8, "over what it hides");
        assert_eq!(step(&closed, 8, -1, 20), 2, "and back over it");
        assert_eq!(step(&closed, 2, -1, 20), 1);
        assert_eq!(
            step(&closed, 1, 3, 20),
            9,
            "a fold costs one step, not its height"
        );
    }

    /// Leaving an inner fold can land inside the one holding it.
    #[test]
    fn folds_that_hold_one_another_are_left_in_one_go() {
        let closed = vec![(0usize, 20usize), (2, 8)];
        assert_eq!(step(&closed, 0, 1, 30), 20);
        assert_eq!(step(&closed, 20, -1, 30), 0);
    }

    #[test]
    fn a_step_stops_at_either_end_of_the_buffer() {
        assert_eq!(step(&[], 0, -1, 9), 0);
        assert_eq!(step(&[], 9, 1, 9), 9);
        assert_eq!(step(&[(5, 9)], 5, 1, 9), 9, "the closing line still shows");
    }

    /// Two ranges sharing a start line are not each other's parent: counting
    /// them as nested would make a fold its own ancestor and shift every level
    /// by one.
    #[test]
    fn a_fold_is_not_its_own_parent() {
        let ranges = vec![(3, 9), (3, 5)];
        assert_eq!(depth(&ranges, (3, 9)), 0);
        assert_eq!(depth(&ranges, (3, 5)), 0);
    }
}
