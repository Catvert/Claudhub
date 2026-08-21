//! Review notes: what there is to say about a piece of code, and what is needed
//! to send it back to the agent that wrote it.
//!
//! ## Why a note does not attach to the selection
//!
//! `ReviewState::diff_selection` is a pair of indices into the **displayed**
//! list. It is invalidated by the unified/two-column switch, by a context
//! change, and by any reload of the diff — that is, by every file write in the
//! worktree. A note attached to it would point at something else a few seconds
//! later.
//!
//! We therefore keep **line numbers** — which git already gives — and the
//! **code excerpt** itself. The numbers relocate the note in the common case;
//! the excerpt catches it when the file has moved under it. And if both fail,
//! the note is called *drifted* and **stays in the list**: a note lost in
//! silence is worse than no note at all.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::git::{DiffLineKind, DiffRange};
use crate::ui::diff_view::{Rendered, Row};

/// Which version of the file a note is about.
///
/// Commenting on removed code makes sense — "why was that taken out?" is as
/// legitimate a review remark as any — and a removed line has no number in the
/// new version.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Side {
    Old,
    New,
}

/// A remark taken on a range of lines.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Note {
    pub id: u64,
    /// The range the note was taken in. A remark about what the branch wrote is
    /// not read back in the changes in progress.
    pub range: DiffRange,
    pub path: PathBuf,
    pub side: Side,
    /// Line numbers, **never** list indices: those do not survive a reload of
    /// the diff.
    pub start: usize,
    pub end: usize,
    /// The quoted code, as `Rendered::copy_text` returns it — no `+`/`-`, no
    /// numbers, no `@@` header. It is what makes it possible to find the note
    /// again when the numbers have moved, and what we quote in the prompt.
    pub excerpt: String,
    /// The remark itself.
    pub body: String,
    /// Sent to the agent. That is not the same as handled: it is the review of
    /// the answer that closes a note.
    pub sent: bool,
    pub done: bool,
}

impl Default for Note {
    fn default() -> Self {
        Self {
            id: 0,
            range: DiffRange::Working,
            path: PathBuf::new(),
            side: Side::New,
            start: 0,
            end: 0,
            excerpt: String::new(),
            body: String::new(),
            sent: false,
            done: false,
        }
    }
}

impl Note {
    /// `path:start-end`, the form everybody can read — and an agent can open.
    pub fn location(&self) -> String {
        if self.start == self.end {
            format!("{}:{}", self.path.display(), self.start)
        } else {
            format!("{}:{}-{}", self.path.display(), self.start, self.end)
        }
    }
}

/// Where a note goes back in the displayed diff.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Anchor {
    /// At the recorded numbers, and the text agrees: the common case.
    At { from: usize, to: usize },
    /// The file has moved, but the excerpt is found elsewhere.
    Moved { from: usize, to: usize },
    /// Neither numbers nor excerpt: the note stays, marked drifted.
    Drifted,
}

impl Anchor {
    pub fn rows(self) -> Option<(usize, usize)> {
        match self {
            Self::At { from, to } | Self::Moved { from, to } => Some((from, to)),
            Self::Drifted => None,
        }
    }
}

/// Puts a note back into a diff that has just arrived.
///
/// The indices returned are those of the **unified** list (`Rendered::rows`),
/// which alone carries the file's order; the two-column view maps back to it as
/// it already does for copying.
pub fn relocate(rendered: &Rendered, note: &Note) -> Anchor {
    if let Some((from, to)) = by_numbers(rendered, note) {
        if copied(rendered, from, to) == note.excerpt {
            return Anchor::At { from, to };
        }
    }
    match by_excerpt(rendered, note) {
        Some((from, to)) => Anchor::Moved { from, to },
        None => Anchor::Drifted,
    }
}

/// The range of lines carrying the recorded numbers, on the recorded side.
fn by_numbers(rendered: &Rendered, note: &Note) -> Option<(usize, usize)> {
    let mut bounds: Option<(usize, usize)> = None;
    for (index, row) in rendered.rows.iter().enumerate() {
        let Row::Line { hunk, line } = *row else {
            continue;
        };
        let source = rendered.file.hunks.get(hunk)?.lines.get(line)?;
        let number = match note.side {
            Side::Old => source.old_no,
            Side::New => source.new_no,
        };
        // A line with no number on that side — an addition seen from the old
        // version — does not belong to the range: it does not exist in the file
        // the note is about.
        let Some(number) = number else { continue };
        if number < note.start || number > note.end {
            continue;
        }
        bounds = Some(match bounds {
            Some((a, b)) => (a.min(index), b.max(index)),
            None => (index, index),
        });
    }
    bounds
}

