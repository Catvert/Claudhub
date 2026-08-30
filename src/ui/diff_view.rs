//! The diff view.
//!
//! A diff is read line by line, and an agent-review diff regularly runs to
//! several thousand. Every line is therefore flattened into a single list —
//! hunk headers included — and rendered by a virtualised list: only what is on
//! screen exists as elements.
//!
//! Flattening is what makes virtualisation possible: a list can only address its
//! entries by an index, whereas a diff is a two-level tree (hunks, then lines).

use std::path::Path;

use gpui_component::highlighter::HighlightTheme;

use crate::git::{DiffLineKind, FileDiff};
use crate::ui::highlight::DiffHighlights;

/// A diff ready to display.
///
/// Everything derived from the diff — flattening, highlighting, staging patches,
/// gutter width — is computed here, once, when the diff arrives. Rendering then
/// only reads: that is what lets a frame cost only the visible lines, whereas
/// highlighting a ten-thousand-line file is measured in tens of milliseconds.
pub struct Rendered {
    pub file: FileDiff,
    pub rows: Vec<Row>,
    /// The same thing in two columns, paired line by line. Built at the same
    /// time as the rest rather than on the first switch to "split" mode: a
    /// `Rendered` is shared by `Rc` and never changes again, and the cost bears
    /// no comparison to the highlighting's.
    pub split: Vec<SplitRow>,
    pub highlights: DiffHighlights,
    /// The file this diff is of, as the patch that stages a hunk names it.
    pub path: std::path::PathBuf,
    /// Every line's text, ready to hand to gpui.
    ///
    /// A `SharedString` is an `Arc`: the render hands one out for every visible
    /// line of every frame, and building it from the `String` copied the line
    /// each time. Indexed by hunk, then by line.
    pub texts: Vec<Vec<SharedString>>,
    /// The `@@ … @@` headers, for the same reason.
    pub headers: Vec<SharedString>,
    /// The words that changed **inside** a line, as byte ranges of its text,
    /// indexed like `texts`.
    ///
    /// Only the lines the column pairing puts opposite each other carry any:
    /// a removal facing an addition is two versions of one line, and the
    /// ranges say which words differ — the block colour alone leaves the eye
    /// hunting one identifier across a hundred unchanged columns. Computed
    /// here once, like everything derived from the diff.
    pub words: Vec<Vec<Vec<std::ops::Range<usize>>>>,
    pub gutter_digits: usize,
    /// The index of the widest row and its length in characters.
    ///
    /// The virtualised list measures only one item to decide the scrollable
    /// width. Naming it the longest line is what lets horizontal scrolling reach
    /// the end of the file; without that it stops at the width of the first
    /// line, which is almost always short.
    pub longest_row: usize,
    pub longest_chars: usize,
    /// The length of every entry, in characters, in `rows` order.
    ///
    /// It is what gives a wrapped row's height without touching the text: the
    /// diff's font is fixed-pitch, one character is one column, and the number
    /// of visible lines of a long line is a division. Computed once here because
    /// the height is recomputed on every width change — a separator drag
    /// produces one per frame — and rewalking the file's text every time would
    /// cost what virtualisation saves.
    pub row_chars: Vec<usize>,
    /// The diff carries a single version of the file: brand new — only
    /// additions — or deleted, only removals.
    ///
    /// It is what lets the two-column mode decline: pairing such a file puts a
    /// full column of text against a full column of "nothing opposite" tint,
    /// and the unified layout shows the same content at the full width. The
    /// fallback is index-safe by construction — with no removal facing an
    /// addition, every line becomes exactly one pair, so the split and unified
    /// lists have the same entries in the same order.
    pub one_sided: bool,
    /// The file's changes: each a maximal run of added and removed lines, as
    /// the first and last index of the unified list.
    ///
    /// A hunk is what git cuts, and it cuts by context: asked for the whole
    /// file (`Settings::context_lines`), it hands back **one** hunk, and "the
    /// hunk under the eye" would be the file — `j`/`k` leapt from file to file
    /// and the gutter marked everything. What the eye calls a change is the red
    /// and green block; the whole-file view reads these instead, see `blocks`.
    pub changes: Vec<(usize, usize)>,
    /// The heights `v_virtual_list` walks in wrapped mode, kept between frames.
    ///
    /// They depend on the column count and the line height and on nothing else,
    /// a `Rendered` never changing again: the key is those two, and a resize or
    /// a zoom is what rebuilds them — not every frame.
    wrap_sizes: std::cell::RefCell<Option<WrapSizes>>,
}

/// The wrapped list's sizes, and the three things they depend on.
type WrapSizes = (bool, usize, Pixels, Rc<Vec<gpui::Size<Pixels>>>);

impl Rendered {
    pub fn new(path: &Path, file: FileDiff, theme: &HighlightTheme) -> Self {
        let rows = rows(&file);
        // The widths first, the widest read off them: two sweeps of the file's
        // text where one does.
        let row_chars: Vec<usize> = rows.iter().map(|row| row_width(&file, *row)).collect();
        let (longest_row, longest_chars) = longest(&row_chars);
        let split = split_rows(&file, &rows);
        Self {
            one_sided: one_sided(&file),
            words: word_marks(&file, &rows, &split),
            row_chars,
            changes: changes(&file, &rows),
            highlights: DiffHighlights::compute(path, &file, theme),
            path: path.to_path_buf(),
            texts: file
                .hunks
                .iter()
                .map(|hunk| {
                    hunk.lines
                        .iter()
                        .map(|line| SharedString::from(line.text.clone()))
                        .collect()
                })
                .collect(),
            headers: file
                .hunks
                .iter()
                .map(|hunk| SharedString::from(hunk.header.clone()))
                .collect(),
            gutter_digits: gutter_digits(&file),
            longest_row,
            longest_chars,
            split,
            wrap_sizes: std::cell::RefCell::new(None),
            rows,
            file,
        }
    }

    /// The sizes of the wrapped list, from the cache.
    ///
    /// `v_virtual_list` wants a vector as long as the list, and building it on
    /// every frame walked every entry of the file — the very sweep
    /// virtualisation exists to avoid.
    ///
    /// **The mode is part of the key**, not only the width: the two lists do
    /// not have the same entries, so a vector kept from one would be read
    /// against the other's indices — heights taken from the wrong rows, and one
    /// vector too short or too long for the list walking it.
    fn wrap_sizes(
        &self,
        split: bool,
        cols: usize,
        line_height: Pixels,
    ) -> Rc<Vec<gpui::Size<Pixels>>> {
        let mut slot = self.wrap_sizes.borrow_mut();
        if let Some((had_split, had_cols, had_height, sizes)) = slot.as_ref() {
            if *had_split == split && *had_cols == cols && *had_height == line_height {
                return sizes.clone();
            }
        }
        let heights = if split {
            split_heights(self, cols)
        } else {
            unified_heights(self, cols)
        };
        let sizes = Rc::new(
            heights
                .into_iter()
                .map(|lines| gpui::size(px(0.), line_height * lines as f32))
                .collect::<Vec<_>>(),
        );
        *slot = Some((split, cols, line_height, sizes.clone()));
        sizes
    }

    /// A line's text as gpui takes it: an `Arc` clone, not a copy.
    fn line_text(&self, hunk: usize, line: usize) -> Option<&SharedString> {
        self.texts.get(hunk)?.get(line)
    }

    /// The changed-word ranges of a line — empty for context lines, for a
    /// change with no counterpart, and for lines the pairing declined to
    /// refine.
    pub fn word_ranges(&self, hunk: usize, line: usize) -> &[std::ops::Range<usize>] {
        self.words
            .get(hunk)
            .and_then(|h| h.get(line))
            .map(Vec::as_slice)
            .unwrap_or(&[])
    }

    /// The text of a range of lines, ready to paste.
    ///
    /// Without the markers, what comes out is **code**: no `+`/`-`, no line
    /// numbers, no `@@` headers. It is what one wants to paste into an editor or
    /// into an agent's prompt, and cleaning it by hand afterwards is exactly the
    /// chore this view is meant to save.
    ///
    /// With the markers, it is a patch excerpt: git's signs — `-` and not the
    /// display's real minus sign, which does not apply.
    pub fn copy_text(&self, from: usize, to: usize, with_markers: bool) -> String {
        let (from, to) = (from.min(to), from.max(to));
        let mut out = String::new();
        for index in from..=to {
            let Some(row) = self.rows.get(index).copied() else {
                continue;
            };
            match row {
                Row::Header { .. } => {
                    if !with_markers {
                        continue;
                    }
                    out.push_str(self.row_text(row));
                    out.push('\n');
                }
                Row::Line { hunk, line } => {
                    let Some(source) = self
                        .file
                        .hunks
                        .get(hunk)
                        .and_then(|hunk| hunk.lines.get(line))
                    else {
                        continue;
                    };
                    // "\ No newline at end of file" is a patch annotation, not a
                    // line of the file.
                    if source.kind == DiffLineKind::NoNewline && !with_markers {
                        continue;
                    }
                    if with_markers {
                        out.push_str(patch_sign(source.kind));
                    }
                    out.push_str(&source.text);
                    out.push('\n');
                }
            }
        }
        out
    }

    /// Which hunk a displayed entry belongs to.
    ///
    /// It is what says "you are here": the entry under the selection names a
    /// hunk, and every row of that hunk is marked. A paired entry answers by
    /// its old half, and by its new one when the old is empty — both belong to
    /// the same hunk anyway.
    pub fn hunk_of(&self, index: usize, split: bool) -> Option<usize> {
        if !split {
            return match self.rows.get(index)? {
                Row::Header { hunk } | Row::Line { hunk, .. } => Some(*hunk),
            };
        }
        match self.split.get(index)? {
            SplitRow::Header { hunk, .. } => Some(*hunk),
            SplitRow::Pair { old, new } => old.or(*new).and_then(|row| self.hunk_of(row, false)),
        }
    }

    /// The indices of a hunk's first and last line.
    pub fn hunk_bounds(&self, hunk: usize) -> Option<(usize, usize)> {
        let first = self
            .rows
            .iter()
            .position(|row| matches!(row, Row::Header { hunk: h } if *h == hunk))?;
        let last = self
            .rows
            .iter()
            .rposition(|row| matches!(row, Row::Line { hunk: h, .. } if *h == hunk))
            .unwrap_or(first);
        Some((first, last))
    }

    /// Which change a displayed entry belongs to — `hunk_of`, for the
    /// whole-file view.
    pub fn change_of(&self, index: usize, split: bool) -> Option<usize> {
        let unified = if split {
            match self.split.get(index)? {
                SplitRow::Header { .. } => return None,
                SplitRow::Pair { old, new } => old.or(*new)?,
            }
        } else {
            index
        };
        self.changes
            .iter()
            .position(|(first, last)| (*first..=*last).contains(&unified))
    }

    /// The index, in the displayed list, of a unified entry.
    fn display_index(&self, unified: usize, split: bool) -> Option<usize> {
        if !split {
            return Some(unified);
        }
        self.split
            .iter()
            .position(|row| row.unified().any(|index| index == unified))
    }

    /// What `j`/`k` stop on and what the gutter marks, in the displayed list:
    /// the hunk headers, or — whole file asked — the first line of every
    /// change, with the block each one opens, as `(start, end)`.
    ///
    /// One list for both readings, so that what is marked is what the key
    /// reaches: with the whole file on screen the single header sits at the
    /// top, far from the first change, and would be a stop at nothing.
    pub fn blocks(&self, split: bool, whole_file: bool) -> Vec<(usize, usize)> {
        if whole_file {
            return self
                .changes
                .iter()
                .filter_map(|(first, last)| {
                    Some((
                        self.display_index(*first, split)?,
                        self.display_index(*last, split)?,
                    ))
                })
                .collect();
        }
        let headers = self.headers(split);
        let len = self.len(split);
        headers
            .iter()
            .enumerate()
            .map(|(h, start)| {
                let end = headers.get(h + 1).map_or(len, |next| *next);
                (*start, end.saturating_sub(1).max(*start))
            })
            .collect()
    }

    /// Which block a displayed entry belongs to: `hunk_of`, or `change_of`
    /// when the whole file is shown.
    pub fn block_of(&self, index: usize, split: bool, whole_file: bool) -> Option<usize> {
        if whole_file {
            self.change_of(index, split)
        } else {
            self.hunk_of(index, split)
        }
    }

    /// The number of entries in the displayed list.
    pub fn len(&self, split: bool) -> usize {
        if split {
            self.split.len()
        } else {
            self.rows.len()
        }
    }

    /// Brings a range of the two-column list back to the unified list.
    ///
    /// It is the unified list that carries the file's order: paired, the removed
    /// and added lines end up on the same entry, and a copy has to return both
    /// in the order git writes them.
    pub fn unified_span(&self, from: usize, to: usize) -> Option<(usize, usize)> {
        let (from, to) = (from.min(to), from.max(to));
        let mut bounds: Option<(usize, usize)> = None;
        for row in self
            .split
            .get(from..=to.min(self.split.len().saturating_sub(1)))?
        {
            for index in row.unified() {
                bounds = Some(match bounds {
                    Some((a, b)) => (a.min(index), b.max(index)),
                    None => (index, index),
                });
            }
        }
        bounds
    }

    /// The index, in the **displayed** list, of a line named by its hunk and its
    /// rank.
    ///
    /// A search hit is about the file's text, not about a list entry: it is the
    /// translation between the two that is missing, and it depends on the
    /// layout — paired, a removal and the addition answering it sit on the same
    /// entry.
    pub fn display_row(&self, hunk: usize, line: usize, split: bool) -> Option<usize> {
        let unified = self.rows.iter().position(
            |row| matches!(row, Row::Line { hunk: h, line: l } if *h == hunk && *l == line),
        )?;
        if !split {
            return Some(unified);
        }
        self.split
            .iter()
            .position(|row| row.unified().any(|index| index == unified))
    }

    /// The indices of the hunk headers in the displayed list, in increasing
    /// order.
    pub fn headers(&self, split: bool) -> Vec<usize> {
        if split {
            self.split
                .iter()
                .enumerate()
                .filter(|(_, row)| matches!(row, SplitRow::Header { .. }))
                .map(|(index, _)| index)
                .collect()
        } else {
            self.rows
                .iter()
                .enumerate()
                .filter(|(_, row)| matches!(row, Row::Header { .. }))
                .map(|(index, _)| index)
                .collect()
        }
    }

    /// An entry's text, for measuring as for rendering.
    pub fn row_text(&self, row: Row) -> &str {
        match row {
            Row::Header { hunk } => self
                .file
                .hunks
                .get(hunk)
                .map(|h| h.header.as_str())
                .unwrap_or_default(),
            Row::Line { hunk, line } => self
                .file
                .hunks
                .get(hunk)
                .and_then(|h| h.lines.get(line))
                .map(|l| l.text.as_str())
                .unwrap_or_default(),
        }
    }
}

