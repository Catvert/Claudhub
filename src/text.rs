//! Plain text, as the workers hand it on: undressed of a terminal's escapes,
//! quoted into Markdown for an agent.
//!
//! Pure, and in the core rather than beside one of its callers: a hook's line
//! (`wt`) and a test runner's narration (`suite`) are dressed the same way,
//! and two copies of the undressing had already drifted apart — one of them
//! let a window title through into the lines a run shows. The prompts written
//! for an agent (a failing test, a Sentry trace, a review finding, a note)
//! quote code the same way too, and two of the four quoted it with a fence
//! the code could close. And a panel's filter reads case one way everywhere —
//! the lists a worker fills (Sentry, GitHub) as much as those the interface
//! does.

use std::ops::Range;

/// A line, undressed: ANSI escapes are instructions to a terminal, and the
/// console panel and the journal are plain text — an SGR left in shows as
/// `␛[36m` in the middle of the sentence. CSI sequences (colours, cursor),
/// OSC ones (titles, up to BEL or ST) and the two-byte escapes are dropped;
/// the carriage returns of a progress bar go with them.
pub fn strip_ansi(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '\u{1b}' => match chars.next() {
                // CSI: parameters and intermediates, then one final byte.
                Some('[') => {
                    for c in chars.by_ref() {
                        if ('\u{40}'..='\u{7e}').contains(&c) {
                            break;
                        }
                    }
                }
                // OSC: swallowed up to BEL or ST (ESC \).
                Some(']') => {
                    while let Some(c) = chars.next() {
                        if c == '\u{07}' || (c == '\u{1b}' && chars.peek() == Some(&'\\')) {
                            if c == '\u{1b}' {
                                chars.next();
                            }
                            break;
                        }
                    }
                }
                // Two-byte escapes (ESC c, ESC =, …): the second byte goes too.
                Some(_) | None => {}
            },
            '\r' => {}
            c => out.push(c),
        }
    }
    out
}

/// The longest head of `text` that fits in `max` **bytes** without cutting a
/// character in two — a slice cut on raw bytes is no longer valid UTF-8, and
/// panics. `str::floor_char_boundary` is the same thing, not stable at this
/// crate's `rust-version`.
pub fn head_bytes(text: &str, max: usize) -> &str {
    if text.len() <= max {
        return text;
    }
    let mut end = max;
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    &text[..end]
}

/// `text` at most `max` **characters** long, the cut said by an ellipsis.
pub fn ellipsized(text: &str, max: usize) -> String {
    match text.char_indices().nth(max) {
        Some((at, _)) => format!("{}…", &text[..at]),
        None => text.to_string(),
    }
}

/// A fenced block the text cannot close: one backtick more than its longest
/// run, and never fewer than three. `language` follows the opening fence, and
/// may be empty.
///
/// Three backticks would almost always be enough; almost, because a Markdown
/// excerpt — or a line of this very file — contains some, and would close the
/// fence in the middle of the quoted code, the rest of the prompt read as
/// something else.
pub fn fence(text: &str, language: &str) -> String {
    let fence = "`".repeat((backtick_run(text) + 1).max(3));
    let mut out = String::with_capacity(text.len() + 2 * fence.len() + language.len() + 3);
    out.push_str(&fence);
    out.push_str(language);
    out.push('\n');
    out.push_str(text);
    if !text.ends_with('\n') {
        out.push('\n');
    }
    out.push_str(&fence);
    out.push('\n');
    out
}

/// Inline code the text cannot close, by the same rule — a shell line can
/// hold a backtick inside its quotes.
pub fn inline_code(text: &str) -> String {
    let fence = "`".repeat(backtick_run(text) + 1);
    let pad = if text.starts_with('`') || text.ends_with('`') {
        " "
    } else {
        ""
    };
    format!("{fence}{pad}{text}{pad}{fence}")
}

/// The longest run of backticks in the text.
fn backtick_run(text: &str) -> usize {
    let (mut longest, mut run) = (0, 0);
    for c in text.chars() {
        if c == '`' {
            run += 1;
            longest = longest.max(run);
        } else {
            run = 0;
        }
    }
    longest
}

/// Does the query match the text? The filter every panel applies, a row at a
/// time — **smart case**: an all-lowercase query ignores case, a query
/// carrying a capital respects it. It is every editor's convention, and it
/// saves one more button for a setting that changes with every search. An
/// empty query matches everything.
pub fn matches(query: &str, haystack: &str) -> bool {
    let query = query.trim();
    if query.is_empty() {
        return true;
    }
    first_match(query, haystack, 0).is_some()
}

/// Every occurrence, as **byte** offsets — that is what gpui expects to style a
/// fragment of text, and indexing by characters breaks at the first accent.
pub fn find_all(query: &str, haystack: &str) -> Vec<Range<usize>> {
    let query = query.trim();
    let mut out = Vec::new();
    if query.is_empty() {
        return out;
    }
    let mut from = 0;
    while let Some(range) = first_match(query, haystack, from) {
        // An empty occurrence would loop forever: `first_match` returns none,
        // the query never being empty here.
        from = range.end;
        out.push(range);
    }
    out
}

/// The first occurrence at or after `from` — what vim's `/` and `n` step with,
/// so that both searches read case the same way.
pub fn find_from(query: &str, haystack: &str, from: usize) -> Option<Range<usize>> {
    let query = query.trim();
    if query.is_empty() {
        return None;
    }
    first_match(query, haystack, from)
}