/// Looks for the excerpt, line by line, in the whole diff.
///
/// A comparison of whole lines and not of substrings: it is what the note
/// quoted, and a partial match would put the remark in the middle of a line
/// where it means nothing.
fn by_excerpt(rendered: &Rendered, note: &Note) -> Option<(usize, usize)> {
    let needle: Vec<&str> = note.excerpt.lines().collect();
    if needle.is_empty() {
        return None;
    }
    // The entries as copying returns them: no headers, no "\ No newline"
    // annotation. The line's index in the unified list is kept alongside, the
    // only way back from one to the other.
    let hay: Vec<(usize, &str)> = rendered
        .rows
        .iter()
        .enumerate()
        .filter_map(|(index, row)| match *row {
            Row::Line { hunk, line } => {
                let source = rendered.file.hunks.get(hunk)?.lines.get(line)?;
                (source.kind != DiffLineKind::NoNewline).then_some((index, source.text.as_str()))
            }
            Row::Header { .. } => None,
        })
        .collect();
    if needle.len() > hay.len() {
        return None;
    }
    for start in 0..=hay.len() - needle.len() {
        if hay[start..start + needle.len()]
            .iter()
            .zip(&needle)
            .all(|((_, text), wanted)| text == wanted)
        {
            return Some((hay[start].0, hay[start + needle.len() - 1].0));
        }
    }
    None
}

fn copied(rendered: &Rendered, from: usize, to: usize) -> String {
    rendered.copy_text(from, to, false)
}

/// The annotated lines of a diff, one slot per list entry.
///
/// Two vectors and not one: the unified list and the two-column list do not
/// count the same entries, and the view switches between them with a shortcut.
/// Computing both when the diff arrives costs two walks, against one test per
/// visible line per frame if it were done in the render closure.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Marks {
    pub unified: Vec<bool>,
    pub split: Vec<bool>,
}

impl Marks {
    pub fn at(&self, index: usize, split: bool) -> bool {
        let marks = if split { &self.split } else { &self.unified };
        marks.get(index).copied().unwrap_or(false)
    }
}

/// Marks the lines the notes cover.
pub fn marks(rendered: &Rendered, spans: &[(usize, usize)]) -> Marks {
    let mut unified = vec![false; rendered.rows.len()];
    for (from, to) in spans {
        for mark in unified
            .get_mut(*from..=(*to).min(rendered.rows.len().saturating_sub(1)))
            .unwrap_or_default()
        {
            *mark = true;
        }
    }
    // The column view follows from the unified one: an entry is annotated as
    // soon as one of the lines it covers is.
    let split = rendered
        .split
        .iter()
        .map(|row| row.unified().any(|index| unified[index]))
        .collect();
    Marks { unified, split }
}

/// What a selection gives as a note.
///
/// The numbers are those of the **first and the last** of the range that carry
/// one on the chosen side. A block made entirely of lines from the other
/// version has none: the note then anchors on the opposite side, the only one
/// able to carry it.
pub fn anchor_selection(
    rendered: &Rendered,
    from: usize,
    to: usize,
) -> Option<(Side, usize, usize)> {
    let (from, to) = (from.min(to), from.max(to));
    let mut old: Option<(usize, usize)> = None;
    let mut new: Option<(usize, usize)> = None;
    for index in from..=to {
        let Some(Row::Line { hunk, line }) = rendered.rows.get(index).copied() else {
            continue;
        };
        let Some(source) = rendered
            .file
            .hunks
            .get(hunk)
            .and_then(|hunk| hunk.lines.get(line))
        else {
            continue;
        };
        for (slot, number) in [(&mut old, source.old_no), (&mut new, source.new_no)] {
            let Some(number) = number else { continue };
            *slot = Some(match *slot {
                Some((a, b)) => (a.min(number), b.max(number)),
                None => (number, number),
            });
        }
    }
    // The new version first: it is the one being reviewed, and a remark almost
    // always concerns the code that will remain.
    match (new, old) {
        (Some((start, end)), _) => Some((Side::New, start, end)),
        (None, Some((start, end))) => Some((Side::Old, start, end)),
        (None, None) => None,
    }
}