/// The widest entry, in number of characters.
///
/// Counted in characters and not in bytes: at fixed pitch, it is the number of
/// characters that gives the width, and an accent takes two bytes for a single
/// column.
fn longest(row_chars: &[usize]) -> (usize, usize) {
    let mut best = (0usize, 0usize);
    for (index, width) in row_chars.iter().copied().enumerate() {
        if width > best.1 {
            best = (index, width);
        }
    }
    best
}

/// An entry's length, in characters and not in bytes: it is a width on screen,
/// and an accent counts as one there.
fn row_width(file: &FileDiff, row: Row) -> usize {
    let text = match row {
        Row::Header { hunk } => file.hunks.get(hunk).map(|h| h.header.as_str()),
        Row::Line { hunk, line } => file
            .hunks
            .get(hunk)
            .and_then(|h| h.lines.get(line))
            .map(|l| l.text.as_str()),
    };
    text.map(|t| t.chars().count()).unwrap_or(0)
}

// — Line wrapping in the two-column view ——————————————————————————————

/// How many visible lines an entry `chars` characters wide takes up.
///
/// Wrapping happens **at the column**, as in a terminal, and not at spaces: that
/// is what makes the height computable ahead of time. gpui's shaper, for its
/// part, breaks at words, and a guessed height that does not land exactly would
/// let the rows overlap — the virtualised list reserves exactly what it is told.
pub fn wrapped_lines(chars: usize, cols: usize) -> usize {
    if cols == 0 {
        return 1;
    }
    chars.div_ceil(cols).max(1)
}

/// The height of every entry of the two-column view, in lines.
///
/// A pair is as tall as the taller of its two halves: both versions stay
/// opposite each other, which is the whole point of this view.
pub fn split_heights(diff: &Rendered, cols: usize) -> Vec<usize> {
    diff.split
        .iter()
        .map(|row| match row {
            SplitRow::Header { .. } => 1,
            SplitRow::Pair { old, new } => [old, new]
                .into_iter()
                .flatten()
                .map(|index| wrapped_lines(diff.row_chars.get(*index).copied().unwrap_or(0), cols))
                .max()
                .unwrap_or(1),
        })
        .collect()
}

/// The same, of the unified list: one entry per line, a header being one row.
///
/// Simpler than `split_heights` for the reason the list itself is: an entry is
/// one version of one line, so its height is that line's and nothing else —
/// there is no half opposite to be as tall as.
pub fn unified_heights(diff: &Rendered, cols: usize) -> Vec<usize> {
    diff.rows
        .iter()
        .enumerate()
        .map(|(index, row)| match row {
            Row::Header { .. } => 1,
            Row::Line { .. } => {
                wrapped_lines(diff.row_chars.get(index).copied().unwrap_or(0), cols)
            }
        })
        .collect()
}

/// Where a line wraps, in bytes: one offset per segment, plus the end.
///
/// Counted in characters and cut in bytes: at fixed pitch it is characters that
/// give the column, and an accent takes two bytes for one column. One sweep of
/// the text for the whole line — taking the segments one by one restarted from
/// the beginning each time, which made a long line quadratic in its own length.
fn wrap_offsets(text: &str, cols: usize, segments: usize) -> Vec<usize> {
    let mut out = Vec::with_capacity(segments + 1);
    out.push(0);
    if cols > 0 {
        for (count, (offset, _)) in text.char_indices().enumerate() {
            if count > 0 && count % cols == 0 {
                out.push(offset);
            }
        }
    }
    // A segment past the end of the text is empty, not missing: the half
    // opposite may be taller, and it is the caller that decides how many lines
    // the row has.
    while out.len() <= segments {
        out.push(text.len());
    }
    out
}

/// A slice's ranges, brought back to its start.
///
/// They stay **sorted and disjoint**, the invariant gpui does not check and
/// whose violation shifts everything after it — the slicing only clips ranges
/// already in that order.
fn slice_runs<T: Clone>(
    runs: &[(std::ops::Range<usize>, T)],
    span: &std::ops::Range<usize>,
) -> Vec<(std::ops::Range<usize>, T)> {
    runs.iter()
        .filter_map(|(range, style)| {
            let start = range.start.max(span.start);
            let end = range.end.min(span.end);
            (start < end).then(|| (start - span.start..end - span.start, style.clone()))
        })
        .collect()
}

/// One entry of the list.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Row {
    /// The `@@ … @@` header, which also carries the hunk's staging button.
    Header {
        hunk: usize,
    },
    Line {
        hunk: usize,
        line: usize,
    },
}

/// Flattens a diff, headers included.
pub fn rows(diff: &FileDiff) -> Vec<Row> {
    let mut rows = Vec::new();
    for (h, hunk) in diff.hunks.iter().enumerate() {
        rows.push(Row::Header { hunk: h });
        rows.extend((0..hunk.lines.len()).map(|line| Row::Line { hunk: h, line }));
    }
    rows
}

/// The changes of a diff, as runs of the unified list — see
/// `Rendered::changes`.
///
/// A "no newline" marker belongs to the line before it: it neither opens nor
/// closes a run. A header closes one, being never a change itself.
pub fn changes(diff: &FileDiff, rows: &[Row]) -> Vec<(usize, usize)> {
    let mut out: Vec<(usize, usize)> = Vec::new();
    let mut open = false;
    for (index, row) in rows.iter().enumerate() {
        let kind = match row {
            Row::Header { .. } => None,
            Row::Line { hunk, line } => diff
                .hunks
                .get(*hunk)
                .and_then(|h| h.lines.get(*line))
                .map(|l| l.kind),
        };
        match kind {
            Some(DiffLineKind::Added | DiffLineKind::Removed) => {
                if open {
                    out.last_mut().expect("an open run has a start").1 = index;
                } else {
                    out.push((index, index));
                    open = true;
                }
            }
            Some(DiffLineKind::NoNewline) => {
                if open {
                    out.last_mut().expect("an open run has a start").1 = index;
                }
            }
            _ => open = false,
        }
    }
    out
}

/// One entry of the two-column list.
///
/// The indices are the **unified** list's: it stays the reference — for the
/// text, the highlighting and the copying — and the two columns are only another
/// arrangement of it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SplitRow {
    Header {
        hunk: usize,
        row: usize,
    },
    /// A left line, a right one, or only one of the two: an addition with no
    /// removal opposite leaves the left empty, and vice versa.
    Pair {
        old: Option<usize>,
        new: Option<usize>,
    },
}

impl SplitRow {
    /// The unified list's entries this entry covers.
    pub fn unified(self) -> impl Iterator<Item = usize> {
        let (a, b) = match self {
            SplitRow::Header { row, .. } => (Some(row), None),
            SplitRow::Pair { old, new } => (old, new),
        };
        a.into_iter().chain(b)
    }
}

/// Pairs a diff's two versions.
///
/// A block of removals followed by a block of additions is what git writes for a
/// change: pairing them rank by rank puts the two versions of one line opposite
/// each other, which is the whole point of the column view. When the blocks are
/// not the same height, the shorter one ends with empty slots — there is nothing
/// to show opposite.
pub fn split_rows(diff: &FileDiff, rows: &[Row]) -> Vec<SplitRow> {
    let mut out = Vec::new();
    let mut olds: Vec<usize> = Vec::new();
    let mut news: Vec<usize> = Vec::new();
    for (index, row) in rows.iter().enumerate() {
        match *row {
            Row::Header { hunk } => {
                pair_up(&mut olds, &mut news, &mut out);
                out.push(SplitRow::Header { hunk, row: index });
            }
            Row::Line { hunk, line } => {
                let kind = diff
                    .hunks
                    .get(hunk)
                    .and_then(|h| h.lines.get(line))
                    .map(|l| l.kind);
                match kind {
                    Some(DiffLineKind::Removed) => olds.push(index),
                    Some(DiffLineKind::Added) => news.push(index),
                    // A context line belongs to both versions: it closes the
                    // current block and occupies both columns.
                    _ => {
                        pair_up(&mut olds, &mut news, &mut out);
                        out.push(SplitRow::Pair {
                            old: Some(index),
                            new: Some(index),
                        });
                    }
                }
            }
        }
    }
    pair_up(&mut olds, &mut news, &mut out);
    out
}

/// The changed words of every paired line, indexed `[hunk][line]`.
///
/// The pairing is the one the column view shows: a removal facing an addition
/// is two versions of one line, and those are the only rows worth refining —
/// a context line has not changed, and a change with nothing opposite has no
/// version to differ from. `refine::word_changes` declines the pairs that
/// share no word, so rank-pairing two unrelated lines marks nothing.
pub fn word_marks(
    diff: &FileDiff,
    rows: &[Row],
    split: &[SplitRow],
) -> Vec<Vec<Vec<std::ops::Range<usize>>>> {
    let mut out: Vec<Vec<Vec<std::ops::Range<usize>>>> = diff
        .hunks
        .iter()
        .map(|hunk| vec![Vec::new(); hunk.lines.len()])
        .collect();
    for row in split {
        // A context line occupies both columns under one index: same line,
        // nothing to compare.
        let SplitRow::Pair {
            old: Some(old),
            new: Some(new),
        } = row
        else {
            continue;
        };
        if old == new {
            continue;
        }
        let line_at = |index: usize| match rows.get(index)? {
            Row::Line { hunk, line } => Some((*hunk, *line)),
            Row::Header { .. } => None,
        };
        let Some(((old_hunk, old_line), (new_hunk, new_line))) = line_at(*old).zip(line_at(*new))
        else {
            continue;
        };
        let text_of = |hunk: usize, line: usize, kind: DiffLineKind| {
            let source = diff.hunks.get(hunk)?.lines.get(line)?;
            (source.kind == kind).then_some(source.text.as_str())
        };
        let Some((old_text, new_text)) = text_of(old_hunk, old_line, DiffLineKind::Removed)
            .zip(text_of(new_hunk, new_line, DiffLineKind::Added))
        else {
            continue;
        };
        if let Some((removed, added)) = crate::ui::refine::word_changes(old_text, new_text) {
            out[old_hunk][old_line] = removed;
            out[new_hunk][new_line] = added;
        }
    }
    out
}

fn pair_up(olds: &mut Vec<usize>, news: &mut Vec<usize>, out: &mut Vec<SplitRow>) {
    for i in 0..olds.len().max(news.len()) {
        out.push(SplitRow::Pair {
            old: olds.get(i).copied(),
            new: news.get(i).copied(),
        });
    }
    olds.clear();
    news.clear();
}

/// Whether the diff carries a single version of the file — see
/// `Rendered::one_sided`.
///
/// A context line proves two versions at once; otherwise it is the presence of
/// exactly one of the two kinds that says so — a diff with both has lines to
/// pair, and an empty one has no version at all.
pub fn one_sided(file: &FileDiff) -> bool {
    let mut added = false;
    let mut removed = false;
    for line in file.hunks.iter().flat_map(|hunk| hunk.lines.iter()) {
        match line.kind {
            DiffLineKind::Context => return false,
            DiffLineKind::Added => added = true,
            DiffLineKind::Removed => removed = true,
            DiffLineKind::NoNewline => {}
        }
    }
    added != removed
}

/// The number of digits of the file's largest line number.
///
/// The gutter is sized once for the whole diff: computing it per screen would
/// make it change width mid-scroll, going from line 99 to line 100.
pub fn gutter_digits(diff: &FileDiff) -> usize {
    diff.hunks
        .iter()
        .flat_map(|hunk| hunk.lines.iter())
        .filter_map(|line| line.new_no.max(line.old_no))
        .max()
        .unwrap_or(1)
        .to_string()
        .len()
}

/// The sign shown in front of a line.
pub fn sign(kind: DiffLineKind) -> &'static str {
    match kind {
        DiffLineKind::Added => "+",
        // A real minus sign, and not git's hyphen: at fixed pitch it lines up
        // with the `+`, whereas the hyphen floats.
        DiffLineKind::Removed => "−",
        DiffLineKind::Context | DiffLineKind::NoNewline => " ",
    }
}

/// git's sign, the one that keeps an excerpt an applicable patch.
fn patch_sign(kind: DiffLineKind) -> &'static str {
    match kind {
        DiffLineKind::Added => "+",
        DiffLineKind::Removed => "-",
        DiffLineKind::Context => " ",
        DiffLineKind::NoNewline => "",
    }
}

// — Rendering ————————————————————————————————————————————————————————

use std::rc::Rc;

use gpui::{
    div, prelude::*, px, uniform_list, App, Context, Entity, Focusable,
    ListHorizontalSizingBehavior, Pixels, SharedString, StyledText, Window,
};
use gpui_component::{
    button::{Button, ButtonVariants},
    h_flex,
    menu::ContextMenuExt,
    v_flex, v_virtual_list, ActiveTheme, Selectable, Sizable, StyledExt,
};

use crate::git::DiffRange;
use crate::tr;
use crate::ui::app::ClaudhubApp;
use crate::ui::follow::{followable, Armed, Spot};
use crate::ui::icons::icon;
use crate::ui::theme::DiffColors;

/// A row's height, as a proportion of the text size.
///
/// It is fixed and not measured: every entry is exactly one line tall, and an
/// explicit height spares the virtualised list from measuring anything. The
/// factor follows the chosen size, otherwise enlarging the text would make it
/// spill out of a height that had stayed constant.
const LINE_SPACING: f32 = 1.5;

/// The diff's scrollbar, and therefore the key of its smoothing: a single value
/// for both, see `ui::scroll`.
const DIFF_SCROLL: &str = "diff-lines-bar";

/// A movement that does not depend on the current line — or that depends on it
/// only through a view height.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Jump {
    Start,
    End,
    PageUp,
    PageDown,
}

pub fn line_height(font_size: Pixels) -> Pixels {
    (font_size * LINE_SPACING).round()
}

impl ClaudhubApp {
    /// Selects a line, or extends the selection to it.
    ///
    /// Plain click: the line becomes the anchor. Shift+click: the anchor stays
    /// and the head moves, which is every list's convention and saves dragging
    /// over three hundred lines to catch a block of them.
    pub(super) fn select_diff_row(&mut self, index: usize, extend: bool, cx: &mut Context<Self>) {
        let Some(state) = self.active_review_mut() else {
            return;
        };
        let next = match (extend, state.diff_selection) {
            (true, Some((anchor, _))) => Some((anchor, index)),
            _ => Some((index, index)),
        };
        // A drag goes through this function on every hovered line: without this
        // guard, every pixel of movement would ask for a render of the whole
        // list for a selection that has not moved.
        if state.diff_selection == next {
            return;
        }
        state.diff_selection = next;
        cx.notify();
    }

    /// What the right click selects before opening its menu.
    ///
    /// The line under the cursor, unless it is already part of what is selected
    /// — a menu opened on a block of lines has to act on that block. Without
    /// this the menu acted on a selection made somewhere else, or on none at
    /// all, while pointing at the line the eye had just picked.
    pub(super) fn aim_diff_row(&mut self, index: usize, cx: &mut Context<Self>) {
        let inside = self
            .active_review()
            .and_then(|state| state.diff_selection)
            .is_some_and(|(anchor, head)| (anchor.min(head)..=anchor.max(head)).contains(&index));
        if inside {
            return;
        }
        self.select_diff_row(index, false, cx);
    }

