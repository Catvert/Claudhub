//! Splitting and rebuilding a command line.
//!
//! Outside `src/ui/` because both ends use it: the settings form writes pieces
//! and reads them back as one line, and the workers (`files::open_external`,
//! `commit_msg::ask`) split what the settings hand them — a server binary
//! without gpui has to be able to do it too.

/// Splits a command line, honouring quotes.
///
/// `split_whitespace` breaks on any path containing a space — and on Windows
/// as on macOS, that is the common case. The rules are a POSIX shell's, cut
/// down to the essentials: `'…'` literal, `"…"` with backslash escapes,
/// backslash outside quotes.
///
/// **Between double quotes a backslash escapes five characters and no
/// others** — `$`, `` ` ``, `"`, `\` and the newline, the last one a line
/// continuation — and stays a backslash before anything else. That is the
/// whole of what makes `"C:\Program Files\agent.exe"` a Windows path: read as
/// escaping everything, it came out `C:Program Filesagent.exe`.
pub fn split_command(line: &str) -> Vec<String> {
    let mut parts = Vec::new();
    let mut current = String::new();
    let mut started = false;
    let mut chars = line.chars().peekable();
    let mut quote: Option<char> = None;
    while let Some(c) = chars.next() {
        match (quote, c) {
            (Some(q), c) if c == q => quote = None,
            (Some('\''), c) => current.push(c),
            (Some(_), '\\') => match chars.peek() {
                Some('$' | '`' | '"' | '\\') => current.extend(chars.next()),
                Some('\n') => {
                    chars.next();
                }
                _ => current.push('\\'),
            },
            (Some(_), c) => current.push(c),
            (None, '\'') | (None, '"') => {
                quote = Some(c);
                // An empty argument is an argument: `--sep ''` is one.
                started = true;
            }
            (None, '\\') => current.push(chars.next().unwrap_or('\\')),
            (None, c) if c.is_whitespace() => {
                if started || !current.is_empty() {
                    parts.push(std::mem::take(&mut current));
                    started = false;
                }
            }
            (None, c) => current.push(c),
        }
    }
    if started || !current.is_empty() {
        parts.push(current);
    }
    parts
}

/// Rebuilds a command line from its pieces.
///
/// The round trip with `split_command` has to be faithful: the form writes
/// pieces and reads them back as one line, and a path with a space must not
/// split in two on the first pass.
///
/// Inside the double quotes it writes, a backslash is always doubled and a
/// quote always escaped — the two escapes both readings agree on, ours and a
/// shell's, since this line may go to either.
pub fn join_command(parts: impl IntoIterator<Item = impl AsRef<str>>) -> String {
    parts
        .into_iter()
        .map(|part| {
            let part = part.as_ref();
            // Backslashes too: outside quotes they escape, and a Windows path
            // would lose its own on the first round trip.
            if part.is_empty()
                || part
                    .chars()
                    .any(|c| c.is_whitespace() || c == '\'' || c == '"' || c == '\\')
            {
                format!("\"{}\"", part.replace('\\', "\\\\").replace('"', "\\\""))
            } else {
                part.to_string()
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_command_line_survives_quotes_and_spaces() {
        // The flaw this splitting fixes: a path containing a space.
        assert_eq!(
            split_command(r#""/opt/my agent/bin/agent" --model "gpt 5""#),
            vec!["/opt/my agent/bin/agent", "--model", "gpt 5"]
        );
        // Single quotes, literal.
        assert_eq!(
            split_command("sh -c 'echo one two'"),
            vec!["sh", "-c", "echo one two"]
        );
        // An empty argument counts as one.
        assert_eq!(split_command("agent --sep ''"), vec!["agent", "--sep", ""]);
        assert_eq!(split_command("   "), Vec::<String>::new());
    }

    /// Between double quotes a backslash escapes `$`, `` ` ``, `"`, `\` and
    /// the newline, as in a POSIX shell, and is a backslash before anything
    /// else — which is what a Windows path is made of.
    #[test]
    fn a_backslash_in_double_quotes_escapes_five_characters() {
        assert_eq!(
            split_command(r#""C:\Program Files\x.exe" --flag"#),
            vec![r"C:\Program Files\x.exe", "--flag"]
        );
        assert_eq!(
            split_command(r#""a\"b\\c\$d\`e\f""#),
            vec![r#"a"b\c$d`e\f"#]
        );
        // A backslash and a newline are a continuation, and vanish.
        assert_eq!(split_command("\"ab\\\ncd\""), vec!["abcd"]);
        // Single quotes are literal, backslash and all.
        assert_eq!(split_command(r"'C:\x\'"), vec![r"C:\x\"]);
        // Outside quotes it still escapes whatever follows.
        assert_eq!(split_command(r"my\ agent"), vec!["my agent"]);
    }

    #[test]
    fn a_command_line_round_trips() {
        for line in [
            "claude",
            r#""/opt/my agent/bin/agent" --model "gpt 5""#,
            r#"agent --say "it says \"no\"""#,
            r#""C:\Program Files\agent.exe""#,
            r#""C:\dir\\" "a\\\"b" "cost \$5" "\`x\`""#,
        ] {
            let parts = split_command(line);
            assert_eq!(split_command(&join_command(&parts)), parts, "{line}");
        }
    }
}
