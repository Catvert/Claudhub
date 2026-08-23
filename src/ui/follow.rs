//! The words one can follow: `Ctrl`+click on a symbol, wherever code is read.
//!
//! Three surfaces paint code without being an editor — a diff line, a line of
//! the search results, a line of the search preview — and the same gesture must
//! work in all three. What they share is here: where the pointer is
//! (`Spot`), what a line needs to make its words clickable (`Armed`), and the
//! element that carries the click (`followable`).
//!
//! **The modifier is held, or nothing of this exists.** `ClaudhubApp::
//! follow_armed` is read once per frame; while it is false no word range is
//! computed anywhere and the lines are the plain elements they have always
//! been. See `ClaudhubApp::follow_armed` for why the flag is kept rather than
//! read at each render.

use std::ops::Range;

use gpui::{prelude::*, Context, Entity, SharedString, StyledText};

use crate::ui::app::ClaudhubApp;

/// Which line a followable word belongs to.
///
/// The row index alone would not do: a diff entry has two texts in two columns
/// and a text per visible line when it wraps, and two surfaces number their
/// rows from zero. It is what says whether the word the pointer is over is
/// *this* element's — and the underline follows from that.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Spot {
    /// A diff line: the entry, the column (0 old, 1 new) and the wrapped
    /// segment.
    Diff {
        row: usize,
        side: u8,
        segment: usize,
    },
    /// A line of the search result list.
    SearchHit { row: usize },
    /// A line of the file shown beside the results.
    SearchLine { row: usize },
}

impl Spot {
    /// The spot naming a whole diff entry, whatever column or segment the
    /// pointer ends up in. What `leave_row` is given: a row is left as a row.
    pub fn diff_row(row: usize) -> Self {
        Self::Diff {
            row,
            side: 0,
            segment: 0,
        }
    }

    /// Same surface, same row — the columns and segments of a diff entry being
    /// one row.
    fn same_row(&self, other: &Self) -> bool {
        match (self, other) {
            (Self::Diff { row: a, .. }, Self::Diff { row: b, .. }) => a == b,
            (Self::SearchHit { row: a }, Self::SearchHit { row: b }) => a == b,
            (Self::SearchLine { row: a }, Self::SearchLine { row: b }) => a == b,
            _ => false,
        }
    }
}

/// What a line needs to make its symbols followable.
///
/// Built only while the modifier is held: `None` is the state of every line
/// almost all the time, and it costs exactly what it did before.
pub struct Armed<'a> {
    pub id: gpui::ElementId,
    pub spot: Spot,
    /// The word the pointer is over, in this text's own bytes — the one to
    /// underline. It comes from the frame before, which is what a hover always
    /// does.
    pub hovered: Option<Range<usize>>,
    pub entity: &'a Entity<ClaudhubApp>,
}

impl ClaudhubApp {
    /// The pointer has moved onto a word: underline that one.
    ///
    /// `None` for the word means the pointer is on the same text but between
    /// two of them — a space, a bracket — which is a reason to take the
    /// underline away, not to leave the last one standing.
    pub(super) fn hover_word(
        &mut self,
        spot: Spot,
        word: Option<Range<usize>>,
        cx: &mut Context<Self>,
    ) {
        let next = word.map(|word| (spot, word));
        if self.follow_hover != next {
            self.follow_hover = next;
            cx.notify();
        }
    }

    /// The pointer is over a row: whatever is underlined elsewhere — another
    /// row, or another surface entirely — is not under it any more.
    ///
    /// **Not the row's own words**: those are a child of it, and a child is
    /// dispatched before its ancestor, so this runs *after* the text has said
    /// what it has. Without it a word stayed underlined once the pointer moved
    /// off the text and onto the line numbers, where nothing would ever have
    /// told it otherwise.
    pub(super) fn leave_row(&mut self, here: Spot, cx: &mut Context<Self>) {
        if self
            .follow_hover
            .as_ref()
            .is_some_and(|(spot, _)| !spot.same_row(&here))
        {
            self.follow_hover = None;
            cx.notify();
        }
    }
}

/// A line whose symbols can be followed: `Ctrl`+click on one looks it up.
///
/// **`InteractiveText` and not arithmetic on the pointer's x.** The column of a
/// click could be divided out of a fixed-pitch font, and it would be wrong the
/// day a glyph falls back to another face, or the gutter gains a pixel. This
/// asks the shaped line itself which character was hit, which is the same
/// question the caret of an editor answers.
pub fn followable(armed: Armed, text: SharedString, styled: StyledText) -> gpui::AnyElement {
    let ranges = crate::ui::search::word_ranges(&text);
    let (clicked, hovered) = (ranges.clone(), ranges.clone());
    let (for_click, for_hover) = (armed.entity.clone(), armed.entity.clone());
    let spot = armed.spot;
    gpui::InteractiveText::new(armed.id, styled)
        .on_click(ranges, move |index, window, cx| {
            if !window.modifiers().secondary() {
                return;
            }
            let Some(range) = clicked.get(index) else {
                return;
            };
            let symbol = text[range.clone()].to_string();
            for_click.update(cx, |this, cx| {
                this.follow_symbol(&symbol, window, cx);
            });
        })
        // **The character, not the word**: what comes back is where the pointer
        // is, and the word around it is ours to find. It only fires when that
        // character changes, so this costs a frame per character crossed and
        // nothing while the hand is still.
        .on_hover(move |index, _event, _window, cx| {
            let word =
                index.and_then(|at| hovered.iter().find(|range| range.contains(&at)).cloned());
            for_hover.update(cx, |this, cx| this.hover_word(spot, word, cx));
        })
        .into_any_element()
}

#[cfg(test)]
mod tests {
    use super::Spot;

    /// The columns and the wrapped segments of a diff entry are one row: the
    /// pointer crossing from the old side to the new one has not left it, and
    /// clearing the underline there would make it flicker.
    #[test]
    fn a_diff_entry_is_one_row_whatever_column_the_pointer_is_in() {
        let old = Spot::Diff {
            row: 7,
            side: 0,
            segment: 0,
        };
        let new = Spot::Diff {
            row: 7,
            side: 1,
            segment: 3,
        };
        assert!(old.same_row(&new));
        assert!(old.same_row(&Spot::diff_row(7)));
        assert!(!old.same_row(&Spot::diff_row(8)));
    }

    /// Two surfaces number their rows from zero: without the variant, hovering
    /// the seventh line of the preview would leave the seventh diff line
    /// underlined.
    #[test]
    fn the_same_number_on_two_surfaces_is_not_the_same_row() {
        assert!(!Spot::diff_row(3).same_row(&Spot::SearchHit { row: 3 }));
        assert!(!Spot::SearchHit { row: 3 }.same_row(&Spot::SearchLine { row: 3 }));
        assert!(Spot::SearchLine { row: 3 }.same_row(&Spot::SearchLine { row: 3 }));
    }
}