    /// Extends the selection during a drag.
    pub(super) fn drag_diff_row(&mut self, index: usize, cx: &mut Context<Self>) {
        if !self.diff_dragging {
            return;
        }
        self.select_diff_row(index, true, cx);
    }

    pub(super) fn end_diff_drag(&mut self) {
        self.diff_dragging = false;
    }

    /// Switches between one column and two.
    ///
    /// The selection is dropped: its indices name the displayed list, and the
    /// two lists do not count the same entries.
    pub(super) fn toggle_diff_split(&mut self, cx: &mut Context<Self>) {
        crate::ui::settings::Settings::update_global(cx, |s| s.diff_split = !s.diff_split);
        if let Some(state) = self.active_review_mut() {
            state.diff_selection = None;
        }
        cx.notify();
    }

    /// Wraps long lines, or lets them run.
    ///
    /// The selection falls, as on a mode change: its indices name the displayed
    /// list, and the two lists do not have the same geometry.
    pub(super) fn toggle_diff_wrap(&mut self, cx: &mut Context<Self>) {
        crate::ui::settings::Settings::update_global(cx, |s| s.diff_wrap = !s.diff_wrap);
        cx.notify();
    }

    /// Switches between "the whole file" and the changes alone.
    ///
    /// The diff is re-read: the elided lines are nowhere in what is held in
    /// memory, git alone knows what they contained.
    pub(super) fn toggle_whole_file(&mut self, cx: &mut Context<Self>) {
        crate::ui::settings::Settings::update_global(cx, |s| {
            s.diff_whole_file = !s.diff_whole_file
        });
        self.reload_diff(cx);
    }

    /// Selects the whole displayed diff.
    pub(super) fn select_whole_diff(&mut self, cx: &mut Context<Self>) {
        let split = crate::ui::settings::Settings::global(cx).diff_split;
        let Some(last) = self
            .active_review()
            .and_then(|state| state.diff.as_ref())
            .map(|diff| diff.len(split).saturating_sub(1))
        else {
            return;
        };
        let Some(state) = self.active_review_mut() else {
            return;
        };
        state.diff_selection = Some((0, last));
        cx.notify();
    }

    /// Copies the selection, or the whole file if there is none.
    ///
    /// With no selection, `Ctrl+C` on a diff can only mean one thing, and
    /// refusing to act would be a polite refusal for no reason.
    pub(super) fn copy_diff(&mut self, with_markers: bool, cx: &mut Context<Self>) {
        let Some(state) = self.active_review() else {
            return;
        };
        let Some(diff) = state.diff.clone() else {
            return;
        };
        // Copying always starts from the unified list: it is what carries the
        // file's order. In two columns, the selection is brought back to it.
        let split = crate::ui::settings::Settings::global(cx).diff_split;
        let (from, to) = match (split, state.diff_selection) {
            (true, Some((a, b))) => match diff.unified_span(a, b) {
                Some(span) => span,
                None => return,
            },
            (false, Some(span)) => span,
            (_, None) => (0, diff.rows.len().saturating_sub(1)),
        };
        self.copy_rows(&diff, from, to, with_markers, cx);
    }

    pub(super) fn copy_hunk(&mut self, hunk: usize, with_markers: bool, cx: &mut Context<Self>) {
        let Some(diff) = self.active_review().and_then(|state| state.diff.clone()) else {
            return;
        };
        let Some((from, to)) = diff.hunk_bounds(hunk) else {
            return;
        };
        self.copy_rows(&diff, from, to, with_markers, cx);
    }

    fn copy_rows(
        &mut self,
        diff: &Rc<Rendered>,
        from: usize,
        to: usize,
        with_markers: bool,
        cx: &mut Context<Self>,
    ) {
        let text = diff.copy_text(from, to, with_markers);
        if text.is_empty() {
            return;
        }
        let lines = text.lines().count();
        cx.write_to_clipboard(gpui::ClipboardItem::new_string(text));
        self.announce(tr!("copy-done", { count: lines }), cx);
    }

    pub(super) fn copy_diff_path(&mut self, cx: &mut Context<Self>) {
        let Some(path) = self
            .active_review()
            .and_then(|state| state.selected.clone())
        else {
            return;
        };
        let path = path.display().to_string();
        cx.write_to_clipboard(gpui::ClipboardItem::new_string(path));
        self.announce(tr!("copy-path-done"), cx);
    }

    /// Recomputes the displayed diff's hits, if the query or the diff has
    /// changed.
    ///
    /// Called at render time, but it only works on changes: comparing one string
    /// per frame is what it costs not to have to notify every place a query can
    /// change from.
    pub(super) fn refresh_diff_search(&mut self, query: &str) {
        if self.diff_search.valid && self.diff_search.query == query {
            return;
        }
        let mut hits = Vec::new();
        if let Some(diff) = self.active_review().and_then(|state| state.diff.clone()) {
            for (h, hunk) in diff.file.hunks.iter().enumerate() {
                for (l, line) in hunk.lines.iter().enumerate() {
                    hits.extend(
                        crate::ui::find::find_all(query, &line.text)
                            .into_iter()
                            .map(|range| crate::ui::find::Hit {
                                hunk: h,
                                line: l,
                                range,
                            }),
                    );
                }
            }
        }
        // Filed by line because that is how rendering looks them up, and it does
        // so for every visible line of every frame.
        let mut by_line: std::collections::HashMap<(usize, usize), Vec<std::ops::Range<usize>>> =
            std::collections::HashMap::new();
        for hit in &hits {
            by_line
                .entry((hit.hunk, hit.line))
                .or_default()
                .push(hit.range.clone());
        }
        self.diff_search = crate::ui::find::DiffSearch {
            query: query.to_string(),
            valid: true,
            hits: std::rc::Rc::new(hits),
            by_line: std::rc::Rc::new(by_line),
            current: 0,
            landed: false,
        };
    }

    /// Moves to the next or previous hit, wrapping.
    ///
    /// It wraps, unlike keyboard review which stops at both ends: a search that
    /// stops at the last hit forces going back up by hand to see the first
    /// again, whereas the point is precisely to go round what was found.
    pub(super) fn step_diff_match(&mut self, delta: isize, cx: &mut Context<Self>) {
        let query = self.query(crate::ui::find::Pane::Diff, cx);
        self.refresh_diff_search(&query);
        let total = self.diff_search.hits.len();
        if total == 0 {
            return;
        }
        // A fresh search first reveals where it stands — the first occurrence
        // forwards, the last backwards; only then does Enter step.
        let current = if self.diff_search.landed {
            (self.diff_search.current as isize + delta).rem_euclid(total as isize) as usize
        } else if delta > 0 {
            0
        } else {
            total - 1
        };
        self.diff_search.current = current;
        self.diff_search.landed = true;
        let Some(hit) = self.diff_search.hits.get(current).cloned() else {
            return;
        };
        let split = crate::ui::settings::Settings::global(cx).diff_split;
        let row = self
            .active_review()
            .and_then(|state| state.diff.as_ref())
            .and_then(|diff| diff.display_row(hit.hunk, hit.line, split));
        let Some(row) = row else {
            return;
        };
        if let Some(state) = self.active_review_mut() {
            state.diff_selection = Some((row, row));
        }
        self.reveal_diff_row(row, gpui::ScrollStrategy::Center, cx);
        cx.notify();
    }

    /// Moves the selection by one line, and brings it back into view.
    ///
    /// `extend` keeps the anchor in place: it is Shift+arrow, which takes a
    /// block of lines with the keyboard as the drag takes it with the mouse.
    pub(super) fn step_diff_row(&mut self, delta: isize, extend: bool, cx: &mut Context<Self>) {
        let split = crate::ui::settings::Settings::global(cx).diff_split;
        let Some(len) = self
            .active_review()
            .and_then(|state| state.diff.as_ref())
            .map(|diff| diff.len(split))
        else {
            return;
        };
        let current = self.active_review().and_then(|state| state.diff_selection);
        let Some(head) = step(current.map(|(_, head)| head), delta, len) else {
            return;
        };
        let anchor = match current {
            Some((anchor, _)) if extend => anchor,
            _ => head,
        };
        self.move_diff_selection(anchor, head, cx);
    }

    /// Goes from one end of the file to the other, or by one view height.
    ///
    /// A page's height is the panel's, measured on the previous frame: it is
    /// what "page" means to the eye, and a fixed number of lines would be worth
    /// twice as much once the font is enlarged.
    pub(super) fn jump_diff(&mut self, jump: Jump, cx: &mut Context<Self>) {
        let split = crate::ui::settings::Settings::global(cx).diff_split;
        let page = self.page_rows(cx);
        let Some(len) = self
            .active_review()
            .and_then(|state| state.diff.as_ref())
            .map(|diff| diff.len(split))
        else {
            return;
        };
        if len == 0 {
            return;
        }
        let last = len - 1;
        let current = self
            .active_review()
            .and_then(|state| state.diff_selection)
            .map(|(_, head)| head)
            .unwrap_or(0);
        let target = match jump {
            Jump::Start => 0,
            Jump::End => last,
            Jump::PageUp => current.saturating_sub(page),
            Jump::PageDown => (current + page).min(last),
        };
        self.move_diff_selection(target, target, cx);
    }

    /// How many lines fit in the view.
    ///
    /// At least one: a view never painted has no bounds, and a page of zero
    /// lines would make a key a gesture with no effect.
    pub(super) fn page_rows(&self, cx: &App) -> usize {
        let height = self.diff_base_handle(cx).bounds().size.height;
        let line = line_height(px(crate::ui::settings::Settings::global(cx).diff_font_size));
        ((f32::from(height) / f32::from(line)) as usize).max(1)
    }

    /// Jumps to the previous or next hunk, and to the neighbouring file once the
    /// last one is passed.
    ///
    /// Reviewing is going from one change to the next: the context lines between
    /// two hunks have nothing to show. With the whole file on screen a hunk
    /// *is* the file, so the stops are the changes themselves (`Rendered::
    /// blocks`). And a review does not stop at the end of
    /// a file — the same arrow carries on into the next, entering it from the
    /// end it came from.
    pub(super) fn step_diff_hunk(&mut self, delta: isize, cx: &mut Context<Self>) {
        // The three-pane merge takes this place, so it takes the gesture with
        // it: what "the next change" means there is the next conflict, and
        // having to reach for a different key on a screen that looks the same
        // is the kind of thing one learns twice and remembers neither time.
        if self.merging_shown() {
            self.merge_step(delta, cx);
            return;
        }
        let settings = crate::ui::settings::Settings::global(cx);
        let (split, whole_file) = (settings.diff_split, settings.diff_whole_file);
        let stops: Vec<usize> = self
            .active_review()
            .and_then(|state| state.diff.as_ref())
            .map(|diff| {
                diff.blocks(split, whole_file)
                    .into_iter()
                    .map(|(start, _)| start)
                    .collect()
            })
            .unwrap_or_default();
        let from = self
            .active_review()
            .and_then(|state| state.diff_selection)
            .map(|(_, head)| head);
        match next_header(&stops, from, delta) {
            Some(target) => self.move_diff_selection_to_hunk(target, cx),
            None => self.step_file_to_a_hunk(delta, cx),
        }
    }

    /// Moves to the neighbouring file and records which end to enter it by.
    ///
    /// The selection cannot be set here: the diff will only arrive after the git
    /// command. It is `Evt::FileDiff` that consumes it.
    fn step_file_to_a_hunk(&mut self, delta: isize, cx: &mut Context<Self>) {
        let before = self
            .active_review()
            .and_then(|state| state.selected.clone());
        self.step_file(delta, cx);
        let Some(state) = self.active_review_mut() else {
            return;
        };
        // Nothing moved — we were already at the end of the review: do not arm a
        // jump that would apply to the next file opened with the mouse.
        if state.selected == before {
            return;
        }
        state.pending_jump = Some(if delta > 0 {
            crate::ui::app::Jump::First
        } else {
            crate::ui::app::Jump::Last
        });
    }

    /// Selects a hunk's header and brings it under the eye.
    ///
    /// Apart from the scrolling, `move_diff_selection`: what changes is that the
    /// view **always** moves, see `reveal_diff_hunk`.
    fn move_diff_selection_to_hunk(&mut self, target: usize, cx: &mut Context<Self>) {
        let Some(state) = self.active_review_mut() else {
            return;
        };
        state.diff_selection = Some((target, target));
        self.reveal_diff_hunk(target, cx);
        cx.notify();
    }

    /// Does the hunk starting at that header fit under a centred header?
    ///
    /// It is what decides between centring the hunk and putting it at the top —
    /// see `reveal_diff_hunk`. The count is in **entries**, like `page_rows`: a
    /// wrapped line takes more than one row on screen, and the answer is only
    /// ever a choice between two placements — being a line or two out changes
    /// nothing to which one is right.
    pub(super) fn hunk_fits_below_the_middle(&self, header: usize, cx: &App) -> bool {
        self.hunk_rows(header, cx) <= self.page_rows(cx) / 2
    }

    /// How many rows the hunk opening at `header` takes, header included.
    ///
    /// Zero when no block starts there, which reads as "it fits": the row is
    /// then a line like any other, and moving the view for it is the ordinary
    /// case.
    fn hunk_rows(&self, header: usize, cx: &App) -> usize {
        let settings = crate::ui::settings::Settings::global(cx);
        let (split, whole_file) = (settings.diff_split, settings.diff_whole_file);
        let Some(diff) = self.active_review().and_then(|state| state.diff.as_ref()) else {
            return 0;
        };
        let end = diff
            .blocks(split, whole_file)
            .into_iter()
            .find(|(start, _)| *start == header)
            .map_or(header, |(_, end)| end + 1);
        end.saturating_sub(header)
    }

    fn move_diff_selection(&mut self, anchor: usize, head: usize, cx: &mut Context<Self>) {
        let Some(state) = self.active_review_mut() else {
            return;
        };
        state.diff_selection = Some((anchor, head));
        // Non-strict scrolling: an already-visible line does not make the view
        // jump, which leaves the eye where it is as long as one does not leave
        // the screen.
        self.reveal_diff_row(head, gpui::ScrollStrategy::Top, cx);
        cx.notify();
    }