/// The message handed to the agent.
///
/// This is the piece to lock down — the rest of the sending is plumbing. The
/// format is the one an agent reads without further instruction: one heading per
/// location, the code quoted inside a fence, the remark as a block quote.
pub fn prompt(branch: &str, notes: &[Note]) -> String {
    let mut out = String::new();
    out.push_str(&crate::tr!("notes-prompt-intro", { branch: branch }));
    out.push_str("\n\n");
    for note in notes {
        out.push_str("## ");
        out.push_str(&note.location());
        out.push('\n');
        push_fence(&mut out, &note.excerpt, fence_language(&note.path));
        for line in note.body.lines() {
            out.push_str("> ");
            out.push_str(line);
            out.push('\n');
        }
        out.push('\n');
    }
    // Where to answer, and what not to touch.
    //
    // The variables are not expanded here: they are in the pty's environment
    // (`CLAUDHUB_NOTES_DIR`, `CLAUDHUB_TODO`), and it is the agent that reads
    // them — which keeps this function pure and testable, and makes a prompt
    // pasted into another terminal of the same worktree work too.
    out.push_str(&crate::tr!("notes-prompt-outro"));
    out.push('\n');
    // The final newline adds nothing and shows up in an agent's prompt, which
    // displays it as it is.
    while out.ends_with('\n') {
        out.pop();
    }
    out
}

/// A free question asked about a piece of code, without going through a note.
///
/// It is the most frequent gesture in practice: you read, something puzzles
/// you, you ask — with no remark to record.
pub fn ask(location: &str, path: &std::path::Path, excerpt: &str, question: &str) -> String {
    let mut out = String::new();
    out.push_str(&crate::tr!("notes-prompt-ask", { location: location }));
    out.push_str("\n\n");
    push_fence(&mut out, excerpt, fence_language(path));
    out.push_str(question.trim());
    out
}

/// A code fence whose number of backticks exceeds the longest run the excerpt
/// contains.
///
/// Backticks would almost always be enough; almost, because a Markdown excerpt
/// — or this very file — contains some, and would close the fence in the middle
/// of the quoted code.
fn push_fence(out: &mut String, excerpt: &str, language: &str) {
    let longest = excerpt.split(|c| c != '`').map(str::len).max().unwrap_or(0);
    let fence = "`".repeat(longest.max(2) + 1);
    out.push_str(&fence);
    out.push_str(language);
    out.push('\n');
    out.push_str(excerpt);
    if !excerpt.ends_with('\n') {
        out.push('\n');
    }
    out.push_str(&fence);
    out.push('\n');
}

