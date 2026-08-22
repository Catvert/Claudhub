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