    /// The diff's wheel: zoom when the platform key is held, smoothed scrolling
    /// otherwise.
    ///
    /// A single listener for both, and it cannot be otherwise: zoom and
    /// smoothing both want to **give back** the jump gpui has just applied, and
    /// two listeners would give it back twice.
    ///
    /// The list has in fact **already** scrolled when this listener runs — both
    /// are in the bubble phase, and the child is handled before its parent. gpui
    /// exposes no capture phase for the wheel: we therefore give the offset back
    /// rather than try to prevent it, otherwise every zoom notch would also make
    /// the reading jump three lines.
    pub(super) fn on_diff_scroll(
        &mut self,
        event: &gpui::ScrollWheelEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let handle = self.diff_base_handle(cx);
        if !event.modifiers.secondary() {
            if self
                .motion(DIFF_SCROLL.into(), crate::ui::motion::Axes::Both)
                .on_wheel(&handle, event, window)
            {
                cx.notify();
            }
            return;
        }
        // A zoom in the middle of a smoothed scroll: the transition would aim at
        // a position computed on lines that no longer have the same height.
        self.motion(DIFF_SCROLL.into(), crate::ui::motion::Axes::Both)
            .cancel();
        let delta = event.delta.pixel_delta(window.line_height().max(px(1.)));
        // Only one axis moves at a time: it is gpui's default behaviour, which
        // only lets the dominant component through.
        let undo = if delta.x.abs() > delta.y.abs() {
            gpui::point(delta.x, px(0.))
        } else {
            gpui::point(px(0.), delta.y)
        };
        handle.set_offset(handle.offset() - undo);

        let steps = crate::ui::terminal_view::zoom_steps(delta.y);
        if steps != 0. {
            crate::ui::settings::Settings::update_global(cx, |s| {
                s.zoom(crate::ui::settings::Zoom::Diff, steps);
            });
        }
        cx.notify();
    }
}

impl ClaudhubApp {
    /// The centre of the editing screen: the open file, or enough to know one
    /// has to be chosen.
    ///
    /// It no longer shares the diff's slot: they are two screens, and the tab
    /// says what it carries without having to change name.
    pub(super) fn render_editor_panel(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        // A picture takes the editor's place: the tab is the same, what fills
        // it is not. Asked first, because the editor state a preview holds is
        // empty and would draw as a blank file.
        if let Some(preview) = self.render_image_preview(cx) {
            return preview;
        }
        match self.render_editor(window, cx) {
            Some(editor) => editor.into_any_element(),
            None => centered_message(tr!("editor-pick-a-file"), cx),
        }
    }

    /// The block above a commit's diff: its message, its author, its hash.
    ///
    /// `None` outside a commit range — a working or branch diff has no single
    /// story to tell — and while the message has not arrived: the block is a
    /// caption, and a caption that appears with its text is better than one
    /// that flashes empty. The detail is matched against the range's id, not
    /// just trusted: it follows a git command, and the click may have moved on.
    fn render_commit_block(&self, cx: &mut Context<Self>) -> Option<gpui::AnyElement> {
        let state = self.active_review()?;
        let DiffRange::Commit { id, .. } = &state.range else {
            return None;
        };
        let detail = state.commit_detail.clone().filter(|d| &d.id == id)?;
        let mono = cx.theme().mono_font_family.clone();
        let muted = cx.theme().muted_foreground;
        Some(
            v_flex()
                .w_full()
                .flex_shrink_0()
                .px_3()
                .py_2()
                .gap_1()
                .border_b_1()
                .border_color(cx.theme().border)
                .bg(cx.theme().secondary)
                .child(
                    h_flex()
                        .w_full()
                        .gap_2()
                        .items_center()
                        .child(
                            div()
                                .flex_none()
                                .font_family(mono)
                                .text_xs()
                                .text_color(muted)
                                .child(detail.short.clone()),
                        )
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .truncate()
                                .text_sm()
                                .font_semibold()
                                .child(detail.subject.clone()),
                        )
                        .child(
                            div()
                                .flex_none()
                                .text_xs()
                                .text_color(muted)
                                .child(format!("{} · {}", detail.author, detail.date)),
                        ),
                )
                .when(!detail.body.is_empty(), |el| {
                    el.child(
                        div()
                            .id("diff-commit-body")
                            .w_full()
                            // Capped, and scrollable past the cap: a release
                            // message runs to dozens of lines, and the diff is
                            // what the panel is for. Derived from the text
                            // size, never a fixed pixel count.
                            .max_h(crate::ui::theme::row_height(cx) * 8.)
                            .overflow_y_scroll()
                            .text_xs()
                            .text_color(muted)
                            .children(
                                detail
                                    .body
                                    .split('\n')
                                    .map(|line| {
                                        // An empty child collapses to nothing:
                                        // a blank line of the message has to
                                        // keep its height.
                                        div().child(if line.is_empty() {
                                            SharedString::from(" ")
                                        } else {
                                            SharedString::from(line.to_string())
                                        })
                                    })
                                    .collect::<Vec<_>>(),
                            ),
                    )
                })
                .into_any_element(),
        )
    }

    pub(super) fn render_diff(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        // A conflicted file opened from the conflicts panel takes this place
        // instead of a diff: what a diff of an unmerged file shows is the
        // markers git wrote through it, which is the one thing nobody needs to
        // read.
        if let Some(merge) = self.render_merge(cx) {
            return merge;
        }
        // The hits are recomputed here rather than in every place the query can
        // change from: comparing one string per frame is the price of having
        // nobody to notify.
        let query = self.query(crate::ui::find::Pane::Diff, cx);
        self.refresh_diff_search(&query);
        let find = self.render_find(crate::ui::find::Pane::Diff, cx);
        let Some(state) = self.active_review() else {
            return div().into_any_element();
        };
        let Some(path) = state.selected.clone() else {
            // An empty commit has no file to open, but it still has a story:
            // its block stays above the hint.
            let message = centered_message(tr!("review-pick-a-file"), cx);
            return match self.render_commit_block(cx) {
                Some(block) => v_flex()
                    .size_full()
                    .child(block)
                    .child(div().flex_1().min_h_0().child(message))
                    .into_any_element(),
                None => message,
            };
        };
        let stageable = state.range == DiffRange::Working;
        let diff = state.diff.clone();
        let settings = crate::ui::settings::Settings::global(cx);
        let (split, whole_file) = (settings.diff_split, settings.diff_whole_file);
        let wrap = settings.diff_wrap;
        let mono = cx.theme().mono_font_family.clone();
        let font_size = px(crate::ui::settings::Settings::global(cx).diff_font_size);
        let line_height = line_height(font_size);

        let position = self.diff_file_position(&path, cx);
        let header =
            self.render_diff_header(&path, position, split, wrap, whole_file, mono.clone(), cx);

        let Some(diff) = diff else {
            return v_flex()
                .size_full()
                .children(self.render_commit_block(cx))
                .child(header)
                .child(hint(tr!("review-loading"), cx))
                .into_any_element();
        };
        if diff.file.binary {
            return v_flex()
                .size_full()
                .children(self.render_commit_block(cx))
                .child(header)
                .child(hint(tr!("review-binary"), cx))
                .into_any_element();
        }
        if diff.rows.is_empty() {
            return v_flex()
                .size_full()
                .children(self.render_commit_block(cx))
                .child(header)
                .child(hint(tr!("review-no-change"), cx))
                .into_any_element();
        }

        // A single-version file — created, or deleted — is shown unified even
        // in two-column mode: pairing it fills one column with "nothing
        // opposite" tint from top to bottom. The indices survive the swap (see
        // `Rendered::one_sided`), so everything that reads the setting
        // elsewhere still names the same rows; the toggle keeps its state and
        // applies again on the next two-sided file.
        let split = split && !diff.one_sided;

        let cell = cell_width(&mono, font_size, window);
        let layout = self.diff_layout(&diff, split, wrap, cell, window, cx);
        let DiffLayout {
            gutter,
            column,
            content_width,
            cols,
        } = layout;

        let colors = DiffColors::of(cx);
        let entity = cx.entity();
        let rows = diff.clone();
        let count = diff.len(split);
        let selection = self
            .active_review()
            .and_then(|state| state.diff_selection)
            .map(|(a, b)| (a.min(b), a.max(b)));
        let selection_bg = cx.theme().selection;
        // The annotated lines are computed when the diff arrives: the closure
        // below runs for every visible line on every frame, and therefore has
        // nothing to look up there.
        let marks = self
            .active_review()
            .map(|state| state.note_marks.clone())
            .unwrap_or_default();
        let note_color = cx.theme().warning;
        // Which hunk is being read, from the entry the selection ends on. It is
        // computed here and not in the closure below, which runs for every
        // visible entry of every frame.
        let current_hunk = self
            .active_review()
            .and_then(|state| state.diff_selection)
            .and_then(|(_, head)| diff.block_of(head, split, whole_file));
        let hunk_color = cx.theme().primary;
        // The hits are filed by line on every change of query or of diff, and the
        // closure below only looks them up: it runs for every visible line of
        // every frame.
        let search = SearchPaint {
            by_line: self.diff_search.by_line.clone(),
            current: self.diff_search.hits.get(self.diff_search.current).cloned(),
            color: crate::ui::find::highlight_color(false, cx),
            current_color: crate::ui::find::highlight_color(true, cx),
        };

        // One entry, whichever list asks for it: the two branches below differ
        // only in how the height is reserved, not in what they paint.
        // Read once for the frame: `on_modifiers_changed` on the root is what
        // asks for a new one when it flips, so this is never a frame behind for
        // long. See `ClaudhubApp::follow_armed`.
        let armed = self.follow_armed;
        let hovered = self.follow_hover.clone();
        let build = move |ix: usize, cx: &mut gpui::App| {
            let selected = selection.is_some_and(|(a, b)| ix >= a && ix <= b);
            let style = RowStyle {
                line_height,
                gutter,
                stageable,
                selected,
                selection_bg,
                annotated: marks.at(ix, split),
                note_color,
                current_hunk: current_hunk.is_some()
                    && rows.block_of(ix, split, whole_file) == current_hunk,
                hunk_color,
                armed,
                hovered: hovered.clone(),
            };
            if split {
                render_split_row(
                    &rows, ix, &colors, column, cols, &style, &search, &entity, cx,
                )
            } else {
                render_row(
                    &rows,
                    ix,
                    &colors,
                    content_width,
                    cols,
                    &style,
                    &search,
                    &entity,
                    cx,
                )
            }
        };

        // Wrapped, the two-column view no longer has entries of equal height: a
        // long line takes three, the one opposite a single one. `uniform_list`
        // finds the visible interval by a division and therefore cannot paint
        // it; `v_virtual_list` walks a vector of sizes, which we give it. It is
        // the only place where the extra cost is justified — and there is
        // nothing left to scroll horizontally, which this list would not know
        // how to do.
        let list = if wrap {
            let sizes = diff.wrap_sizes(split, cols, line_height);
            crate::ui::scroll::vertical(
                DIFF_SCROLL,
                &self.diff_wrap_scroll,
                v_virtual_list(
                    cx.entity(),
                    "diff-lines-wrapped",
                    sizes,
                    move |_, range, _window, cx| range.map(|ix| build(ix, cx)).collect::<Vec<_>>(),
                )
                .size_full()
                .font_family(mono)
                .text_size(font_size)
                .track_scroll(&self.diff_wrap_scroll),
            )
        } else {
            crate::ui::scroll::both(
                DIFF_SCROLL,
                &self.diff_scroll,
                uniform_list("diff-lines", count, move |range, _window, cx| {
                    range.map(|ix| build(ix, cx)).collect::<Vec<_>>()
                })
                .size_full()
                .font_family(mono)
                .text_size(font_size)
                // Without `Unconstrained`, the lines are constrained to the
                // view's width and horizontal scrolling has nothing to reveal;
                // the scrollable width is derived from the single item named
                // below.
                .with_horizontal_sizing_behavior(ListHorizontalSizingBehavior::Unconstrained)
                // In two columns, every entry has the same width — that of both
                // columns together — and any one of them therefore measures the
                // right one.
                .with_width_from_item(Some(if split { 0 } else { diff.longest_row }))
                .track_scroll(&self.diff_scroll.clone()),
            )
        };

        v_flex()
            .size_full()
            // Above the file bar: the commit captions the whole set of files,
            // where the bar names the one being read.
            .children(self.render_commit_block(cx))
            .child(header)
            .children(find)
            .child(
                div()
                    .id("diff-zoom")
                    .relative()
                    .flex_1()
                    .min_h_0()
                    // Everything below is derived from a width read **before**
                    // the frame is laid out, so the frame that follows a
                    // resize — a panel zoomed, a handle dropped — is painted at
                    // the size the view no longer has. Nothing would repaint it:
                    // a window only redraws on an event, and the next one is the
                    // background sweep, two seconds later. This measures the
                    // frame *after* layout and asks for one more when the width
                    // has moved, which settles as soon as it stops moving.
                    .child(
                        gpui::canvas(
                            {
                                let entity = cx.entity();
                                move |bounds: gpui::Bounds<Pixels>, window, cx| {
                                    entity.update(cx, |this, _| {
                                        if (bounds.size.width - this.diff_laid_out).abs() > px(0.5)
                                        {
                                            this.diff_laid_out = bounds.size.width;
                                            window.request_animation_frame();
                                        }
                                    });
                                }
                            },
                            |_, _, _, _| {},
                        )
                        .absolute()
                        .size_full(),
                    )
                    .on_scroll_wheel(cx.listener(Self::on_diff_scroll))
                    .on_mouse_up(
                        gpui::MouseButton::Left,
                        cx.listener(|this, _, _window, _cx| this.end_diff_drag()),
                    )
                    .child(list)
                    // The right click carries the gestures with no button to
                    // hand: lines have just been selected, and going back up to
                    // the header bar to act on them is one round trip too many.
                    .context_menu({
                        let entity = cx.entity();
                        move |menu, _window, _cx| diff_menu(menu, &entity)
                    }),
            )
            // The remainder of a partially staged file, under the diff: what
            // the next commit would leave behind, still one click from going
            // in. Absent for every other file.
            .children(self.render_unstaged_panel(cx))
            .into_any_element()
    }

    /// The diff's bar: the path, and the gestures that act on the file as a
    /// whole.
    #[allow(clippy::too_many_arguments)]
    fn render_diff_header(
        &self,
        path: &Path,
        // `position`: the file's one-based rank in the displayed list, and the
        // list's length. `None` when the list does not show the file.
        position: Option<(usize, usize)>,
        split: bool,
        wrap: bool,
        whole_file: bool,
        mono: SharedString,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        // Three groups and not one long row of children, because the middle one
        // is the only one that may shrink. A flex item's minimum size is that of
        // its content: without `min_w_0` on the path — and without the two
        // groups refusing to shrink — a long path pushed the buttons on the
        // right out of the bar, where nothing showed them and nothing said so.
        h_flex()
            .h(crate::ui::theme::bar_height(cx))
            .w_full()
            .px_2()
            .gap_2()
            .items_center()
            .overflow_hidden()
            .border_b_1()
            .border_color(cx.theme().border)
            .child(
                h_flex()
                    .flex_shrink_0()
                    .gap_1()
                    .items_center()
                    // Going from one change to the next is the gesture of a
                    // review, and it had only the arrow keys — which belong to
                    // whoever has the focus, so to nobody after a click in a
                    // terminal. The buttons are the same two moves, always here,
                    // and their tooltips name the keys for whoever would rather
                    // keep their hands still.
                    .child(self.step_button("prev-hunk", "arrow-up", -1, false, cx))
                    .child(self.step_button("next-hunk", "arrow-down", 1, false, cx))
                    .child(self.step_button("prev-file", "arrow-left", -1, true, cx))
                    // Between the two arrows it counts for: where one stands
                    // in the walk the arrows make, and how far the end is.
                    //
                    // **A width it keeps**, reserved for the widest rank this
                    // walk can show — which is the total itself. Sized by its
                    // own text, going from file 9 to file 10 made the label a
                    // digit wider and moved the "next file" arrow out from
                    // under the finger, in the middle of the one gesture
                    // nobody makes slowly. The measure is in monospace
                    // characters, so it holds in both catalogues and at any
                    // font size, and the text is centred in what it is given.
                    .when_some(position, |el, (rank, total)| {
                        let widest = tr!("diff-file-position", { rank: total, total: total });
                        el.child(
                            div()
                                .flex_none()
                                .min_w(gpui::rems(0.5 * widest.chars().count() as f32))
                                .text_center()
                                .text_xs()
                                .font_family(cx.theme().mono_font_family.clone())
                                .text_color(cx.theme().muted_foreground)
                                .child(tr!("diff-file-position", { rank: rank, total: total })),
                        )
                    })
                    .child(self.step_button("next-file", "arrow-right", 1, true, cx)),
            )
            .child(
                // The path and its opening gesture travel together: the button
                // hugs the file's name — one reads which file, then opens that
                // file — instead of sitting among the arrows at the far left.
                // It is this group that gives way. Without `min_w_0` the bar
                // is as wide as the longest path a review contains.
                h_flex()
                    .flex_1()
                    .min_w_0()
                    .gap_2()
                    .items_center()
                    // The writing gesture leads the group rather than trailing
                    // it: the path is what gives way when the bar narrows, and
                    // a button behind it was the first thing a long name
                    // pushed out of reach. Icon alone — a pencil beside a name
                    // is not a word anybody has to read.
                    .child(
                        Button::new("diff-edit")
                            .ghost()
                            .xsmall()
                            .icon(icon("pencil"))
                            .tooltip(tr!("diff-edit-tooltip"))
                            .on_click(cx.listener(|this, _, _window, cx| this.edit_diff_file(cx))),
                    )
                    // The same icon the tree and the changes list give this
                    // file: the shape says the family and the tint says the
                    // language, and a file recognised in one place should be
                    // recognised in the next.
                    .child(crate::ui::file_icons::file_icon(path, cx))
                    .child(
                        div()
                            .id("diff-path")
                            .min_w_0()
                            .truncate()
                            .text_sm()
                            .cursor_pointer()
                            .font_family(mono)
                            .tooltip(|window, cx| {
                                gpui_component::tooltip::Tooltip::new(tr!("action-copy-path"))
                                    .build(window, cx)
                            })
                            .on_click(cx.listener(|this, _, _window, cx| this.copy_diff_path(cx)))
                            .child(path.display().to_string()),
                    ),
            )
            .child(
                h_flex()
                    .flex_shrink_0()
                    .gap_1()
                    .items_center()
                    .child(
                        Button::new("diff-whole-file")
                            .ghost()
                            .xsmall()
                            // The icon says the current state, like the tree toggle: the
                            // whole file, or its changes alone.
                            .icon(icon(if whole_file { "file-text" } else { "file-diff" }))
                            .tooltip(if whole_file {
                                tr!("diff-hunks-only")
                            } else {
                                tr!("diff-whole-file")
                            })
                            .on_click(
                                cx.listener(|this, _, _window, cx| this.toggle_whole_file(cx)),
                            ),
                    )
                    .child(
                        Button::new("diff-split")
                            .ghost()
                            .xsmall()
                            .icon(icon(if split { "columns-2" } else { "list" }))
                            .tooltip(if split {
                                tr!("diff-unified")
                            } else {
                                tr!("diff-split")
                            })
                            .on_click(
                                cx.listener(|this, _, _window, cx| this.toggle_diff_split(cx)),
                            ),
                    )
                    // Offered in both modes: a single column is the view's
                    // width, not the file's, so a line longer than it scrolls
                    // just the same — and it is a file *added* that ends up
                    // here without having asked, two columns declining to pair
                    // a version against nothing.
                    .child(
                        Button::new("diff-wrap")
                            .ghost()
                            .xsmall()
                            .selected(wrap)
                            .icon(icon("wrap-text"))
                            .tooltip(if wrap {
                                tr!("diff-nowrap")
                            } else {
                                tr!("diff-wrap")
                            })
                            .on_click(
                                cx.listener(|this, _, _window, cx| this.toggle_diff_wrap(cx)),
                            ),
                    ),
            )
    }

    /// The reviewed file, and the line the selection rests on — one-based, as a
    /// file's lines are numbered everywhere else.
    ///
    /// The new version's number when the line has one, the old one otherwise: a
    /// deleted line no longer exists in the file one is about to open, and the
    /// place it used to hold is the closest thing to an answer.
    pub(super) fn diff_place(&self, cx: &App) -> Option<(std::path::PathBuf, usize)> {
        let split = crate::ui::settings::Settings::global(cx).diff_split;
        let state = self.active_review()?;
        let path = state.selected.clone()?;
        let line = state
            .diff
            .as_ref()
            .zip(state.diff_selection)
            .and_then(|(diff, (anchor, head))| {
                // In two columns the selection addresses the paired rows, whose
                // indices are not those of the flat list the numbers come from.
                let row = if split {
                    diff.unified_span(anchor, head)?.0
                } else {
                    anchor.min(head)
                };
                let Row::Line { hunk, line } = diff.rows.get(row).copied()? else {
                    return None;
                };
                let source = diff.file.hunks.get(hunk)?.lines.get(line)?;
                source.new_no.or(source.old_no)
            })
            .unwrap_or(1);
        Some((path, line))
    }

    /// Opens the file being reviewed in the built-in editor, at the line the
    /// selection rests on.
    ///
    /// The same path as the explorer's: the file is read by a worker, and it is
    /// its arrival that installs the editor and calls up its screen.
    pub(super) fn edit_diff_file(&mut self, cx: &mut Context<Self>) {
        let Some((path, line)) = self.diff_place(cx) else {
            return;
        };
        // `Landing::Position` counts from zero, a diff's numbers from one.
        let landing = crate::ui::explorer::Landing::Position {
            line: line.saturating_sub(1) as u32,
            character: 0,
        };
        self.open_at(path, Some(landing), cx);
    }

    /// One of the four navigation buttons of the diff's bar.
    ///
    /// Up and down go from one **change** to the next — the lines of context
    /// between two hunks have nothing to show — and overflow into the
    /// neighbouring file once the last one is passed; left and right change file
    /// outright. That is exactly what the arrow keys do, and it is the same code
    /// underneath: two ways of making one gesture that did not lead to the same
    /// place would be one too many.
    fn step_button(
        &self,
        id: &'static str,
        glyph: &'static str,
        delta: isize,
        by_file: bool,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let key = match (by_file, delta > 0) {
            (false, false) => "shortcut-previous-hunk",
            (false, true) => "shortcut-next-hunk",
            (true, false) => "shortcut-previous-file",
            (true, true) => "shortcut-next-file",
        };
        Button::new(id)
            .ghost()
            .xsmall()
            .icon(icon(glyph))
            .tooltip(tr!(key))
            .on_click(cx.listener(move |this, _, _window, cx| {
                if by_file {
                    this.step_file(delta, cx);
                } else {
                    this.step_diff_hunk(delta, cx);
                }
            }))
    }

    /// The widths the diff is painted at, and the frame's scrolling.
    ///
    /// Both at once because they come from the same reading: the width measured
    /// on the previous frame, which is what the smoothing has just written and
    /// what every column below is derived from.
    fn diff_layout(
        &mut self,
        diff: &Rendered,
        split: bool,
        wrap: bool,
        cell: Pixels,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> DiffLayout {
        let gutter = cell * diff.gutter_digits as f32 + px(6.);
        // The smoothing advances by one frame. Its order relative to building
        // the list is free: the offset is only read at layout time.
        let base = self.diff_base_handle(cx);
        self.motion(DIFF_SCROLL.into(), crate::ui::motion::Axes::Both)
            .advance(&base, window);
        // The content's width takes the viewport measured on the previous frame
        // into account: without that floor, the coloured background of a changed
        // line would stop at the end of its text instead of crossing the view.
        //
        // On the very first diff, that measurement does not exist yet. We then
        // ask for another frame: without it, the view keeps its initial width
        // until the next event — the background sweep, two seconds later — which
        // shows all the more since wrapping computes its columns on it. The
        // following diffs start from the recorded width.
        let measured = base.bounds().size.width;
        if measured > px(1.) {
            self.diff_width = measured;
            self.diff_measures = 0;
        } else if self.diff_measures < 4 {
            self.diff_measures += 1;
            window.request_animation_frame();
        }
        let viewport = if measured > px(1.) {
            measured
        } else {
            self.diff_width
        };
        let text_width = cell * diff.longest_chars as f32 + px(24.);
        // In two columns, each is cut for the file's longest line — and not for
        // half the view. Cutting them to the view would either cut the code or
        // wrap it, whereas all of it stays reachable through horizontal
        // scrolling, which carries both columns together and therefore keeps the
        // versions opposite each other.
        // Wrapped, the columns are the view and nothing else: that is the whole
        // point of wrapping, no longer having to scroll to read a long line.
        // Halved in two-column mode, whole in one.
        let column = if wrap {
            // The note margin (3 px) belongs to the entry, not to the columns:
            // forgetting it would make the row overflow by three pixels, which
            // no bar would reveal since wrapping removes one.
            let usable = (viewport - px(3.)).max(px(80.));
            if split {
                (usable / 2.).max(px(80.))
            } else {
                usable
            }
        } else {
            ((text_width + gutter).max(viewport / 2.)).max(px(80.))
        };
        let content_width = if split {
            column * 2.
        } else {
            (text_width + gutter * 2.).max(viewport)
        };
        // The text columns an entry holds: what is left once the gutters, the
        // sign and the note margin are taken. **Two gutters in one column** —
        // the unified list shows both numbers, where a half shows its own — and
        // counting one would wrap a gutter's worth of characters too late, past
        // the right edge. Zero when nothing wraps, which the rows read as "let
        // the line run".
        let cols = if wrap {
            let gutters = if split { gutter } else { gutter * 2. };
            ((f32::from((column - gutters - px(20.)).max(px(0.))) / f32::from(cell)) as usize)
                .max(8)
        } else {
            0
        };
        DiffLayout {
            gutter,
            column,
            content_width,
            cols,
        }
    }
}

