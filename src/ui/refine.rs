//! Which words changed between the two versions of a line.
//!
//! A paired diff row says "this line became that one", and the block colour
//! stops there: the eye still has to find the one identifier that moved inside
//! a hundred columns of unchanged code. This module answers with byte ranges —
//! the words of the old line that are gone, the words of the new one that
//! arrived — and the view paints them as a stronger background over the line's
//! tint, the way every diff tool worth reading does.
//!
//! Pure on purpose: text in, ranges out, no gpui. The ranges follow the two
//! invariants the styling layer never checks — **sorted and disjoint**, and in
//! **bytes** — because they end up in `highlight::overlay` unchanged.

use std::ops::Range;

/// Past this many tokens a side is not refined: the LCS table is quadratic,
/// and a line that long is generated code nobody reads word by word.
const MOST_TOKENS: usize = 512;

/// What a token is, and therefore where a change starts and stops.
///
/// Identifiers stick together — `real_start_time` changes as one word, not as
/// five — and so does whitespace, so that a re-indentation is one change and
/// not one per space. Everything else is a token of its own: `(`, `,`, `'` —
/// punctuation matches punctuation one sign at a time.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Class {
    Word,
    Space,
    Symbol,
}

fn classify(ch: char) -> Class {
    if ch.is_alphanumeric() || ch == '_' {
        Class::Word
    } else if ch.is_whitespace() {
        Class::Space
    } else {
        Class::Symbol
    }
}

/// Cuts a line into tokens, as byte ranges. Contiguous by construction: every
/// byte of the text belongs to exactly one token.
fn tokens(text: &str) -> Vec<Range<usize>> {
    let mut out = Vec::new();
    let mut iter = text.char_indices().peekable();
    while let Some((start, ch)) = iter.next() {
        let class = classify(ch);
        let mut end = start + ch.len_utf8();
        if class != Class::Symbol {
            while let Some(&(at, next)) = iter.peek() {
                if classify(next) != class {
                    break;
                }
                end = at + next.len_utf8();
                iter.next();
            }
        }
        out.push(start..end);
    }
    out
}

/// The two sides' changed ranges: the old line's, then the new one's.
pub type Changes = (Vec<Range<usize>>, Vec<Range<usize>>);

/// The words that changed between two versions of a line: the old side's, then
/// the new side's, each sorted, disjoint and in bytes.
///
/// `None` means "do not refine": the lines share no actual word — pairing put
/// two unrelated lines opposite each other, and scattered marks on matching
/// commas would draw the eye to nothing — or a side is too long to compare.
/// The caller then leaves the block colour alone, which already says
/// "changed".
pub fn word_changes(old: &str, new: &str) -> Option<Changes> {
    let a = tokens(old);
    let b = tokens(new);
    if a.len() > MOST_TOKENS || b.len() > MOST_TOKENS {
        return None;
    }
    let (n, m) = (a.len(), b.len());

    // Longest common subsequence of tokens, compared by text. The table is
    // filled backwards so the backtrack below can walk forwards.
    let width = m + 1;
    let mut table = vec![0u32; (n + 1) * width];
    for i in (0..n).rev() {
        for j in (0..m).rev() {
            table[i * width + j] = if old[a[i].clone()] == new[b[j].clone()] {
                table[(i + 1) * width + j + 1] + 1
            } else {
                table[(i + 1) * width + j].max(table[i * width + j + 1])
            };
        }
    }

    let mut kept_a = vec![false; n];
    let mut kept_b = vec![false; m];
    // A match on whitespace or punctuation anchors nothing: two unrelated
    // lines of code share commas and indentation. Only a common *word* says
    // the pairing put two versions of the same line opposite each other.
    let mut anchored = false;
    let (mut i, mut j) = (0, 0);
    while i < n && j < m {
        if old[a[i].clone()] == new[b[j].clone()] {
            kept_a[i] = true;
            kept_b[j] = true;
            anchored |= old[a[i].clone()]
                .chars()
                .any(|ch| classify(ch) == Class::Word);
            i += 1;
            j += 1;
        } else if table[(i + 1) * width + j] >= table[i * width + j + 1] {
            i += 1;
        } else {
            j += 1;
        }
    }
    if !anchored {
        return None;
    }

    let changed_a = merge(&a, &kept_a);
    let changed_b = merge(&b, &kept_b);
    if changed_a.is_empty() && changed_b.is_empty() {
        return None;
    }
    Some((changed_a, changed_b))
}

