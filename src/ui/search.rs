//! What the search panel shows, without a line of gpui.
//!
//! `git::search` says what git found; this says what the list makes of it —
//! which rows exist, what a fold hides, where an arrow lands. It is the same
//! split as `notes.rs` in front of `notes_view.rs`, and for the same reason:
//! these are the only decisions of the panel worth a test, and they are the
//! ones that break silently.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use crate::git::search::Results;

/// A row of the displayed list.
///
/// **Indices and not values**, the rule of `ui::tree` and of the databases
/// tree: the list is rebuilt on every fold and on every arrival, and cloning a
/// path per row is what a gesture can pay and a frame cannot.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Row {
    File { file: usize },
    Hit { file: usize, hit: usize },
}

impl Row {
    pub fn file(self) -> usize {
        match self {
            Row::File { file } | Row::Hit { file, .. } => file,
        }
    }

    pub fn is_file(self) -> bool {
        matches!(self, Row::File { .. })
    }
}

/// The flattened list: one row per file, and its hits under it unless it is
/// folded.
///
/// A file is always shown, folded or not: it is the answer to "which files are
/// concerned", which is half of what a project-wide search is asked.
pub fn rows(results: &Results, folded: &HashSet<PathBuf>) -> Vec<Row> {
    let mut out = Vec::new();
    for (file, hits) in results.files.iter().enumerate() {
        out.push(Row::File { file });
        if folded.contains(&hits.path) {
            continue;
        }
        out.extend((0..hits.hits.len()).map(|hit| Row::Hit { file, hit }));
    }
    out
}

/// Where an arrow lands, from where one stands.
///
/// **It stops at both ends rather than wrapping.** A result list is read from
/// the top down, and coming back to the first hit after the last one is how one
/// reads the same file twice without noticing. The same choice as the review's
/// arrows.
pub fn step(rows: &[Row], from: Option<usize>, delta: isize) -> Option<usize> {
    if rows.is_empty() {
        return None;
    }
    let last = rows.len() - 1;
    let Some(from) = from else {
        // Nothing selected yet: the first row going down, the last going up.
        return Some(if delta >= 0 { 0 } else { last });
    };
    let next = from as isize + delta;
    Some(next.clamp(0, last as isize) as usize)
}

/// What a selection offers the search field, if it offers anything.
///
/// The gesture is PhpStorm's: one highlights a call, asks where else it is
/// made, and the shortcut spares retyping what is already under the cursor.
/// What is worth a test is the refusal, because each refusal is a query nobody
/// meant to ask — and because a bad one is silent, the field simply carrying a
/// word the hand did not choose.
///
/// - **Blanks are trimmed**, a selection dragged with the mouse rarely stopping
///   on the word.
/// - **More than one line is refused**: `git grep` matches *within* a line, so a
///   two-line term can only ever find nothing.
/// - **Longer than `MAX_SEED` is refused rather than cut**: a truncated query
///   searches for something no one selected.
pub fn seed(selection: &str) -> Option<String> {
    let text = selection.trim();
    if text.is_empty() || text.contains('\n') || text.chars().count() > MAX_SEED {
        return None;
    }
    Some(text.to_string())
}

/// Past this many characters a selection is a passage and not a term.
pub const MAX_SEED: usize = 200;

/// The first hit of the list, which is what a finished search selects.
///
/// The first **hit** and not the first row: landing on a file heading would
/// leave the preview empty on a search that found something.
pub fn first_hit(rows: &[Row]) -> Option<usize> {
    rows.iter().position(|row| !row.is_file())
}

/// Which file a row belongs to, as a path.
pub fn path_of(results: &Results, row: Row) -> Option<&Path> {
    results
        .files
        .get(row.file())
        .map(|file| file.path.as_path())
}