/// The widths a diff is painted at, all derived from one character's width.
struct DiffLayout {
    gutter: Pixels,
    /// One column's width, in two-column mode.
    column: Pixels,
    /// The width of an entry of the unified list.
    content_width: Pixels,
    /// The text columns a half holds when wrapping. Zero when nothing wraps,
    /// which `half` reads as "let the line run".
    cols: usize,
}

/// A character's width, measured on the font actually chosen: a fixed pitch does
/// not mean a width known in advance, and a one-pixel discrepancy shifts the
/// gutter by a whole character after a hundred columns.
fn cell_width(mono: &SharedString, font_size: Pixels, window: &mut Window) -> Pixels {
    let font = gpui::Font {
        family: mono.clone(),
        features: Default::default(),
        weight: Default::default(),
        style: Default::default(),
        fallbacks: None,
    };
    let font_id = window.text_system().resolve_font(&font);
    window
        .text_system()
        .advance(font_id, font_size, 'M')
        .map(|size| size.width)
        .unwrap_or(px(7.))
}

/// The right click on a diff line: the gestures that have no button to hand.
fn diff_menu(
    menu: gpui_component::menu::PopupMenu,
    entity: &gpui::Entity<ClaudhubApp>,
) -> gpui_component::menu::PopupMenu {
    let (note, ask) = (entity.clone(), entity.clone());
    let (here, edit) = (entity.clone(), entity.clone());
    let (copy, patch) = (entity.clone(), entity.clone());
    menu.item(
        gpui_component::menu::PopupMenuItem::new(tr!("note-add"))
            .icon(icon("message-square-plus"))
            .on_click(move |_, window, cx| {
                note.update(cx, |this, cx| this.annotate_selection(window, cx));
            }),
    )
    .item(
        gpui_component::menu::PopupMenuItem::new(tr!("note-ask-title"))
            .icon(icon("bot"))
            .on_click(move |_, window, cx| {
                ask.update(cx, |this, cx| this.ask_about_selection(window, cx));
            }),
    )
    .item(
        // Editing a line is read from the diff: one walks the changes, one sees
        // what is wrong, and the file opens at that very line rather than at its
        // top with the number to find again.
        gpui_component::menu::PopupMenuItem::new(tr!("diff-edit-line"))
            .icon(icon("pencil"))
            .on_click(move |_, _window, cx| {
                here.update(cx, |this, cx| this.edit_diff_file(cx));
            }),
    )
    .item(
        gpui_component::menu::PopupMenuItem::new(tr!("editor-external"))
            .icon(icon("external-link"))
            .on_click(move |_, _window, cx| {
                edit.update(cx, |this, cx| this.open_diff_externally(cx));
            }),
    )
    .separator()
    .item(
        gpui_component::menu::PopupMenuItem::new(tr!("action-copy-file"))
            .icon(icon("copy"))
            .on_click(move |_, _window, cx| {
                copy.update(cx, |this, cx| this.copy_diff(false, cx));
            }),
    )
    .item(
        gpui_component::menu::PopupMenuItem::new(tr!("action-copy-patch"))
            .icon(icon("file-diff"))
            .on_click(move |_, _window, cx| {
                patch.update(cx, |this, cx| this.copy_diff(true, cx));
            }),
    )
}

/// What the search lays over the diff's lines.
///
/// Empty most of the time, and that is what counts: with no query, `by_line` is
/// empty too, `marks` returns an empty slice without allocating anything, and
/// the highlighting goes exactly down the path it took before.
#[derive(Clone)]
pub struct SearchPaint {
    pub by_line: crate::ui::find::MatchesByLine,
    /// The current hit, painted more brightly than the others: in a file with
    /// forty of them, "where am I" is the question.
    pub current: Option<crate::ui::find::Hit>,
    pub color: gpui::Hsla,
    pub current_color: gpui::Hsla,
}

impl SearchPaint {
    fn marks(&self, hunk: usize, line: usize) -> Vec<(std::ops::Range<usize>, gpui::Hsla)> {
        let Some(ranges) = self.by_line.get(&(hunk, line)) else {
            return Vec::new();
        };
        ranges
            .iter()
            .map(|range| {
                let current = self
                    .current
                    .as_ref()
                    .is_some_and(|hit| hit.hunk == hunk && hit.line == line && hit.range == *range);
                (
                    range.clone(),
                    if current {
                        self.current_color
                    } else {
                        self.color
                    },
                )
            })
            .collect()
    }
}

