//! Plain text, as the workers hand it on: undressed of a terminal's escapes.
//!
//! Pure, and in the core rather than beside one of its callers: a hook's line
//! (`wt`) and a test runner's narration (`suite`) are dressed the same way,
//! and two copies of the undressing had already drifted apart — one of them
//! let a window title through into the lines a run shows.

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
}
