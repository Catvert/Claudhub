//! Keeping a server's copy of a document in step with ours.
//!
//! Two things live here, and both are pure, which is what makes them testable
//! without a server and without gpui.
//!
//! **The version counter**, which a server uses to discard what arrives out of
//! order, and which must only ever go up.
//!
//! **The one-range edit.** A server declares how it wants changes: whole text
//! (`Full`) or ranges (`Incremental`). Sending the whole document to a server
//! that asked for ranges is a thing most of them tolerate and none of them
//! promise, so we compute the range: the common prefix and the common suffix of
//! the two texts frame exactly what changed, and one replacement describes it.
//! It is also cheaper by orders of magnitude on the gesture that actually
//! happens — one character typed in a file of two thousand lines.
//!
//! The subtlety is the unit. LSP positions count lines and **UTF-16 code
//! units**, not bytes and not characters: `é` is two bytes, one unit; an emoji
//! is four bytes and *two* units. Counting bytes would put the edit a column
//! early on every accented line, and counting characters would put it a column
//! late on every emoji.

/// A position as LSP counts them: zero-based line, and UTF-16 code units into
/// that line.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Position {
    pub line: u32,
    pub character: u32,
}

/// The single replacement that turns one text into another.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Change {
    pub start: Position,
    pub end: Position,
    pub text: String,
}

/// What the server holds of one file, and what we last told it.
#[derive(Debug, Clone)]
pub struct Document {
    pub text: String,
    /// Starts at 1, as `didOpen` announces, and never goes back.
    pub version: i32,
}

impl Document {
    pub fn new(text: String) -> Self {
        Self { text, version: 1 }
    }

    /// Records a new text and returns the edit that describes it, or `None`
    /// when nothing changed — an editor emits a change event for a keystroke
    /// that replaced a selection with itself, and a round trip for nothing is
    /// a round trip that can arrive out of order.
    pub fn edit(&mut self, text: String) -> Option<Change> {
        let change = change(&self.text, &text)?;
        self.text = text;
        self.version += 1;
        Some(change)
    }
}

/// The one replacement that turns `old` into `new`.
pub fn change(old: &str, new: &str) -> Option<Change> {
    if old == new {
        return None;
    }
    let prefix = common_prefix(old, new);
    let suffix = common_suffix(old, new, prefix);
    Some(Change {
        start: position_at(old, prefix),
        end: position_at(old, old.len() - suffix),
        text: new[prefix..new.len() - suffix].to_string(),
    })
}

/// The position of a byte offset, which the caller must have taken on a
/// character boundary — both of ours come from character-wise scans.
pub fn position_at(text: &str, offset: usize) -> Position {
    let mut line = 0u32;
    let mut character = 0u32;
    for (index, c) in text.char_indices() {
        if index >= offset {
            break;
        }
        if c == '\n' {
            line += 1;
            character = 0;
        } else {
            character += c.len_utf16() as u32;
        }
    }
    Position { line, character }
}

/// The end of a document, which is where an append lands.
pub fn end_of(text: &str) -> Position {
    position_at(text, text.len())
}

fn common_prefix(a: &str, b: &str) -> usize {
    let mut bytes = 0;
    for (x, y) in a.chars().zip(b.chars()) {
        if x != y {
            break;
        }
        bytes += x.len_utf8();
    }
    bytes
}

/// The common suffix, in bytes, stopping at `prefix` so the two halves of the
/// span never overlap — without that floor, `"aa"` → `"a"` would claim a prefix
/// of one and a suffix of one on a text two bytes long, and the range would run
/// backwards.
fn common_suffix(a: &str, b: &str, prefix: usize) -> usize {
    let mut bytes = 0;
    let mut left = a[prefix..].chars().rev();
    let mut right = b[prefix..].chars().rev();
    loop {
        match (left.next(), right.next()) {
            (Some(x), Some(y)) if x == y => bytes += x.len_utf8(),
            _ => return bytes,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(line: u32, character: u32) -> Position {
        Position { line, character }
    }

    #[test]
    fn an_unchanged_text_is_no_edit() {
        assert!(change("same", "same").is_none());
    }

    #[test]
    fn one_character_typed_is_one_insertion() {
        let c = change("let x = 1;\nlet y = 2;\n", "let x = 1;\nlet yy = 2;\n").unwrap();
        assert_eq!(c.start, at(1, 5));
        assert_eq!(c.end, at(1, 5));
        assert_eq!(c.text, "y");
    }

    #[test]
    fn a_deletion_is_an_empty_replacement() {
        let c = change("abcdef", "abef").unwrap();
        assert_eq!(c.start, at(0, 2));
        assert_eq!(c.end, at(0, 4));
        assert_eq!(c.text, "");
    }

    /// The column is in UTF-16 units: `é` is two bytes and one unit, so an edit
    /// after three of them is at column three, not six.
    #[test]
    fn accents_count_one_column_each() {
        let c = change("ééé x", "ééé y").unwrap();
        assert_eq!(c.start, at(0, 4));
        assert_eq!(c.end, at(0, 5));
        assert_eq!(c.text, "y");
    }

    /// And an emoji is four bytes but **two** units — the case where counting
    /// characters is as wrong as counting bytes.
    #[test]
    fn an_emoji_counts_two_columns() {
        let c = change("🐘 x", "🐘 y").unwrap();
        assert_eq!(c.start, at(0, 3));
        assert_eq!(c.end, at(0, 4));
    }

    /// Prefix and suffix must not overlap, or the range runs backwards and the
    /// server applies nonsense.
    #[test]
    fn the_prefix_and_the_suffix_never_overlap() {
        let c = change("aa", "a").unwrap();
        assert_eq!(c.start, at(0, 1));
        assert_eq!(c.end, at(0, 2));
        assert_eq!(c.text, "");
    }

    #[test]
    fn a_line_added_at_the_end_starts_at_the_end() {
        let c = change("one\ntwo\n", "one\ntwo\nthree\n").unwrap();
        assert_eq!(c.start, at(2, 0));
        assert_eq!(c.end, at(2, 0));
        assert_eq!(c.text, "three\n");
    }

    #[test]
    fn the_version_only_goes_up_and_only_on_a_real_change() {
        let mut doc = Document::new("a".into());
        assert_eq!(doc.version, 1);
        assert!(doc.edit("a".into()).is_none());
        assert_eq!(doc.version, 1);
        assert!(doc.edit("ab".into()).is_some());
        assert_eq!(doc.version, 2);
        assert_eq!(doc.text, "ab");
    }

    #[test]
    fn the_end_of_a_text_is_after_its_last_line() {
        assert_eq!(end_of("a\nbc"), at(1, 2));
        assert_eq!(end_of("a\n"), at(1, 0));
        assert_eq!(end_of(""), at(0, 0));
    }
}