/// What changes from one entry to the next without coming from the diff: the
/// selection's state, the annotation, the geometry.
///
/// An aggregate rather than eight parameters: they crossed three functions, and
/// the compiler says nothing when two neighbouring booleans are swapped.
pub struct RowStyle {
    pub line_height: Pixels,
    /// The width of the line-number column.
    pub gutter: Pixels,
    pub stageable: bool,
    pub selected: bool,
    pub selection_bg: gpui::Hsla,
    /// A note is about this entry.
    pub annotated: bool,
    pub note_color: gpui::Hsla,
    /// The entry belongs to the hunk the selection is in.
    pub current_hunk: bool,
    pub hunk_color: gpui::Hsla,
    /// The modifier that makes a symbol clickable is held down.
    ///
    /// Per frame and not per row, but it travels with the rest: it decides what
    /// a row paints, exactly as the selection does. See `ui::follow`.
    pub armed: bool,
    /// The word the pointer is over, and which text it belongs to.
    ///
    /// One per frame, at most: the pointer is in one place. Every line is
    /// handed it and only the one it names underlines anything.
    pub hovered: Option<(Spot, std::ops::Range<usize>)>,
}

impl RowStyle {
    /// The word to underline in one text, if the pointer is in that one.
    fn hovered_word(&self, row: usize, side: u8, segment: usize) -> Option<std::ops::Range<usize>> {
        let (spot, word) = self.hovered.as_ref()?;
        (*spot == Spot::Diff { row, side, segment }).then(|| word.clone())
    }

    /// The rule that says which hunk one is reading.
    ///
    /// A **border** and not a child strip: it is outside the padding, so it
    /// lands at the same place on a header — which is padded — and on a line,
    /// which is not. Always there, transparent when it is not the current one:
    /// a width that appears and disappears would shift the whole row.
    fn hunk_rule<E: Styled>(&self, el: E) -> E {
        el.border_l_2().border_color(if self.current_hunk {
            self.hunk_color
        } else {
            gpui::transparent_black()
        })
    }
}

#[allow(clippy::too_many_arguments)]
fn render_row(
    diff: &Rc<Rendered>,
    index: usize,
    colors: &DiffColors,
    content_width: Pixels,
    // `cols`: text columns before wrapping, zero when the line runs and
    // horizontal scrolling takes care of it — `half`'s rule, on the one column
    // this list has.
    cols: usize,
    style: &RowStyle,
    search: &SearchPaint,
    entity: &Entity<ClaudhubApp>,
    cx: &mut gpui::App,
) -> gpui::AnyElement {
    let Some(row) = diff.rows.get(index).copied() else {
        return div().into_any_element();
    };
    let (selected, selection_bg) = (style.selected, style.selection_bg);
    match row {
        Row::Header { hunk } => {
            render_header(diff, index, hunk, colors, content_width, style, entity, cx)
        }
        Row::Line { hunk, line } => {
            let Some(source) = diff.file.hunks.get(hunk).and_then(|h| h.lines.get(line)) else {
                return div().into_any_element();
            };
            let (bg, fg) = line_colors(source.kind, colors);
            let word_bg = word_color(source.kind, colors);
            let marks = search.marks(hunk, line);
            let line_height = style.line_height;
            // Wrapped, the entry becomes a stack of fixed-height lines, as a
            // half does in two columns: the numbers and the sign stay on the
            // first, aligned to the top, and what follows is the continuation
            // of the text. Its height is then exactly the one announced to the
            // list — `unified_heights`, from this very count.
            let wrapped = cols > 0;
            let lines = if wrapped {
                wrapped_lines(diff.row_chars.get(index).copied().unwrap_or(0), cols)
            } else {
                1
            };
            let follow = |segment: usize| {
                style.armed.then(|| Armed {
                    id: ("diff-word", index).into(),
                    spot: Spot::Diff {
                        row: index,
                        side: 0,
                        segment,
                    },
                    hovered: style.hovered_word(index, 0, segment),
                    entity,
                })
            };

            let row = h_flex()
                .id(("line", index))
                .h(line_height * lines as f32)
                // No floor on the width once it wraps: the width is the view's,
                // there is nothing left to scroll to, and a minimum taken from
                // the longest line of the file would put a bar under a list
                // that has nowhere to go.
                .when(!wrapped, |el| el.min_w(content_width))
                .map(|el| {
                    if wrapped {
                        el.items_start()
                    } else {
                        el.items_center()
                    }
                })
                .whitespace_nowrap()
                // The selection replaces the row's background rather than adding
                // to it: gpui does not stack two backgrounds on one node, and a
                // selection barely distinguishable from the addition it covers
                // is useless.
                .when_some(bg.filter(|_| !selected), |el, bg| el.bg(bg))
                .when(selected, |el| el.bg(selection_bg))
                // The annotation marker is a rule in the margin, before the
                // numbers: it has to be visible without moving a column, and the
                // gutter is the only place horizontal scrolling does not take
                // out of view.
                .child(note_mark(style))
                .child(
                    number(source.old_no, style.gutter, colors)
                        .when(wrapped, |el| el.h(line_height)),
                )
                .child(
                    number(source.new_no, style.gutter, colors)
                        .when(wrapped, |el| el.h(line_height)),
                )
                .child(
                    div()
                        .w(px(14.))
                        .flex_none()
                        .text_center()
                        .when(wrapped, |el| el.h(line_height))
                        .when_some(fg, |el, fg| el.text_color(fg))
                        .child(sign(source.kind)),
                )
                .map(|el| {
                    if !wrapped {
                        return el.child(line_content(
                            diff,
                            hunk,
                            line,
                            fg,
                            word_bg,
                            &marks,
                            None,
                            follow(0),
                        ));
                    }
                    let bounds = wrap_offsets(&source.text, cols, lines);
                    el.child(
                        v_flex()
                            .flex_1()
                            .min_w_0()
                            .children((0..lines).map(|segment| {
                                // An id per segment, as in `half`: an element is
                                // named by the path of the ids above it, and the
                                // words of two segments would otherwise share one
                                // name.
                                div()
                                    .id(("segment", segment))
                                    .h(line_height)
                                    .child(line_content(
                                        diff,
                                        hunk,
                                        line,
                                        fg,
                                        word_bg,
                                        &marks,
                                        Some(bounds[segment]..bounds[segment + 1]),
                                        follow(segment),
                                    ))
                            })),
                    )
                });
            style
                .hunk_rule(with_row_gestures(row, index, entity))
                .into_any_element()
        }
    }
}

/// The three gestures every entry of the list carries: the click that selects,
/// the right click that aims the context menu, and the drag that extends the
/// selection.
///
/// One function: the three were copied into each of the three kinds of row, and
/// a row that forgets one is a row the selection stops at with nothing to say
/// so.
fn with_row_gestures<E: InteractiveElement>(
    el: E,
    index: usize,
    entity: &Entity<ClaudhubApp>,
) -> E {
    let (for_click, for_menu, for_drag) = (entity.clone(), entity.clone(), entity.clone());
    el.on_mouse_down(gpui::MouseButton::Left, move |event, window, cx| {
        select(&for_click, index, event.modifiers.shift, window, cx);
    })
    .on_mouse_down(gpui::MouseButton::Right, move |_, window, cx| {
        aim(&for_menu, index, window, cx);
    })
    .on_mouse_move(move |event, _window, cx| drag(&for_drag, index, event, cx))
}

/// The `@@ … @@` header, with its buttons. The same in both modes: it is about
/// the whole hunk, which does not have two versions.
#[allow(clippy::too_many_arguments)]
fn render_header(
    diff: &Rc<Rendered>,
    index: usize,
    hunk: usize,
    colors: &DiffColors,
    content_width: Pixels,
    style: &RowStyle,
    entity: &Entity<ClaudhubApp>,
    cx: &mut gpui::App,
) -> gpui::AnyElement {
    let (selected, selection_bg, line_height, stageable) = (
        style.selected,
        style.selection_bg,
        style.line_height,
        style.stageable,
    );
    let header = diff.headers.get(hunk).cloned().unwrap_or_default();
    let for_copy = entity.clone();
    let for_stage = entity.clone();
    let row = h_flex()
        .id(("hunk", index))
        .h(line_height)
        .min_w(content_width)
        .px_2()
        .gap_2()
        .items_center()
        .whitespace_nowrap()
        .bg(if selected {
            selection_bg
        } else {
            colors.hunk_bg
        })
        .child(div().text_color(cx.theme().muted_foreground).child(header))
        .child(
            Button::new(("copy-hunk", index))
                .ghost()
                .xsmall()
                .icon(icon("copy"))
                .tooltip(tr!("action-copy-hunk"))
                .on_click(move |_, _window, cx| {
                    for_copy.update(cx, |this, cx| this.copy_hunk(hunk, false, cx));
                }),
        )
        // Staging a hunk on its own only makes sense from the unstaged changes:
        // elsewhere, either everything is already in the index, or one is
        // looking at commits already written.
        .when(stageable, |el| {
            el.child(
                Button::new(("stage-hunk", index))
                    .ghost()
                    .xsmall()
                    .icon(icon("plus"))
                    .tooltip(tr!("action-stage-hunk"))
                    .on_click(move |_, _window, cx| {
                        for_stage.update(cx, |this, cx| this.stage_hunk(hunk, cx));
                    }),
            )
        });
    style
        .hunk_rule(with_row_gestures(row, index, entity))
        .into_any_element()
}

fn line_colors(
    kind: DiffLineKind,
    colors: &DiffColors,
) -> (Option<gpui::Hsla>, Option<gpui::Hsla>) {
    match kind {
        DiffLineKind::Added => (Some(colors.added_bg), Some(colors.added_fg)),
        DiffLineKind::Removed => (Some(colors.removed_bg), Some(colors.removed_fg)),
        DiffLineKind::Context | DiffLineKind::NoNewline => (None, None),
    }
}

/// The background of a changed word, on the side that carries it.
fn word_color(kind: DiffLineKind, colors: &DiffColors) -> Option<gpui::Hsla> {
    match kind {
        DiffLineKind::Added => Some(colors.added_word_bg),
        DiffLineKind::Removed => Some(colors.removed_word_bg),
        DiffLineKind::Context | DiffLineKind::NoNewline => None,
    }
}

/// A line's text, coloured if it is.
///
/// Tabs are rendered as they are by the font: replacing them here would keep the
/// alignment but shift the highlighting ranges, which are computed on the
/// original text.
#[allow(clippy::too_many_arguments)]
fn line_content(
    diff: &Rc<Rendered>,
    hunk: usize,
    line: usize,
    fg: Option<gpui::Hsla>,
    // `word_bg`: the background of the words that changed inside this line,
    // on the side that carries them. `None` on a context line, which has no
    // other version to differ from.
    word_bg: Option<gpui::Hsla>,
    marks: &[(std::ops::Range<usize>, gpui::Hsla)],
    // `span`: the slice of the text to show, in **bytes**, when the line is
    // wrapped. Its ends are computed once per line by `wrap_offsets`, where
    // taking them a column count at a time walked the text once per segment.
    span: Option<std::ops::Range<usize>>,
    // `follow`: what makes the words clickable. `None` while nobody holds the
    // modifier, which is all the time: no ranges are computed, and the text is
    // the plain element it has always been.
    follow: Option<Armed>,
) -> gpui::AnyElement {
    let Some(source) = diff.file.hunks.get(hunk).and_then(|h| h.lines.get(line)) else {
        return div().into_any_element();
    };
    let words: Vec<(std::ops::Range<usize>, gpui::Hsla)> = match word_bg {
        Some(bg) => diff
            .word_ranges(hunk, line)
            .iter()
            .map(|range| (range.clone(), bg))
            .collect(),
        None => Vec::new(),
    };
    // The whole line is borrowed — its text is an `Arc` clone and its runs stay
    // where they are; only a wrapped segment owns anything.
    let sliced;
    let (text, styles, words, marks) = match span {
        None => (
            diff.line_text(hunk, line).cloned().unwrap_or_default(),
            diff.highlights.line(hunk, line),
            words.as_slice(),
            marks,
        ),
        Some(bytes) => {
            sliced = (
                slice_runs(diff.highlights.line(hunk, line), &bytes),
                slice_runs(&words, &bytes),
                slice_runs(marks, &bytes),
            );
            (
                SharedString::from(source.text[bytes].to_string()),
                sliced.0.as_slice(),
                sliced.1.as_slice(),
                sliced.2.as_slice(),
            )
        }
    };
    // Four layers, in the order they were decided: the grammar, the changed
    // words of the pair, the search hits, and the underline of the word being
    // pointed at. Each is laid on the one below rather than replacing it — a
    // hit in coloured code keeps its colours, and so does an underlined
    // symbol.
    let base = if words.is_empty() {
        styles.to_vec()
    } else {
        crate::ui::highlight::overlay(styles, words)
    };
    let base = if marks.is_empty() {
        base
    } else {
        crate::ui::highlight::overlay(&base, marks)
    };
    let highlights = match follow.as_ref().and_then(|follow| follow.hovered.clone()) {
        Some(word) => crate::ui::highlight::underline(&base, word),
        None => base,
    };
    // Nothing to colour and nobody holding the modifier — which is the common
    // case by far: the line is the plain element it has always been, painted by
    // the code that painted it before, at the same cost.
    if highlights.is_empty() && follow.is_none() {
        return div()
            .when_some(fg, |el, fg| el.text_color(fg))
            .child(text)
            .into_any_element();
    }
    let styled = StyledText::new(text.clone()).with_highlights(highlights);
    let content = match follow {
        None => styled.into_any_element(),
        Some(follow) => followable(follow, text, styled),
    };
    if !styles.is_empty() {
        return content;
    }
    // The addition or removal colour stays carried by the container when the
    // grammar has nothing to say: without it, a line found in a file with no
    // grammar would lose its diff tint.
    div()
        .when_some(fg, |el, fg| el.text_color(fg))
        .child(content)
        .into_any_element()
}

/// One entry of the two-column view.
#[allow(clippy::too_many_arguments)]
fn render_split_row(
    diff: &Rc<Rendered>,
    index: usize,
    colors: &DiffColors,
    column: Pixels,
    cols: usize,
    style: &RowStyle,
    search: &SearchPaint,
    entity: &Entity<ClaudhubApp>,
    cx: &mut gpui::App,
) -> gpui::AnyElement {
    let Some(row) = diff.split.get(index).copied() else {
        return div().into_any_element();
    };
    let (old, new) = match row {
        SplitRow::Header { hunk, .. } => {
            return render_header(diff, index, hunk, colors, column * 2., style, entity, cx)
        }
        SplitRow::Pair { old, new } => (old, new),
    };

    // The entry's height is that of the taller of its two halves: it is what was
    // announced to the list, which reserves exactly what it is told.
    let lines = if cols == 0 {
        1
    } else {
        [old, new]
            .into_iter()
            .flatten()
            .map(|index| wrapped_lines(diff.row_chars.get(index).copied().unwrap_or(0), cols))
            .max()
            .unwrap_or(1)
    };
    let row = with_row_gestures(
        h_flex()
            .id(("pair", index))
            .h(style.line_height * lines as f32)
            .items_start()
            .whitespace_nowrap(),
        index,
        entity,
    )
    .child(note_mark(style))
    .child(half(
        diff,
        old,
        Column::Old,
        colors,
        style,
        column,
        cols,
        lines,
        search,
        index,
        entity,
    ))
    .child(half(
        diff,
        new,
        Column::New,
        colors,
        style,
        column,
        cols,
        lines,
        search,
        index,
        entity,
    ));
    style.hunk_rule(row).into_any_element()
}

