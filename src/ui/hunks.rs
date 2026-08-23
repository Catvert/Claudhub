//! What the editor's gutter marks: where the buffer differs from its git base.
//!
//! PhpStorm paints a strip beside the line numbers — green where lines were
//! added, blue where they changed, a wedge where some were deleted — and the
//! strip answers *while you type*, not after a save. That is the whole reason
//! this is here rather than a call to `git diff`: `git diff` compares what is on
//! disk, and the buffer that matters is the one under the caret. So the base
//! text is fetched once (`git show HEAD:<path>`) and the comparison is redone in
//! process on every edit.
//!
//! Which means a line differ, and no crate for it: the comparison is *patience*
//! — anchor on lines that appear exactly once on both sides, recurse between the
//! anchors — because that is what makes a hunk land where a reader would put it.
//! A greedy edit script is free to pair the closing brace of one function with
//! the closing brace of the next, and the marker then straddles two things that
//! have nothing to do with each other.
//!
//! No gpui in here, and none wanted: this is the part that goes wrong quietly.
//! A rollback splices bytes back into a live buffer, so a span that is off by a
//! newline eats a line nobody asked it to.

use std::ops::Range;

/// What a hunk is, read off its two sides rather than stored.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    /// Lines the buffer has and the base does not.
    Added,
    /// Lines the base has and the buffer does not. Its `rows` are empty: a
    /// deletion sits *between* two lines, it does not occupy one.
    Removed,
    /// Both sides have lines here, and they differ.
    Changed,
}

/// One run of difference between the base and the buffer.
///
/// `rows` counts lines in the **buffer**, which is what the gutter paints
/// against; `old` is what the base had there, verbatim and without line
/// endings, which is what the band shows and what a rollback puts back.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Hunk {
    pub rows: Range<usize>,
    pub old: Vec<String>,
}

impl Hunk {
    pub fn kind(&self) -> Kind {
        match (self.rows.is_empty(), self.old.is_empty()) {
            (false, true) => Kind::Added,
            (true, false) => Kind::Removed,
            _ => Kind::Changed,
        }
    }

    /// The line a deletion's wedge is drawn against.
    ///
    /// A removal's `rows` are empty, so it has no line of its own; it is drawn
    /// on the boundary above `rows.start`, and clicking anywhere on that line
    /// has to find it. Clamping to the last line matters for a deletion at the
    /// very end of the file, whose `rows.start` is one past it.
    pub fn anchor(&self, last: usize) -> usize {
        self.rows.start.min(last)
    }

    /// Whether a click on `row` should open this hunk.
    pub fn covers(&self, row: usize, last: usize) -> bool {
        if self.rows.is_empty() {
            row == self.anchor(last)
        } else {
            self.rows.contains(&row)
        }
    }
}

/// Splits a buffer the way this module counts lines.
///
/// `split('\n')` and not `lines()`, and the difference is the point: `lines()`
/// reads "a\n" and "a" as the same document, so adding the final newline to a
/// file would show no hunk at all. The trailing empty part *is* the final
/// newline, and it takes part in the comparison like any other line.
fn split(text: &str) -> Vec<&str> {
    text.split('\n').collect()
}

/// Where the differences are, in the order the buffer has them.
pub fn compare(base: &str, now: &str) -> Vec<Hunk> {
    let base = split(base);
    let now = split(now);
    regions(&base, &now)
        .into_iter()
        .map(|(b, n)| Hunk {
            rows: n,
            old: base[b].iter().map(|line| (*line).to_string()).collect(),
        })
        .collect()
}