/// The line a row points at, or the top of the file for a heading.
pub fn line_of(results: &Results, row: Row) -> u32 {
    match row {
        Row::File { .. } => 1,
        Row::Hit { file, hit } => results
            .files
            .get(file)
            .and_then(|file| file.hits.get(hit))
            .map(|hit| hit.line)
            .unwrap_or(1),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::git::search::{FileHits, Hit};

    fn results() -> Results {
        Results {
            files: vec![
                FileHits {
                    path: PathBuf::from("a.rs"),
                    hits: vec![
                        Hit {
                            line: 3,
                            text: "one".into(),
                        },
                        Hit {
                            line: 8,
                            text: "two".into(),
                        },
                    ],
                    capped: false,
                },
                FileHits {
                    path: PathBuf::from("b.rs"),
                    hits: vec![Hit {
                        line: 1,
                        text: "three".into(),
                    }],
                    capped: false,
                },
            ],
            total: 3,
            truncated: false,
        }
    }

    #[test]
    fn every_file_and_its_hits_make_a_row() {
        let rows = rows(&results(), &HashSet::new());
        assert_eq!(rows.len(), 5);
        assert!(rows[0].is_file());
        assert_eq!(rows[1], Row::Hit { file: 0, hit: 0 });
        assert_eq!(rows[3], Row::File { file: 1 });
    }

    /// A folded file keeps its heading: it is the answer to "which files", and
    /// removing it would make folding look like filtering.
    #[test]
    fn a_folded_file_keeps_its_heading_and_loses_its_hits() {
        let mut folded = HashSet::new();
        folded.insert(PathBuf::from("a.rs"));
        let rows = rows(&results(), &folded);
        assert_eq!(rows.len(), 3);
        assert_eq!(rows[0], Row::File { file: 0 });
        assert_eq!(rows[1], Row::File { file: 1 });
    }

    #[test]
    fn the_arrows_stop_at_both_ends() {
        let rows = rows(&results(), &HashSet::new());
        assert_eq!(step(&rows, Some(0), -1), Some(0));
        assert_eq!(step(&rows, Some(4), 1), Some(4));
        assert_eq!(step(&rows, Some(2), 1), Some(3));
        assert_eq!(step(&rows, None, 1), Some(0));
        assert_eq!(step(&rows, None, -1), Some(4));
        assert_eq!(step(&[], Some(0), 1), None);
    }

    #[test]
    fn a_finished_search_lands_on_a_hit_and_not_on_a_heading() {
        let rows = rows(&results(), &HashSet::new());
        assert_eq!(first_hit(&rows), Some(1));
        let mut folded = HashSet::new();
        folded.insert(PathBuf::from("a.rs"));
        // Everything folded but the second file: the first hit is under it.
        let rows = rows_of(&folded);
        assert_eq!(first_hit(&rows), Some(2));
    }

    fn rows_of(folded: &HashSet<PathBuf>) -> Vec<Row> {
        rows(&results(), folded)
    }

    #[test]
    fn a_row_names_its_file_and_its_line() {
        let results = results();
        let rows = rows(&results, &HashSet::new());
        assert_eq!(path_of(&results, rows[2]), Some(Path::new("a.rs")));
        assert_eq!(line_of(&results, rows[2]), 8);
        // A heading points at the top of its file: the preview has to open
        // somewhere.
        assert_eq!(line_of(&results, rows[0]), 1);
    }

    #[test]
    fn a_selection_seeds_the_field_only_when_it_is_a_term() {
        assert_eq!(seed("  handle_event  "), Some("handle_event".into()));
        // A whole line taken in visual-line mode, its newline included.
        assert_eq!(seed("    let value = 1;\n"), Some("let value = 1;".into()));
        // Nothing selected, and a caret's worth of whitespace.
        assert_eq!(seed(""), None);
        assert_eq!(seed(" \t "), None);
        // Two lines: `git grep` matches within a line, so this finds nothing.
        assert_eq!(seed("first\nsecond"), None);
        // Too long is refused, not cut: a truncated query is one nobody asked.
        assert_eq!(seed(&"x".repeat(MAX_SEED)), Some("x".repeat(MAX_SEED)));
        assert_eq!(seed(&"x".repeat(MAX_SEED + 1)), None);
        // Counted in characters and not bytes.
        assert_eq!(
            seed(&"é".repeat(MAX_SEED)).map(|s| s.chars().count()),
            Some(MAX_SEED)
        );
    }
}