/// The unkept tokens, glued into maximal byte ranges. Tokens are contiguous,
/// so two changed neighbours always fuse — the result stays sorted and
/// disjoint.
fn merge(tokens: &[Range<usize>], kept: &[bool]) -> Vec<Range<usize>> {
    let mut out: Vec<Range<usize>> = Vec::new();
    for (token, kept) in tokens.iter().zip(kept) {
        if *kept {
            continue;
        }
        match out.last_mut() {
            Some(last) if last.end == token.start => last.end = token.end,
            _ => out.push(token.clone()),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::word_changes;

    /// The one word that changed is the one marked, on both sides, and the
    /// quoting around it stays quiet.
    #[test]
    fn a_replaced_word_is_marked_on_both_sides() {
        let old = "'ISP.heure_fin', 'ISP.technician_note'";
        let new = "'ISP.heure_fin', 'ISP.real_start_time'";
        let (a, b) = word_changes(old, new).expect("shared words anchor the pair");
        assert_eq!(a.len(), 1);
        assert_eq!(&old[a[0].clone()], "technician_note");
        assert_eq!(b.len(), 1);
        assert_eq!(&new[b[0].clone()], "real_start_time");
    }

    /// An insertion marks only the new side; the old one has lost nothing.
    #[test]
    fn an_insertion_marks_only_the_new_side() {
        let old = "select(a, c)";
        let new = "select(a, b, c)";
        let (a, b) = word_changes(old, new).expect("shared words anchor the pair");
        assert!(a.is_empty());
        assert_eq!(b.len(), 1);
        assert_eq!(&new[b[0].clone()], "b, ");
    }

    /// Two lines that share only punctuation were paired by rank, not by
    /// kinship: no refinement, the block colour already says it all.
    #[test]
    fn unrelated_lines_are_not_refined() {
        assert_eq!(word_changes("return $this->total;", "if ($open) {"), None);
    }

    /// An identifier changes as one word: `heure` alone differing must not cut
    /// `heure_fin` in the middle.
    #[test]
    fn an_identifier_is_one_word() {
        let old = "x.heure_debut";
        let new = "x.minute_debut";
        let (a, b) = word_changes(old, new).expect("shared words anchor the pair");
        assert_eq!(&old[a[0].clone()], "heure_debut");
        assert_eq!(&new[b[0].clone()], "minute_debut");
    }

    /// Byte offsets, not character offsets: an accent weighs two bytes and the
    /// ranges must still land on token boundaries.
    #[test]
    fn multibyte_text_keeps_byte_ranges_on_boundaries() {
        let old = "libellé = ancien";
        let new = "libellé = récent";
        let (a, b) = word_changes(old, new).expect("shared words anchor the pair");
        assert_eq!(&old[a[0].clone()], "ancien");
        assert_eq!(&new[b[0].clone()], "récent");
    }

    /// A re-indented but otherwise identical pair marks only the whitespace,
    /// not the code: each side's leading run differs from the other's.
    #[test]
    fn a_reindent_marks_only_the_whitespace() {
        let old = "    foo(bar);";
        let new = "        foo(bar);";
        let (a, b) = word_changes(old, new).expect("shared words anchor the pair");
        assert_eq!(a, vec![0..4]);
        assert_eq!(b, vec![0..8]);
    }

    /// Past the token cap the comparison is declined, not attempted.
    #[test]
    fn a_line_too_long_is_not_refined() {
        let old = "a ".repeat(600);
        let new = format!("{old}b");
        assert_eq!(word_changes(&old, &new), None);
    }
}