/// Where two line sequences differ, as one index range on each side, in order.
///
/// The comparison itself, with nothing said about which side is a base and
/// which a buffer: the gutter reads it once, the three-way merge reads it twice
/// — the base against ours, the base against theirs — and builds its chunks out
/// of what the two answers agree on.
pub fn regions(a: &[&str], b: &[&str]) -> Vec<(Range<usize>, Range<usize>)> {
    let mut out = Vec::new();
    // An explicit stack rather than recursion: patience peels one anchor run at
    // a time, so a file of ten thousand near-identical lines would nest that
    // deep and blow the stack for a document the editor was willing to open.
    let mut todo = vec![(0..a.len(), 0..b.len())];
    while let Some((x, y)) = todo.pop() {
        region(a, b, x, y, &mut todo, &mut out);
    }
    out.sort_by_key(|(x, y)| (y.start, y.end, x.start));
    out
}

/// Trims what the two sides share at either end, then either emits the rest as
/// one hunk or splits it on anchors and pushes the pieces back.
fn region(
    base: &[&str],
    now: &[&str],
    mut b: Range<usize>,
    mut n: Range<usize>,
    todo: &mut Vec<(Range<usize>, Range<usize>)>,
    out: &mut Vec<(Range<usize>, Range<usize>)>,
) {
    while b.start < b.end && n.start < n.end && base[b.start] == now[n.start] {
        b.start += 1;
        n.start += 1;
    }
    while b.start < b.end && n.start < n.end && base[b.end - 1] == now[n.end - 1] {
        b.end -= 1;
        n.end -= 1;
    }
    if b.is_empty() && n.is_empty() {
        return;
    }
    if b.is_empty() || n.is_empty() {
        out.push((b, n));
        return;
    }

    let anchors = anchors(&base[b.clone()], &now[n.clone()]);
    if anchors.is_empty() {
        // Nothing appears exactly once on both sides — a block of closing
        // braces, a table of near-identical rows. Splitting it on a greedy
        // pairing would put the marker somewhere no one would defend, so the
        // whole run is one hunk, which is at least true.
        out.push((b, n));
        return;
    }

    let mut bi = b.start;
    let mut ni = n.start;
    for (ab, an) in anchors {
        let (ab, an) = (b.start + ab, n.start + an);
        todo.push((bi..ab, ni..an));
        bi = ab + 1;
        ni = an + 1;
    }
    todo.push((bi..b.end, ni..n.end));
}

/// The lines that appear exactly once on each side, paired and kept in an order
/// both sides agree on.
///
/// Two passes: pair up the unique lines, then keep the longest run of pairs
/// whose buffer positions rise — the pairs left out are lines that moved past
/// one another, and honouring them would produce hunks that cross.
fn anchors(base: &[&str], now: &[&str]) -> Vec<(usize, usize)> {
    use std::collections::HashMap;

    let mut seen: HashMap<&str, (usize, usize)> = HashMap::new();
    for line in base {
        seen.entry(line).or_insert((0, usize::MAX)).0 += 1;
    }
    let mut pairs: Vec<(usize, usize)> = Vec::new();
    let mut once: HashMap<&str, usize> = HashMap::new();
    for (i, line) in base.iter().enumerate() {
        if seen.get(line).is_some_and(|(count, _)| *count == 1) {
            once.insert(line, i);
        }
    }
    let mut counts: HashMap<&str, usize> = HashMap::new();
    for line in now {
        *counts.entry(line).or_insert(0) += 1;
    }
    for (j, line) in now.iter().enumerate() {
        if counts.get(line) == Some(&1) {
            if let Some(&i) = once.get(line) {
                pairs.push((i, j));
            }
        }
    }
    pairs.sort_unstable();
    longest_rising(&pairs)
}