/// The language name of a Markdown fence.
///
/// The same table as the highlighting, with one difference: what it does not
/// know gives a bare fence, and not an absent one.
fn fence_language(path: &std::path::Path) -> &'static str {
    crate::ui::highlight::language_for_path(path).unwrap_or("")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::git::{DiffLine, FileDiff, Hunk};

    fn line(kind: DiffLineKind, old: Option<usize>, new: Option<usize>, text: &str) -> DiffLine {
        DiffLine {
            kind,
            old_no: old,
            new_no: new,
            text: text.into(),
        }
    }

    /// A tiny but complete diff: some context, a removal, an addition, and the
    /// end-of-file annotation.
    fn sample() -> Rendered {
        let hunk = Hunk {
            header: "@@ -1,3 +1,3 @@".into(),
            old_start: 1,
            new_start: 1,
            lines: vec![
                line(DiffLineKind::Context, Some(1), Some(1), "fn main() {"),
                line(DiffLineKind::Removed, Some(2), None, "    old();"),
                line(DiffLineKind::Added, None, Some(2), "    new();"),
                line(DiffLineKind::Context, Some(3), Some(3), "}"),
                line(
                    DiffLineKind::NoNewline,
                    None,
                    None,
                    "No newline at end of file",
                ),
            ],
        };
        let file = FileDiff {
            hunks: vec![hunk],
            binary: false,
            empty: false,
        };
        Rendered::new(
            std::path::Path::new("src/main.rs"),
            file,
            &gpui_component::highlighter::HighlightTheme::default_light(),
        )
    }

    fn note_of(rendered: &Rendered, from: usize, to: usize) -> Note {
        let (side, start, end) = anchor_selection(rendered, from, to).expect("an anchor");
        Note {
            id: 1,
            path: "src/main.rs".into(),
            side,
            start,
            end,
            excerpt: rendered.copy_text(from, to, false),
            body: "needs another look".into(),
            ..Default::default()
        }
    }

    #[test]
    fn a_note_returns_to_the_rows_it_was_taken_on() {
        let rendered = sample();
        // Rows 2 and 3 of the list: the removal and the addition.
        let note = note_of(&rendered, 2, 3);
        assert_eq!(note.side, Side::New);
        assert_eq!((note.start, note.end), (2, 2));
        // The round trip returns exactly the range carrying that number — the
        // removed line has no number in the new version.
        assert_eq!(relocate(&rendered, &note), Anchor::Moved { from: 2, to: 3 });
    }

    #[test]
    fn a_context_range_anchors_on_its_own_numbers() {
        let rendered = sample();
        let note = note_of(&rendered, 1, 1);
        assert_eq!((note.side, note.start, note.end), (Side::New, 1, 1));
        assert_eq!(relocate(&rendered, &note), Anchor::At { from: 1, to: 1 });
    }

    #[test]
    fn a_removed_line_anchors_on_the_old_side() {
        let rendered = sample();
        // The only removal: it has no number in the new version, so the note
        // can only anchor on the old one.
        let note = note_of(&rendered, 2, 2);
        assert_eq!((note.side, note.start, note.end), (Side::Old, 2, 2));
        assert_eq!(relocate(&rendered, &note), Anchor::At { from: 2, to: 2 });
    }

    #[test]
    fn the_no_newline_marker_is_never_part_of_an_excerpt() {
        let rendered = sample();
        // The selection goes right to the end, annotation included: it must not
        // end up in the quoted code, otherwise the note never relocates — that
        // line is not in the file.
        let note = note_of(&rendered, 4, 5);
        assert!(!note.excerpt.contains("No newline"));
        assert_eq!(relocate(&rendered, &note), Anchor::At { from: 4, to: 4 });
    }

    #[test]
    fn a_note_follows_its_excerpt_when_the_file_has_moved() {
        let rendered = sample();
        let mut note = note_of(&rendered, 3, 3);
        // Twenty lines added above: the numbers are worthless now, but the
        // quoted code is still there.
        note.start += 20;
        note.end += 20;
        assert_eq!(relocate(&rendered, &note), Anchor::Moved { from: 3, to: 3 });
    }

    #[test]
    fn a_note_whose_code_is_gone_stays_and_says_so() {
        let rendered = sample();
        let mut note = note_of(&rendered, 3, 3);
        note.excerpt = "    gone();\n".into();
        note.start = 99;
        note.end = 99;
        // Drifted, and not deleted: a note lost in silence is worse than no note
        // at all.
        assert_eq!(relocate(&rendered, &note), Anchor::Drifted);
    }

    #[test]
    fn the_marks_of_the_two_layouts_agree() {
        let rendered = sample();
        let note = note_of(&rendered, 2, 3);
        let span = relocate(&rendered, &note).rows().expect("relocated");
        let marks = marks(&rendered, &[span]);
        // One slot per entry of each list: the render closure indexes without
        // checking, and a length that is too short would make the markers at
        // the bottom of the file disappear.
        assert_eq!(marks.unified.len(), rendered.rows.len());
        assert_eq!(marks.split.len(), rendered.split.len());
        assert!(marks.at(2, false) && marks.at(3, false));
        assert!(!marks.at(1, false));
        // Paired, the removal and the addition sit on a single entry of the
        // column view: it is annotated too.
        let annotated_pairs = marks.split.iter().filter(|mark| **mark).count();
        assert_eq!(annotated_pairs, 1);
    }

    #[test]
    fn the_prompt_quotes_the_code_and_the_remark() {
        let notes = vec![Note {
            id: 1,
            path: "src/ui/app.rs".into(),
            side: Side::New,
            start: 120,
            end: 121,
            excerpt: "let x = 1;\nlet y = 2;\n".into(),
            body: "two lines\nfor nothing".into(),
            ..Default::default()
        }];
        let text = prompt("agent/fix", &notes);
        assert!(text.contains("agent/fix"), "{text}");
        assert!(text.contains("## src/ui/app.rs:120-121"), "{text}");
        assert!(
            text.contains("```rust\nlet x = 1;\nlet y = 2;\n```"),
            "{text}"
        );
        assert!(text.contains("> two lines\n> for nothing"), "{text}");
        // The prompt goes into an agent's prompt, which shows trailing
        // whitespace as it is.
        assert!(!text.ends_with('\n'), "{text:?}");
    }

    #[test]
    fn a_fence_survives_an_excerpt_that_contains_one() {
        // A Markdown excerpt — or one from this very repository — contains
        // backticks; a fence of three would close the block in the middle of
        // the code.
        let notes = vec![Note {
            excerpt: "see ```rust``` above\n".into(),
            body: "hm".into(),
            path: "README.md".into(),
            ..Default::default()
        }];
        let text = prompt("main", &notes);
        assert!(text.contains("````"), "{text}");
    }
}