/// The margin rule of an annotated row.
///
/// Always present, coloured only when there is a note: a width that appears and
/// disappears would shift a row's whole content from one line to the next.
fn note_mark(style: &RowStyle) -> impl IntoElement {
    div()
        .w(px(3.))
        .flex_none()
        .h_full()
        .when(style.annotated, |el| el.bg(style.note_color))
}

/// Half a row: its number, its sign and its text.
///
/// With no line to show — an addition has nothing opposite — the half stays
/// empty and greyed: that is what makes it visible that the change has no
/// counterpart on that side.
/// Which of the two versions a column shows.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Column {
    Old,
    New,
}

#[allow(clippy::too_many_arguments)]
fn half(
    diff: &Rc<Rendered>,
    row: Option<usize>,
    side: Column,
    colors: &DiffColors,
    style: &RowStyle,
    column: Pixels,
    // `cols`: text columns before wrapping, zero when the line does not wrap and
    // horizontal scrolling takes care of it.
    cols: usize,
    // `lines`: the entry's visible lines, the taller of the two halves.
    lines: usize,
    search: &SearchPaint,
    // `index`: the entry of the list, which names its clickable words.
    index: usize,
    entity: &Entity<ClaudhubApp>,
) -> gpui::AnyElement {
    let (gutter, selected, selection_bg) = (style.gutter, style.selected, style.selection_bg);
    let source = row
        .and_then(|index| diff.rows.get(index).copied())
        .and_then(|row| match row {
            Row::Line { hunk, line } => Some((hunk, line)),
            Row::Header { .. } => None,
        })
        .and_then(|(hunk, line)| {
            let source = diff.file.hunks.get(hunk)?.lines.get(line)?;
            Some((hunk, line, source))
        });

    let Some((hunk, line, source)) = source else {
        return div()
            .w(column)
            .flex_none()
            .h_full()
            .bg(if selected {
                selection_bg
            } else {
                colors.absent_bg
            })
            .into_any_element();
    };

    let (bg, fg) = line_colors(source.kind, colors);
    let word_bg = word_color(source.kind, colors);
    // *This* version's number: a context line has two, and showing the same one
    // on both sides would make the left column lie as soon as the file has
    // gained or lost lines above.
    let number_of = match side {
        Column::Old => source.old_no.or(source.new_no),
        Column::New => source.new_no.or(source.old_no),
    };
    let kind = source.kind;
    let marks = search.marks(hunk, line);
    let line_height = style.line_height;
    // Wrapped, the half becomes a stack of fixed-height lines: the gutter and
    // the sign stay on the first, aligned to the top, and the following ones are
    // the continuation of the text. The entry's height is then exactly the one
    // announced to the list.
    let wrapped = cols > 0;
    let follow = |segment: usize| {
        style.armed.then(|| Armed {
            id: ("diff-word", index).into(),
            spot: Spot::Diff {
                row: index,
                side: side as u8,
                segment,
            },
            hovered: style.hovered_word(index, side as u8, segment),
            entity,
        })
    };
    h_flex()
        // An id of its own, and it is not decoration: an element's identity is
        // the path of the ids above it, and the two halves would otherwise give
        // the same name to two different lines' words.
        .id(("half", side as usize))
        .w(column)
        .flex_none()
        .h_full()
        .map(|el| {
            if wrapped {
                el.items_start()
            } else {
                el.items_center()
            }
        })
        .whitespace_nowrap()
        .overflow_hidden()
        .when_some(bg.filter(|_| !selected), |el, bg| el.bg(bg))
        .when(selected, |el| el.bg(selection_bg))
        // One gutter per column: each shows its own version, and repeating both
        // numbers there would pay twice the width for information the column
        // opposite already carries.
        .child(number(number_of, gutter, colors).when(wrapped, |el| el.h(line_height)))
        .child(
            div()
                .w(px(14.))
                .flex_none()
                .text_center()
                .when(wrapped, |el| el.h(line_height))
                .when_some(fg, |el, fg| el.text_color(fg))
                .child(sign(kind)),
        )
        .map(|el| {
            if !wrapped {
                return el.child(line_content(
                    diff,
                    hunk,
                    line,
                    fg,
                    word_bg,
                    &marks,
                    None,
                    follow(0),
                ));
            }
            // The entry's index, which `row_chars` indexes: finding it by
            // walking `rows` would cost a sweep of the file per visible half
            // line, on every frame.
            let chars = row
                .and_then(|index| diff.row_chars.get(index).copied())
                .unwrap_or(0);
            let own = wrapped_lines(chars, cols);
            let bounds = wrap_offsets(&source.text, cols, own);
            el.child(
                v_flex()
                    .flex_1()
                    .min_w_0()
                    .children((0..lines).map(|segment| {
                        // Same reason as the half's own id: each wrapped
                        // segment is a line of its own, and its words with it.
                        div().id(("segment", segment)).h(line_height).map(|el| {
                            if segment < own {
                                el.child(line_content(
                                    diff,
                                    hunk,
                                    line,
                                    fg,
                                    word_bg,
                                    &marks,
                                    Some(bounds[segment]..bounds[segment + 1]),
                                    follow(segment),
                                ))
                            } else {
                                el
                            }
                        })
                    })),
            )
        })
        .into_any_element()
}

/// Where the selection goes after an arrow.
///
/// With no selection, the first arrow starts from the end it points at:
/// downwards from the first line, upwards from the last. At the edges it stops
/// rather than wrap — going past the end of a file to come back to its start is
/// never what was meant.
fn step(current: Option<usize>, delta: isize, len: usize) -> Option<usize> {
    if len == 0 {
        return None;
    }
    let last = len as isize - 1;
    let next = match current {
        Some(index) => index as isize + delta,
        None if delta > 0 => 0,
        None => last,
    };
    Some(next.clamp(0, last) as usize)
}

/// The next or previous hunk header, from a position.
fn next_header(headers: &[usize], from: Option<usize>, delta: isize) -> Option<usize> {
    match from {
        // Strictly beyond: otherwise, starting from a header would stay on it.
        Some(index) if delta > 0 => headers.iter().find(|h| **h > index).copied(),
        Some(index) => headers.iter().rev().find(|h| **h < index).copied(),
        None if delta > 0 => headers.first().copied(),
        None => headers.last().copied(),
    }
}

/// Selects a line **and takes the focus**.
///
/// The second point is not a detail: without it, clicking a line leaves the
/// focus with the terminal, and the `Ctrl+C` that follows goes to the program
/// running there instead of copying what has just been selected.
fn select(
    entity: &Entity<ClaudhubApp>,
    index: usize,
    extend: bool,
    window: &mut Window,
    cx: &mut gpui::App,
) {
    let handle = entity.read(cx).focus_handle(cx);
    window.focus(&handle, cx);
    entity.update(cx, |this, cx| {
        this.diff_dragging = true;
        this.select_diff_row(index, extend, cx);
    });
}

/// The right click's press: it puts the selection where the eye is, and no drag
/// starts — the menu opens on the release, and a button that is not held cannot
/// extend anything.
fn aim(entity: &Entity<ClaudhubApp>, index: usize, window: &mut Window, cx: &mut gpui::App) {
    let handle = entity.read(cx).focus_handle(cx);
    window.focus(&handle, cx);
    entity.update(cx, |this, cx| this.aim_diff_row(index, cx));
}

/// Extends the selection as the mouse passes, button held.
///
/// The button is rechecked here and not only on the press: a release outside the
/// window sends no event, and without this condition the selection would go on
/// following the cursor afterwards.
fn drag(
    entity: &Entity<ClaudhubApp>,
    index: usize,
    event: &gpui::MouseMoveEvent,
    cx: &mut gpui::App,
) {
    entity.update(cx, |this, cx| {
        // The pointer is on **this** entry, so a word underlined on another one
        // is no longer under it. The words of this entry have already had their
        // say: a text is a child of its row, and a child is dispatched first.
        this.leave_row(Spot::diff_row(index), cx);
        if event.pressed_button != Some(gpui::MouseButton::Left) {
            this.end_diff_drag();
            return;
        }
        this.drag_diff_row(index, cx);
    });
}

fn number(value: Option<usize>, width: Pixels, colors: &DiffColors) -> gpui::Div {
    div()
        .w(width)
        .flex_none()
        .text_right()
        .pr_1()
        .text_color(colors.line_number)
        .child(value.map(|n| n.to_string()).unwrap_or_default())
}

/// The diff view's empty state.
///
/// An icon and a word in the centre rather than a grey sentence at the top left:
/// an empty panel with no visual cue reads as a broken panel, above all on first
/// launch where it is the first thing one sees.
fn centered_message(text: SharedString, cx: &mut gpui::App) -> gpui::AnyElement {
    v_flex()
        .size_full()
        .items_center()
        .justify_center()
        .gap_2()
        .child(
            icon("file-diff")
                .large()
                .text_color(cx.theme().muted_foreground.opacity(0.4)),
        )
        .child(
            div()
                .text_sm()
                .text_color(cx.theme().muted_foreground)
                .child(text),
        )
        .into_any_element()
}

