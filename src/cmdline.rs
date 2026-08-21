//! Découpage et recomposition d'une ligne de commande.
//!
//! Hors de `src/ui/` parce que les deux bouts s'en servent : le formulaire des
//! réglages écrit des morceaux et les relit en une ligne, et les workers
//! (`files::open_external`, `commit_msg::ask`) découpent ce que les réglages
//! leur passent — un binaire serveur sans gpui doit pouvoir le faire aussi.

/// Découpe une ligne de commande en honorant les guillemets.
///
/// `split_whitespace` casse sur tout chemin contenant une espace — et sous
/// Windows comme sous macOS, c'est le cas courant. Les règles sont celles d'un
/// shell POSIX réduites à l'essentiel : `'…'` littéral, `"…"` avec échappement
/// par contre-oblique, contre-oblique hors guillemets.
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
            (Some(_), '\\') => current.push(chars.next().unwrap_or('\\')),
            (Some(_), c) => current.push(c),
            (None, '\'') | (None, '"') => {
                quote = Some(c);
                // Un argument vide est un argument : `--sep ''` en est un.
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

/// Recompose une ligne de commande à partir de ses morceaux.
///
/// L'aller-retour avec `split_command` doit être fidèle : le formulaire écrit
/// des morceaux et les relit en une ligne, et un chemin avec une espace ne
/// doit pas se scinder en deux au premier passage.
pub fn join_command(parts: impl IntoIterator<Item = impl AsRef<str>>) -> String {
    parts
        .into_iter()
        .map(|part| {
            let part = part.as_ref();
            // La contre-oblique aussi : hors guillemets elle échappe, et un
            // chemin Windows perdrait les siennes au premier aller-retour.
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

    #[test]
    fn a_command_line_round_trips() {
        for line in [
            "claude",
            r#""/opt/my agent/bin/agent" --model "gpt 5""#,
            r#"agent --say "it says \"no\"""#,
            r#""C:\Program Files\agent.exe""#,
        ] {
            let parts = split_command(line);
            assert_eq!(split_command(&join_command(&parts)), parts, "{line}");
        }
    }
}