/// The longest subsequence of pairs whose second coordinate rises.
///
/// Patience sorting: piles of decreasing tops, a binary search per pair, and a
/// back-pointer per pair so the run can be read back out at the end.
fn longest_rising(pairs: &[(usize, usize)]) -> Vec<(usize, usize)> {
    if pairs.is_empty() {
        return Vec::new();
    }
    // `tops[k]` is the index into `pairs` of the smallest tail among the runs of
    // length k + 1 found so far.
    let mut tops: Vec<usize> = Vec::new();
    let mut back: Vec<Option<usize>> = vec![None; pairs.len()];
    for (i, pair) in pairs.iter().enumerate() {
        let at = tops.partition_point(|&t| pairs[t].1 < pair.1);
        back[i] = at.checked_sub(1).map(|k| tops[k]);
        if at == tops.len() {
            tops.push(i);
        } else {
            tops[at] = i;
        }
    }
    let mut run = Vec::with_capacity(tops.len());
    let mut at = tops.last().copied();
    while let Some(i) = at {
        run.push(pairs[i]);
        at = back[i];
    }
    run.reverse();
    run
}

/// The byte span a hunk occupies in the buffer, and what replaces it.
///
/// Both halves are given together because they have to agree about the newline:
/// deleting rows means deleting the line breaks that carry them, and which break
/// belongs to the span depends on whether the run reaches the end of the file.
/// Getting that wrong takes a neighbouring line with it, and the buffer's undo
/// is then the only way back.
pub fn rollback(now: &str, hunk: &Hunk) -> (Range<usize>, String) {
    let lines = split(now);
    let last = lines.len();
    let mut replacement = hunk.old.join("\n");

    // A run that ends short of the last line owns the break that follows it;
    // one that reaches the end owns the break *before* it instead, there being
    // none after.
    if hunk.rows.end < last {
        let start = offset_of(&lines, hunk.rows.start);
        let end = offset_of(&lines, hunk.rows.end);
        if !hunk.old.is_empty() {
            replacement.push('\n');
        }
        (start..end, replacement)
    } else if hunk.rows.start == 0 {
        (0..now.len(), replacement)
    } else {
        let start = offset_of(&lines, hunk.rows.start) - 1;
        if !hunk.old.is_empty() {
            replacement.insert(0, '\n');
        }
        (start..now.len(), replacement)
    }
}