pub(super) fn hint(text: SharedString, cx: &mut gpui::App) -> impl IntoElement {
    div()
        .p_3()
        .text_sm()
        .text_color(cx.theme().muted_foreground)
        .child(text)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::git::{DiffLine, Hunk};
    use gpui_component::highlighter::HighlightTheme as Theme;

    fn hunk(header: &str, kinds: &[DiffLineKind]) -> Hunk {
        Hunk {
            header: header.into(),
            old_start: 1,
            new_start: 1,
            lines: kinds
                .iter()
                .map(|kind| DiffLine {
                    kind: *kind,
                    old_no: Some(1),
                    new_no: Some(1),
                    text: "x".into(),
                })
                .collect(),
        }
    }

    #[test]
    fn flattens_headers_and_lines_in_order() {
        let diff = FileDiff {
            hunks: vec![
                hunk("@@ a @@", &[DiffLineKind::Context, DiffLineKind::Added]),
                hunk("@@ b @@", &[DiffLineKind::Removed]),
            ],
            binary: false,
            empty: false,
        };
        assert_eq!(
            rows(&diff),
            vec![
                Row::Header { hunk: 0 },
                Row::Line { hunk: 0, line: 0 },
                Row::Line { hunk: 0, line: 1 },
                Row::Header { hunk: 1 },
                Row::Line { hunk: 1, line: 0 },
            ]
        );
    }

    /// Asked for the whole file, git hands back one hunk: what the eye calls a
    /// change is each red-and-green block, and that is what `j`/`k` must stop
    /// on and what the gutter must mark — not the file.
    #[test]
    fn the_whole_file_view_reads_changes_and_not_the_one_hunk() {
        use DiffLineKind::*;
        let diff = FileDiff {
            hunks: vec![hunk(
                "@@ -1,9 +1,9 @@",
                &[
                    Context, Context, Removed, Added, Context, Context, Added, Added, Context,
                    NoNewline,
                ],
            )],
            binary: false,
            empty: false,
        };
        let rendered = Rendered::new(Path::new("a.rs"), diff, &Theme::default_light());
        // Unified: rows 0 is the header, line `n` is row `n + 1`.
        assert_eq!(rendered.changes, vec![(3, 4), (7, 8)]);
        assert_eq!(rendered.blocks(false, true), vec![(3, 4), (7, 8)]);
        // The one hunk, read as git cut it, is still the file.
        assert_eq!(rendered.blocks(false, false), vec![(0, 10)]);
        assert_eq!(rendered.change_of(3, false), Some(0));
        assert_eq!(rendered.change_of(5, false), None);
        assert_eq!(rendered.change_of(8, false), Some(1));
        assert_eq!(rendered.block_of(8, false, true), Some(1));
        assert_eq!(rendered.block_of(8, false, false), Some(0));
        // The stops are what `j` walks: from the first change, `j` reaches the
        // second, and a third press has nowhere to go in this file.
        let stops: Vec<usize> = rendered
            .blocks(false, true)
            .into_iter()
            .map(|(s, _)| s)
            .collect();
        assert_eq!(next_header(&stops, None, 1), Some(3));
        assert_eq!(next_header(&stops, Some(3), 1), Some(7));
        assert_eq!(next_header(&stops, Some(7), 1), None);
        // Two columns: the removal and the addition share one entry, so the
        // first change is one entry long, and the second still two.
        let paired = rendered.blocks(true, true);
        assert_eq!(paired.len(), 2);
        assert_eq!(paired[0].0, paired[0].1);
        assert_eq!(paired[1].1 - paired[1].0, 1);
        assert_eq!(rendered.change_of(paired[1].0, true), Some(1));
    }

    /// A brand-new or deleted file carries a single version; anything with a
    /// context line, or with both kinds, has two.
    #[test]
    fn a_single_version_file_reads_as_one_sided() {
        use DiffLineKind::*;
        let of = |kinds: &[DiffLineKind]| FileDiff {
            hunks: vec![hunk("@@ a @@", kinds)],
            binary: false,
            empty: false,
        };
        assert!(one_sided(&of(&[Added, Added, NoNewline])));
        assert!(one_sided(&of(&[Removed])));
        assert!(!one_sided(&of(&[Context, Added])));
        assert!(!one_sided(&of(&[Removed, Added])));
        assert!(!one_sided(&of(&[NoNewline])));
    }

    /// The words that changed inside a paired line reach `Rendered`: the
    /// removal facing an addition carries the ranges of what differs, on both
    /// sides, and the lines around them carry none.
    #[test]
    fn a_paired_line_carries_its_changed_words() {
        use DiffLineKind::*;
        let mut one = hunk("@@ a @@", &[Context, Removed, Added, Added]);
        one.lines[0].text = "let total = 0;".into();
        one.lines[1].text = "foo(alpha, beta)".into();
        one.lines[2].text = "foo(alpha, gamma)".into();
        one.lines[3].text = "bar()".into();
        let diff = FileDiff {
            hunks: vec![one],
            binary: false,
            empty: false,
        };
        let rendered = Rendered::new(Path::new("a.rs"), diff, &Theme::default_light());
        assert!(rendered.word_ranges(0, 0).is_empty());
        let removed = rendered.word_ranges(0, 1);
        assert_eq!(removed.len(), 1);
        assert_eq!(&"foo(alpha, beta)"[removed[0].clone()], "beta");
        let added = rendered.word_ranges(0, 2);
        assert_eq!(&"foo(alpha, gamma)"[added[0].clone()], "gamma");
        // The addition with nothing opposite has no other version to differ
        // from.
        assert!(rendered.word_ranges(0, 3).is_empty());
    }

    /// A trailing "no newline" marker belongs to the change before it, and a
    /// change never crosses a hunk header.
    #[test]
    fn a_change_keeps_its_marker_and_stops_at_a_header() {
        use DiffLineKind::*;
        let diff = FileDiff {
            hunks: vec![
                hunk("@@ a @@", &[Context, Added, NoNewline]),
                hunk("@@ b @@", &[Removed, Context, NoNewline]),
            ],
            binary: false,
            empty: false,
        };
        let rows = rows(&diff);
        assert_eq!(changes(&diff, &rows), vec![(2, 3), (5, 5)]);
    }

    /// The pairing is the whole column view: a block of removals followed by a
    /// block of additions puts the two versions back opposite each other.
    #[test]
    fn the_two_columns_face_each_other() {
        let diff = FileDiff {
            hunks: vec![hunk(
                "@@ a @@",
                &[
                    DiffLineKind::Context,
                    DiffLineKind::Removed,
                    DiffLineKind::Removed,
                    DiffLineKind::Added,
                    DiffLineKind::Context,
                ],
            )],
            binary: false,
            empty: false,
        };
        let rows = rows(&diff);
        assert_eq!(
            split_rows(&diff, &rows),
            vec![
                SplitRow::Header { hunk: 0, row: 0 },
                SplitRow::Pair {
                    old: Some(1),
                    new: Some(1)
                },
                SplitRow::Pair {
                    old: Some(2),
                    new: Some(4)
                },
                // Two removals for a single addition: the second has nothing
                // opposite, and the right-hand slot stays empty.
                SplitRow::Pair {
                    old: Some(3),
                    new: None
                },
                SplitRow::Pair {
                    old: Some(5),
                    new: Some(5)
                },
            ]
        );
    }

    #[test]
    fn a_column_selection_comes_back_to_the_file_order() {
        let diff = FileDiff {
            hunks: vec![hunk(
                "@@ a @@",
                &[
                    DiffLineKind::Removed,
                    DiffLineKind::Added,
                    DiffLineKind::Context,
                ],
            )],
            binary: false,
            empty: false,
        };
        let rendered = Rendered::new(Path::new("x.txt"), diff, &Theme::default_dark());
        // The (removal, addition) pair covers two rows of the unified list:
        // copying them has to return both, in git's order.
        assert_eq!(rendered.unified_span(1, 1), Some((1, 2)));
        assert_eq!(rendered.unified_span(0, 2), Some((0, 3)));
        assert_eq!(rendered.headers(true), vec![0]);
        assert_eq!(rendered.headers(false), vec![0]);
        assert_eq!(rendered.len(false), 4);
        assert_eq!(
            rendered.len(true),
            3,
            "the addition and the removal sit on one entry"
        );
    }

    /// The rule down the side says which hunk one is reading: every entry of
    /// that hunk carries it, in both layouts, and a paired entry answers by
    /// whichever half it has.
    #[test]
    fn every_entry_names_the_hunk_it_belongs_to() {
        let diff = FileDiff {
            hunks: vec![
                hunk("@@ a @@", &[DiffLineKind::Context, DiffLineKind::Removed]),
                hunk("@@ b @@", &[DiffLineKind::Added]),
            ],
            binary: false,
            empty: false,
        };
        let rendered = Rendered::new(Path::new("x.txt"), diff, &Theme::default_dark());
        let unified: Vec<Option<usize>> = (0..rendered.len(false))
            .map(|ix| rendered.hunk_of(ix, false))
            .collect();
        assert_eq!(
            unified,
            vec![Some(0), Some(0), Some(0), Some(1), Some(1)],
            "header then lines, hunk by hunk"
        );
        let split: Vec<Option<usize>> = (0..rendered.len(true))
            .map(|ix| rendered.hunk_of(ix, true))
            .collect();
        assert_eq!(split, vec![Some(0), Some(0), Some(0), Some(1), Some(1)]);
        // Past the end there is no hunk, and no rule to paint.
        assert_eq!(rendered.hunk_of(rendered.len(false), false), None);
    }

    #[test]
    fn arrows_stop_at_the_edges() {
        assert_eq!(step(Some(3), 1, 10), Some(4));
        assert_eq!(step(Some(0), -1, 10), Some(0), "stops at the top");
        assert_eq!(step(Some(9), 1, 10), Some(9), "stops at the bottom");
        // With no selection, the arrow starts from the end it goes towards.
        assert_eq!(step(None, 1, 10), Some(0));
        assert_eq!(step(None, -1, 10), Some(9));
        assert_eq!(step(Some(0), 1, 0), None, "nothing to walk");
    }

    /// `None` is not a refusal: it is the signal that there is no hunk left in
    /// this file, and therefore that one has to move to the neighbour.
    #[test]
    fn hunk_jumps_never_stay_put() {
        let headers = [0usize, 12, 40];
        assert_eq!(next_header(&headers, Some(0), 1), Some(12));
        assert_eq!(next_header(&headers, Some(13), 1), Some(40));
        assert_eq!(
            next_header(&headers, Some(40), 1),
            None,
            "after the last hunk, we change file"
        );
        assert_eq!(next_header(&headers, Some(13), -1), Some(12));
        assert_eq!(next_header(&headers, Some(12), -1), Some(0));
        assert_eq!(
            next_header(&headers, Some(0), -1),
            None,
            "et avant le premier"
        );
        assert_eq!(next_header(&[], None, 1), None, "un fichier sans hunk");
        assert_eq!(next_header(&headers, None, 1), Some(0));
        assert_eq!(next_header(&headers, None, -1), Some(40));
    }

    #[test]
    fn copying_yields_code_and_not_a_patch() {
        let mut diff = FileDiff::default();
        diff.hunks.push(hunk(
            "@@ -1,3 +1,3 @@",
            &[
                DiffLineKind::Context,
                DiffLineKind::Removed,
                DiffLineKind::Added,
                DiffLineKind::NoNewline,
            ],
        ));
        for (ix, text) in ["keep", "before", "after", "\\ No newline"]
            .iter()
            .enumerate()
        {
            diff.hunks[0].lines[ix].text = (*text).into();
        }
        let rendered = Rendered::new(Path::new("a.rs"), diff, &HighlightTheme::default_dark());

        // The whole file, as code: no `@@` header, no signs, and no end-of-file
        // annotation — it is what one pastes into an editor.
        let all = rendered.copy_text(0, rendered.rows.len() - 1, false);
        assert_eq!(all, "keep\nbefore\nafter\n");

        // The same range as a patch keeps what is needed to apply it.
        let patch = rendered.copy_text(0, rendered.rows.len() - 1, true);
        assert_eq!(
            patch,
            "@@ -1,3 +1,3 @@\n keep\n-before\n+after\n\\ No newline\n"
        );

        // A range taken backwards is worth the same: one sometimes selects from
        // the bottom up.
        assert_eq!(
            rendered.copy_text(3, 1, false),
            rendered.copy_text(1, 3, false)
        );
    }

    #[test]
    fn a_hunk_knows_where_it_begins_and_ends() {
        let mut diff = FileDiff::default();
        diff.hunks
            .push(hunk("@@ -1 +1 @@", &[DiffLineKind::Context]));
        diff.hunks.push(hunk(
            "@@ -9 +9 @@",
            &[DiffLineKind::Added, DiffLineKind::Added],
        ));
        let rendered = Rendered::new(Path::new("a.rs"), diff, &HighlightTheme::default_dark());
        // Two entries for the first hunk, three for the second.
        assert_eq!(rendered.hunk_bounds(0), Some((0, 1)));
        assert_eq!(rendered.hunk_bounds(1), Some((2, 4)));
        assert_eq!(rendered.hunk_bounds(2), None);
    }

    #[test]
    fn an_empty_diff_has_no_rows() {
        let diff = FileDiff::default();
        assert!(rows(&diff).is_empty());
        // The gutter keeps a usable width even with no line.
        assert_eq!(gutter_digits(&diff), 1);
    }

    /// The height announced to the list has to land exactly: it reserves exactly
    /// what it is told, and one line too many covers the next.
    #[test]
    fn a_long_line_takes_as_many_lines_as_it_needs() {
        assert_eq!(wrapped_lines(0, 80), 1, "une ligne vide occupe sa ligne");
        assert_eq!(wrapped_lines(80, 80), 1);
        assert_eq!(wrapped_lines(81, 80), 2);
        assert_eq!(wrapped_lines(240, 80), 3);
        // With no known column — the view has not been painted yet — nothing
        // wraps rather than divide by zero.
        assert_eq!(wrapped_lines(240, 0), 1);
    }

    /// A pair is as tall as the taller of its two halves: both versions have to
    /// stay opposite each other, which is the whole point of this view.
    #[test]
    fn a_pair_is_as_tall_as_its_tallest_half() {
        let mut hunk = hunk("@@ a @@", &[DiffLineKind::Removed, DiffLineKind::Added]);
        hunk.lines[0].text = "x".repeat(10);
        hunk.lines[1].text = "x".repeat(45);
        let diff = FileDiff {
            hunks: vec![hunk],
            binary: false,
            empty: false,
        };
        let rendered = Rendered::new(Path::new("x.txt"), diff, &Theme::default_dark());
        // The header, then the pair: the removal fits on one line, its addition
        // needs three, and the whole is three tall.
        assert_eq!(split_heights(&rendered, 20), vec![1, 3]);
        assert_eq!(
            split_heights(&rendered, 0),
            vec![1, 1],
            "sans repli, une ligne"
        );
    }

    /// The unified list has one entry per version, so an entry is as tall as
    /// its own line and nothing else — where the two-column pair had to be as
    /// tall as its taller half. A file added is read here, and it is the list
    /// the setting used to leave unwrapped.
    #[test]
    fn a_unified_entry_is_as_tall_as_its_own_line() {
        let mut hunk = hunk("@@ a @@", &[DiffLineKind::Removed, DiffLineKind::Added]);
        hunk.lines[0].text = "x".repeat(10);
        hunk.lines[1].text = "x".repeat(45);
        let diff = FileDiff {
            hunks: vec![hunk],
            binary: false,
            empty: false,
        };
        let rendered = Rendered::new(Path::new("x.txt"), diff, &Theme::default_dark());
        // The header, the short line, then the long one on three lines — where
        // the pair made a single three-line entry of the two.
        assert_eq!(unified_heights(&rendered, 20), vec![1, 1, 3]);
        assert_eq!(
            unified_heights(&rendered, 0),
            vec![1, 1, 1],
            "sans repli, une ligne"
        );
    }

    /// A wrap counts **characters**: in bytes, an accented line would be cut one
    /// column too early, and in the middle of a character — which panics.
    #[test]
    fn a_wrap_counts_characters_and_not_bytes() {
        let text = "éàü1234";
        let cuts = wrap_offsets(text, 3, 3);
        assert_eq!(cuts, vec![0, 6, 9, 10]);
        assert_eq!(&text[cuts[0]..cuts[1]], "éàü");
        assert_eq!(&text[cuts[1]..cuts[2]], "123");
        assert_eq!(&text[cuts[2]..cuts[3]], "4");

        // A segment past the end of the text is empty, not out of bounds: the
        // half opposite decides how many lines the row has.
        let cuts = wrap_offsets(text, 3, 5);
        assert_eq!(&text[cuts[3]..cuts[4]], "");
        assert_eq!(cuts[5], text.len());

        // Nothing to wrap: the whole line, in one piece.
        assert_eq!(wrap_offsets(text, 0, 1), vec![0, text.len()]);
        assert_eq!(wrap_offsets("", 3, 1), vec![0, 0]);
    }

    /// A slice's ranges stay sorted and disjoint, and start again from zero: it
    /// is the invariant gpui does not check.
    #[test]
    fn sliced_runs_are_moved_back_to_the_start() {
        let runs = vec![(0..4, 'a'), (6..10, 'b'), (12..20, 'c')];
        assert_eq!(slice_runs(&runs, &(5..14)), vec![(1..5, 'b'), (7..9, 'c')]);
        assert!(slice_runs(&runs, &(4..6)).is_empty(), "nothing straddling");
        assert_eq!(slice_runs(&runs, &(0..2)), vec![(0..2, 'a')]);
    }

    #[test]
    fn the_longest_row_is_found_across_hunks_and_headers() {
        let mut diff = FileDiff {
            hunks: vec![
                hunk("@@ court @@", &[DiffLineKind::Context]),
                hunk("@@ b @@", &[DiffLineKind::Added]),
            ],
            binary: false,
            empty: false,
        };
        diff.hunks[1].lines[0].text = "une ligne nettement plus longue que les autres".into();
        let rendered = Rendered::new(Path::new("x.txt"), diff, &Theme::default_dark());

        assert_eq!(rendered.longest_chars, 46);
        assert_eq!(
            rendered.rows[rendered.longest_row],
            Row::Line { hunk: 1, line: 0 }
        );
    }

    #[test]
    fn highlight_runs_stay_sorted_and_disjoint() {
        // gpui converts the ranges into *lengths* of consecutive runs, walking
        // them in the order given: a range out of order or overlapping another
        // silently shifts everything after it.
        let mut diff = FileDiff {
            hunks: vec![hunk(
                "@@ a @@",
                &[DiffLineKind::Added, DiffLineKind::Context],
            )],
            binary: false,
            empty: false,
        };
        diff.hunks[0].lines[0].text = "fn calcule(x: u32) -> u32 { x + 1 }".into();
        diff.hunks[0].lines[1].text = "// a comment with accents: é à ù".into();
        let rendered = Rendered::new(Path::new("src/x.rs"), diff, &Theme::default_dark());

        for line in 0..2 {
            let text = rendered.row_text(Row::Line { hunk: 0, line });
            let mut end = 0usize;
            for (range, _) in rendered.highlights.line(0, line) {
                assert!(
                    range.start >= end,
                    "ranges not sorted: {range:?} after {end}"
                );
                assert!(range.start <= range.end, "reversed range: {range:?}");
                assert!(range.end <= text.len(), "range outside the text: {range:?}");
                assert!(
                    text.is_char_boundary(range.start) && text.is_char_boundary(range.end),
                    "range {range:?} cuts a character of \"{text}\""
                );
                end = range.end;
            }
        }
    }

    #[test]
    fn the_gutter_is_sized_on_the_largest_number() {
        let mut diff = FileDiff {
            hunks: vec![hunk("@@ a @@", &[DiffLineKind::Context])],
            binary: false,
            empty: false,
        };
        diff.hunks[0].lines[0].new_no = Some(1024);
        diff.hunks[0].lines[0].old_no = Some(9);
        assert_eq!(gutter_digits(&diff), 4);
    }
}