/// The first occurrence from a given offset.
///
/// A character-by-character comparison rather than a search inside
/// `to_lowercase()`: lowercasing changes the byte length of some characters,
/// and the offsets returned would no longer point at anything in the original
/// text.
fn first_match(query: &str, haystack: &str, from: usize) -> Option<Range<usize>> {
    let sensitive = query.chars().any(char::is_uppercase);
    let first = query.chars().next()?;
    // Sliced and not skipped: `find_all` restarts from the end of the previous
    // occurrence, and walking the whole text again each time made it quadratic.
    // `from` is always a character boundary — it comes from a match's end.
    for (offset, candidate) in haystack[from..].char_indices() {
        let start = from + offset;
        if !same(candidate, first, sensitive) {
            continue;
        }
        let mut end = start;
        let mut hay = haystack[start..].chars();
        let mut ok = true;
        for wanted in query.chars() {
            match hay.next() {
                Some(c) if same(c, wanted, sensitive) => end += c.len_utf8(),
                _ => {
                    ok = false;
                    break;
                }
            }
        }
        if ok {
            return Some(start..end);
        }
    }
    None
}

fn same(a: char, b: char, sensitive: bool) -> bool {
    if sensitive {
        a == b
    } else {
        a == b || a.to_lowercase().eq(b.to_lowercase())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The dress a hook's line arrives in — colours, a title, a progress
    /// bar's carriage return — is dropped whole; the words stay.
    #[test]
    fn ansi_escapes_are_stripped_and_the_words_stay() {
        assert_eq!(
            strip_ansi("\u{1b}[36mvendor\u{1b}[0m recable\r"),
            "vendor recable"
        );
        assert_eq!(strip_ansi("\u{1b}]0;title\u{07}plain \u{1b}c!"), "plain !");
        assert_eq!(strip_ansi("sans habit"), "sans habit");
    }

    /// Jest paints its stderr even through a pipe.
    #[test]
    fn the_colours_come_out_of_a_narrated_line() {
        assert_eq!(
            strip_ansi(
                "\u{1b}[7m\u{1b}[1m\u{1b}[31m FAIL \u{1b}[39m\u{1b}[22m\u{1b}[27m src/http.test.js"
            ),
            " FAIL  src/http.test.js"
        );
    }

    /// A window title set by a runner, closed by BEL or by ST, is an escape
    /// and not words: a CSI-only undressing let `0;title` through into the
    /// lines a run shows.
    #[test]
    fn a_title_goes_whole_whichever_way_it_is_closed() {
        assert_eq!(strip_ansi("\u{1b}]0;vitest\u{07} RUN  v4"), " RUN  v4");
        assert_eq!(
            strip_ansi("\u{1b}]2;pest\u{1b}\\  PASS  Tests\\Unit"),
            "  PASS  Tests\\Unit"
        );
    }

    /// A two-byte character straddling the limit goes whole, never halved.
    #[test]
    fn a_cut_in_bytes_falls_between_two_characters() {
        assert_eq!(head_bytes("short", 10), "short");
        assert_eq!(head_bytes("abé", 3), "ab");
        assert_eq!(head_bytes("abé", 4), "abé");
        assert_eq!(head_bytes("éé", 1), "");
    }

    #[test]
    fn a_cut_in_characters_says_so() {
        assert_eq!(ellipsized("été", 3), "été");
        assert_eq!(ellipsized("étés", 3), "été…");
        assert_eq!(ellipsized("", 0), "");
    }

    #[test]
    fn a_fence_is_longer_than_what_it_holds() {
        assert_eq!(fence("a ``` b", ""), "````\na ``` b\n````\n");
        assert_eq!(fence("let x = 1;\n", "rust"), "```rust\nlet x = 1;\n```\n");
        assert_eq!(inline_code("echo `x`"), "`` echo `x` ``");
        assert_eq!(inline_code("plain"), "`plain`");
    }

    #[test]
    fn an_all_lowercase_query_ignores_case() {
        assert!(matches("todo", "TODO: rewrite"));
        assert!(matches("REWRITE", "TODO: REWRITE"));
    }

    #[test]
    fn a_query_with_a_capital_respects_it() {
        assert!(!matches("Todo", "todo: rewrite"));
        assert!(matches("Todo", "Todo: rewrite"));
    }

    #[test]
    fn an_empty_query_matches_everything() {
        assert!(matches("", "anything at all"));
        assert!(matches("   ", "anything at all"));
        assert!(find_all("", "anything at all").is_empty());
    }

    /// The offsets are byte offsets: a case-insensitive search must not shift
    /// them by an accent.
    #[test]
    fn offsets_are_byte_offsets_even_past_an_accent() {
        let text = "été chaud";
        let hits = find_all("chaud", text);
        assert_eq!(hits, vec![6..11]);
        assert_eq!(&text[hits[0].clone()], "chaud");
    }

    #[test]
    fn a_repeated_needle_is_found_every_time() {
        assert_eq!(find_all("ab", "abcab"), vec![0..2, 3..5]);
    }

    /// Every occurrence is found from the end of the previous one, and the
    /// offsets stay byte offsets past a multi-byte character: that is what the
    /// resumed scan must not break.
    #[test]
    fn the_scan_resumes_where_the_last_occurrence_ended() {
        let text = "éaébéc";
        let hits = find_all("é", text);
        assert_eq!(hits, vec![0..2, 3..5, 6..8]);
        for hit in &hits {
            assert_eq!(&text[hit.clone()], "é");
        }
    }

    /// Two overlapping occurrences are not returned twice: the ranges have to
    /// stay disjoint for gpui to accept them.
    #[test]
    fn overlapping_occurrences_do_not_overlap_in_the_result() {
        assert_eq!(find_all("aa", "aaaa"), vec![0..2, 2..4]);
    }

    #[test]
    fn a_needle_longer_than_the_line_is_not_found() {
        assert!(find_all("abcdef", "abc").is_empty());
    }
}