/// The byte at which a line starts, counting the breaks before it.
fn offset_of(lines: &[&str], row: usize) -> usize {
    lines[..row].iter().map(|l| l.len() + 1).sum()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rows(hunks: &[Hunk]) -> Vec<(Range<usize>, Vec<&str>)> {
        hunks
            .iter()
            .map(|h| {
                (
                    h.rows.clone(),
                    h.old.iter().map(|s| s.as_str()).collect::<Vec<_>>(),
                )
            })
            .collect()
    }

    #[test]
    fn an_untouched_buffer_has_no_hunks() {
        assert!(compare("a\nb\nc\n", "a\nb\nc\n").is_empty());
        assert!(compare("", "").is_empty());
    }

    #[test]
    fn the_three_kinds_are_read_off_the_two_sides() {
        let added = compare("a\nc\n", "a\nb\nc\n");
        assert_eq!(rows(&added), vec![(1..2, vec![])]);
        assert_eq!(added[0].kind(), Kind::Added);

        let removed = compare("a\nb\nc\n", "a\nc\n");
        assert_eq!(rows(&removed), vec![(1..1, vec!["b"])]);
        assert_eq!(removed[0].kind(), Kind::Removed);

        let changed = compare("a\nb\nc\n", "a\nB\nc\n");
        assert_eq!(rows(&changed), vec![(1..2, vec!["b"])]);
        assert_eq!(changed[0].kind(), Kind::Changed);
    }

    /// The final newline is a line here, so adding one is a hunk — and it is
    /// exactly the change `lines()` would have hidden.
    #[test]
    fn the_last_newline_takes_part() {
        let hunks = compare("a", "a\n");
        assert_eq!(rows(&hunks), vec![(1..2, vec![])]);
    }

    #[test]
    fn several_runs_come_back_in_the_buffers_order() {
        let base = "one\ntwo\nthree\nfour\nfive\n";
        let now = "one\nTWO\nthree\nfour\nfive\nsix\n";
        assert_eq!(
            rows(&compare(base, now)),
            vec![(1..2, vec!["two"]), (5..6, vec![])]
        );
    }

    /// The anchor pass is what keeps a marker from straddling two functions: a
    /// block inserted between two others must not pair its closing brace with
    /// the next one's.
    #[test]
    fn a_block_inserted_between_two_others_lands_where_it_was_written() {
        let base = "fn a() {\n    one();\n}\nfn c() {\n    three();\n}\n";
        let now = "fn a() {\n    one();\n}\nfn b() {\n    two();\n}\nfn c() {\n    three();\n}\n";
        assert_eq!(rows(&compare(base, now)), vec![(3..6, vec![])]);
    }

    /// Nothing unique on either side: one honest hunk beats a pairing nobody
    /// would defend.
    #[test]
    fn a_run_with_no_anchor_stays_in_one_piece() {
        let hunks = compare("}\n}\n}\n", "}\n}\n}\n}\n");
        assert_eq!(hunks.len(), 1);
        assert_eq!(hunks[0].kind(), Kind::Added);
    }

    #[test]
    fn a_deletion_is_found_from_the_line_it_sits_against() {
        let hunks = compare("a\nb\nc\n", "a\nc\n");
        let last = 2;
        assert!(hunks[0].covers(1, last), "the line the deletion precedes");
        assert!(!hunks[0].covers(0, last));
    }

    /// A deletion at the very end has no line after it to sit against.
    #[test]
    fn a_deletion_at_the_end_is_clamped_onto_the_last_line() {
        let hunks = compare("a\nb\nc", "a");
        let now = split("a");
        let last = now.len() - 1;
        assert!(hunks[0].covers(last, last));
    }

    fn undo(base: &str, now: &str) -> String {
        let mut text = now.to_string();
        // Back to front: each splice moves everything after it.
        for hunk in compare(base, now).into_iter().rev() {
            let (span, replacement) = rollback(&text, &hunk);
            text.replace_range(span, &replacement);
        }
        text
    }

    /// The property that matters: rolling every hunk back is the base again,
    /// newline for newline. Anything the spans get wrong shows up here.
    #[test]
    fn rolling_every_hunk_back_gives_the_base_again() {
        let cases = [
            ("a\nb\nc\n", "a\nB\nc\n"),
            ("a\nb\nc\n", "a\nc\n"),
            ("a\nc\n", "a\nb\nc\n"),
            ("a\nb\nc\n", "a\nb\nc"),
            ("a\nb\nc", "a\nb\nc\n"),
            ("a\nb\nc\n", ""),
            ("", "a\nb\nc\n"),
            ("one\ntwo\nthree\n", "ONE\ntwo\nTHREE\nfour\n"),
            ("x\n", "a\nb\nx\nc\nd\n"),
            ("a\nb\nc\nd\ne\n", "a\ne\n"),
        ];
        for (base, now) in cases {
            assert_eq!(undo(base, now), base, "base {base:?} now {now:?}");
        }
    }

    /// One hunk at a time is the real gesture, and it must leave the others be.
    #[test]
    fn rolling_one_hunk_back_leaves_the_others_alone() {
        let base = "one\ntwo\nthree\nfour\n";
        let now = "ONE\ntwo\nTHREE\nfour\n";
        let hunks = compare(base, now);
        assert_eq!(hunks.len(), 2);
        let mut text = now.to_string();
        let (span, replacement) = rollback(&text, &hunks[0]);
        text.replace_range(span, &replacement);
        assert_eq!(text, "one\ntwo\nTHREE\nfour\n");
    }

    /// An empty base is one empty line, and the buffer's own last empty line
    /// matches it — so a file written from nothing is an addition and not a
    /// replacement, which is what its gutter should say.
    #[test]
    fn a_file_that_grew_from_nothing_is_one_hunk() {
        let hunks = compare("", "a\nb\n");
        assert_eq!(rows(&hunks), vec![(0..2, vec![])]);
        assert_eq!(hunks[0].kind(), Kind::Added);
    }
}
