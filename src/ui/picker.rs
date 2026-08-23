//! What the two pickers of the top bar share.
//!
//! `branch_picker` and `worktree_picker` are the same surface twice over — a
//! filter field, a virtualised list of headings and entries, and a second step
//! that replaces the first. What is here is the part of it that decides
//! something rather than paints it, and it is here because a decision made
//! twice drifts at the first correction.

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
