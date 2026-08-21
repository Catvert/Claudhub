//! Highlighting for Blade views.
//!
//! Blade is not a language tree-sitter knows: no grammar is published for it,
//! and the PHP grammar sees nothing but HTML text in `@foreach` or `{{ $x }}`.
//! A Blade view therefore arrived in the review with its tags coloured and all
//! of its own vocabulary in grey.
//!
//! The answer is an overlay: the PHP grammar colours what it can read — HTML,
//! attributes, `<?php` blocks — then this module paints Blade's constructs on
//! top. It is a hand-written scanner, not a parser, and that is accepted:
//! Blade's syntax fits in three shapes, and a full parser would cost far more
//! than it would return.
//!
//! What the overlay recognises: directives (`@if`, `@endforeach`, with their
//! parenthesised argument), echoes (`{{ }}`, `{!! !!}`) and comments
//! (`{{-- --}}`), including across several lines.

use std::ops::Range;
use std::path::Path;

use gpui_component::highlighter::HighlightTheme;

use super::highlight::LineStyles;
use crate::git::FileDiff;

/// True for a Blade view.
///
/// The full name, not the extension: `invoice.blade.php` has `php` for an
/// extension, and it really is PHP — but with an extra dialect.
pub fn is_blade(path: &Path) -> bool {
    path.file_name()
        .and_then(|n| n.to_str())
        .is_some_and(|n| n.to_ascii_lowercase().ends_with(".blade.php"))
}

/// Paints Blade's constructs over the PHP grammar's styles.
pub fn overlay(diff: &FileDiff, theme: &HighlightTheme, styles: &mut [Vec<LineStyles>]) {
    for (h, hunk) in diff.hunks.iter().enumerate() {
        // Comment state starts again from scratch at each hunk: what separates
        // them has been elided, and nothing says a `{{--` left open above was
        // not closed inside the gap.
        let mut open_comment = false;
        for (l, line) in hunk.lines.iter().enumerate() {
            let found = scan(&line.text, &mut open_comment);
            let Some(target) = styles.get_mut(h).and_then(|h| h.get_mut(l)) else {
                continue;
            };
            apply(&found, theme, target);
        }
    }
}

/// Replaces in `target` whatever the overlay covers.
///
/// The grammar's styles touching a Blade range are removed rather than layered:
/// rendering expects sorted, disjoint ranges, and a half-covered keyword means
/// nothing anyway.
fn apply(found: &[(Range<usize>, Scope)], theme: &HighlightTheme, target: &mut LineStyles) {
    let styled: Vec<(Range<usize>, gpui::HighlightStyle)> = found
        .iter()
        .filter_map(|(range, scope)| Some((range.clone(), scope.style(theme)?)))
        .collect();
    if styled.is_empty() {
        return;
    }
    target.retain(|(range, _)| {
        !styled
            .iter()
            .any(|(over, _)| range.start < over.end && over.start < range.end)
    });
    target.extend(styled);
    target.sort_by_key(|(range, _)| range.start);
}

/// Cuts a line into Blade ranges, each with the style name it deserves. The
/// ranges returned are sorted and disjoint.
///
/// `open_comment` carries the only state crossing lines: a `{{--` left
/// unclosed.
fn scan(line: &str, open_comment: &mut bool) -> Vec<(Range<usize>, Scope)> {
    let mut out = Vec::new();
    let mut i = 0;

    if *open_comment {
        match line.find("--}}") {
            Some(end) => {
                out.push((0..end + 4, Scope::Comment));
                *open_comment = false;
                i = end + 4;
            }
            None => {
                if !line.is_empty() {
                    out.push((0..line.len(), Scope::Comment));
                }
                return out;
            }
        }
    }

    while i < line.len() {
        let rest = &line[i..];
        if rest.starts_with("{{--") {
            match rest.find("--}}") {
                Some(end) => {
                    out.push((i..i + end + 4, Scope::Comment));
                    i += end + 4;
                }
                None => {
                    out.push((i..line.len(), Scope::Comment));
                    *open_comment = true;
                    return out;
                }
            }
        } else if let Some(len) = echo(rest, "{!!", "!!}", &mut out, i) {
            i += len;
        } else if let Some(len) = echo(rest, "{{", "}}", &mut out, i) {
            i += len;
        } else if let Some(len) = component(rest, &mut out, i) {
            i += len;
        } else if rest.starts_with("@@") {
            // `@@if` is how a literal `@if` is written: it is not a directive,
            // and reporting it as one would be wrong.
            i += 2;
        } else if rest.starts_with('@') && starts_a_directive(line, i) {
            i += directive(rest, &mut out, i);
        } else {
            i += next_char(rest);
        }
    }
    out
}

/// What a Blade range is, independently of the name the theme gives its
/// colour.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Scope {
    Comment,
    /// `@if`, `@endforeach`: Blade's own vocabulary.
    Directive,
    /// What tells `{{ $x }}` apart from the text around it.
    Delimiter,
    /// What Blade has PHP evaluate — the inside of an echo, a directive's
    /// argument.
    Expression,
    /// A component tag's name: `<x-forms.input>`, `<livewire:…>`.
    Component,
}

impl Scope {
    /// Style names to try, from the most accurate to the most surely present.
    ///
    /// A theme does not have to define the whole nomenclature, and ours have
    /// neither `punctuation` nor `operator`: without a fallback, an echo's
    /// delimiters stayed the colour of the text, that is, invisible.
    fn candidates(self) -> &'static [&'static str] {
        match self {
            Scope::Comment => &["comment"],
            Scope::Directive => &["keyword"],
            Scope::Delimiter => &["punctuation.special", "tag"],
            Scope::Expression => &["embedded", "variable"],
            // A tag's colour, and not a colour of their own: a component *is* a
            // tag to whoever reads the view, and giving it another would suggest
            // a different construct.
            Scope::Component => &["tag", "keyword"],
        }
    }

    fn style(self, theme: &HighlightTheme) -> Option<gpui::HighlightStyle> {
        self.candidates().iter().find_map(|name| theme.style(name))
    }
}

/// A component tag's name: `<x-forms.input>`, `</x-layout.app>`,
/// `<livewire:counter>`. Returns the length consumed, delimiters included.
///
/// **The dot is this case's reason for being.** The HTML grammar knows no
/// dotted tag name: in `<x-layout.app>` it reads `x-layout` as a tag and `.app`
/// as an **attribute**, so the component's name is cut into two colours in its
/// middle. And a Laravel project's components live in subfolders, so the dot is
/// the rule there rather than the exception.
///
/// The whole name is repainted in one piece, which incidentally covers the
/// grammar's faulty reading — `apply` removes what overlaps.
fn component(rest: &str, out: &mut Vec<(Range<usize>, Scope)>, at: usize) -> Option<usize> {
    let after_bracket = rest.strip_prefix('<')?;
    let (closing, name) = match after_bracket.strip_prefix('/') {
        Some(name) => (1, name),
        None => (0, after_bracket),
    };
    // The two prefixes Laravel reserves for itself. Everything else is ordinary
    // HTML, which the grammar reads perfectly well itself.
    if !name.starts_with("x-") && !name.starts_with("livewire:") {
        return None;
    }
    let len = name
        .find(|c: char| !(c.is_alphanumeric() || matches!(c, '-' | '.' | ':' | '_')))
        .unwrap_or(name.len());
    let start = at + 1 + closing;
    out.push((start..start + len, Scope::Component));
    Some(1 + closing + len)
}

/// An echo `{{ … }}` or `{!! … !!}`. Returns the length consumed.
fn echo(
    rest: &str,
    open: &str,
    close: &str,
    out: &mut Vec<(Range<usize>, Scope)>,
    at: usize,
) -> Option<usize> {
    if !rest.starts_with(open) {
        return None;
    }
    out.push((at..at + open.len(), Scope::Delimiter));
    let body = &rest[open.len()..];
    match body.find(close) {
        Some(end) => {
            if end > 0 {
                out.push((at + open.len()..at + open.len() + end, Scope::Expression));
            }
            let stop = at + open.len() + end;
            out.push((stop..stop + close.len(), Scope::Delimiter));
            Some(open.len() + end + close.len())
        }
        // An echo that does not close on its line: the rest belongs to it all
        // the same, and the next line will start again from ordinary text.
        None => {
            if !body.is_empty() {
                out.push((at + open.len()..at + rest.len(), Scope::Expression));
            }
            Some(rest.len())
        }
    }
}

/// A directive `@name` and, if there is one, its parenthesised argument.
/// Returns the length consumed — at least 1, so the scanner moves on.
fn directive(rest: &str, out: &mut Vec<(Range<usize>, Scope)>, at: usize) -> usize {
    let name: usize = rest[1..]
        .chars()
        .take_while(|c| c.is_ascii_alphanumeric() || *c == '_')
        .map(char::len_utf8)
        .sum();
    if name == 0 {
        return 1;
    }
    out.push((at..at + 1 + name, Scope::Directive));
    let mut i = 1 + name;
    // Blade tolerates a space before the parenthesis (`@if ($x)`), but not a
    // newline: looking further would catch text that has nothing to do with it.
    let spaces: usize = rest[i..].chars().take_while(|c| *c == ' ').count();
    if !rest[i + spaces..].starts_with('(') {
        return i;
    }
    i += spaces;
    match argument(&rest[i..]) {
        Some(len) => {
            if len > 2 {
                out.push((at + i + 1..at + i + len - 1, Scope::Expression));
            }
            i + len
        }
        // Parenthesis not closed on the line: we leave the rest to ordinary
        // text rather than guess.
        None => i,
    }
}

/// Length of `(…)`, parentheses included, accounting for nesting and strings —
/// `@if ($x == ')')` is not rare.
fn argument(rest: &str) -> Option<usize> {
    let mut depth = 0usize;
    let mut quote: Option<char> = None;
    let mut escaped = false;
    for (i, c) in rest.char_indices() {
        if let Some(q) = quote {
            if escaped {
                escaped = false;
            } else if c == '\\' {
                escaped = true;
            } else if c == q {
                quote = None;
            }
            continue;
        }
        match c {
            '\'' | '"' => quote = Some(c),
            '(' => depth += 1,
            ')' => {
                // Cannot happen from `directive`, which only calls with an
                // opening parenthesis; returning `None` rather than subtracting
                // from zero keeps the function safe should it be used
                // elsewhere.
                if depth == 0 {
                    return None;
                }
                depth -= 1;
                if depth == 0 {
                    return Some(i + c.len_utf8());
                }
            }
            _ => {}
        }
    }
    None
}

/// True if the `@` at `at` opens a directive.
///
/// What precedes it decides: an e-mail address in the page body
/// (`contact@example.com`) and a stylesheet's `@media` have the same shape,
/// only the preceding character tells them apart.
fn starts_a_directive(line: &str, at: usize) -> bool {
    let before = line[..at].chars().next_back();
    match before {
        None => true,
        Some(c) => !(c.is_alphanumeric() || c == '_' || c == '.' || c == '-' || c == '@'),
    }
}

/// Length of the next character: advancing by one byte would cut an accented
/// character in two, and the ranges returned would no longer be valid
/// boundaries.
fn next_char(rest: &str) -> usize {
    rest.chars().next().map_or(1, char::len_utf8)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scan_line(line: &str) -> Vec<(Range<usize>, Scope)> {
        scan(line, &mut false)
    }

    /// Returns what a range covers, so the tests can be read without counting
    /// bytes by hand.
    fn covered<'a>(line: &'a str, found: &[(Range<usize>, Scope)]) -> Vec<(&'a str, Scope)> {
        found
            .iter()
            .map(|(r, scope)| (&line[r.clone()], *scope))
            .collect()
    }

    #[test]
    fn a_directive_and_its_argument_are_recognized() {
        let line = "    @foreach ($invoices as $invoice)";
        assert_eq!(
            covered(line, &scan_line(line)),
            [
                ("@foreach", Scope::Directive),
                ("$invoices as $invoice", Scope::Expression),
            ]
        );
    }

    #[test]
    fn a_directive_without_argument_stops_at_its_name() {
        let line = "@endforeach";
        assert_eq!(
            covered(line, &scan_line(line)),
            [("@endforeach", Scope::Directive)]
        );
    }

    #[test]
    fn an_echo_separates_its_delimiters_from_its_expression() {
        let line = "<td>{{ $invoice->total }}</td>";
        assert_eq!(
            covered(line, &scan_line(line)),
            [
                ("{{", Scope::Delimiter),
                (" $invoice->total ", Scope::Expression),
                ("}}", Scope::Delimiter),
            ]
        );
    }

    #[test]
    fn an_unescaped_echo_uses_its_own_delimiters() {
        let line = "{!! $html !!}";
        let found = scan_line(line);
        assert_eq!(
            covered(line, &found),
            [
                ("{!!", Scope::Delimiter),
                (" $html ", Scope::Expression),
                ("!!}", Scope::Delimiter),
            ],
            "otherwise the simple echo would bite into the unescaped one, and \
             shift the rest of the line"
        );
    }

    #[test]
    fn a_comment_spans_the_lines_it_needs() {
        let mut open = false;
        let first = "{{-- an explanation";
        assert_eq!(
            covered(first, &scan(first, &mut open)),
            [(first, Scope::Comment)]
        );
        assert!(open, "the comment stays open");

        let middle = "   that carries on";
        assert_eq!(scan(middle, &mut open).len(), 1);
        assert!(open);

        let last = "  --}} <p>rest</p>";
        let found = scan(last, &mut open);
        assert_eq!(&last[found[0].0.clone()], "  --}}");
        assert!(!open, "and closes again");
    }

    #[test]
    fn a_comment_that_closes_on_its_line_leaves_the_rest_alone() {
        let line = "{{-- hidden --}} @if ($x)";
        let found = scan_line(line);
        assert_eq!(
            covered(line, &found),
            [
                ("{{-- hidden --}}", Scope::Comment),
                ("@if", Scope::Directive),
                ("$x", Scope::Expression),
            ]
        );
    }

    /// The naive scanner's trap: not everything that looks like `@word` is a
    /// directive.
    #[test]
    fn what_looks_like_a_directive_but_is_not() {
        let line = "<a href=\"mailto:contact@example.com\">contact@example.com</a>";
        assert!(
            scan_line(line).is_empty(),
            "an e-mail address is not a directive"
        );

        let escaped = "@@if is not a directive";
        assert!(scan_line(escaped).is_empty());

        let alone = "@ on its own";
        assert!(scan_line(alone).is_empty());
    }

    #[test]
    fn an_argument_survives_nesting_and_quotes() {
        let line = "@if (str_contains($a, ')') && count($b))";
        let found = scan_line(line);
        assert_eq!(
            covered(line, &found),
            [
                ("@if", Scope::Directive),
                ("str_contains($a, ')') && count($b)", Scope::Expression),
            ]
        );
    }

    #[test]
    fn ranges_stay_sorted_disjoint_and_on_character_boundaries() {
        let line = "<p>Größe {{ $n }} — @if ($ok) yes @endif</p>";
        let found = scan_line(line);
        let mut last = 0;
        for (range, _) in &found {
            assert!(range.start >= last, "ranges not sorted: {found:?}");
            assert!(range.start < range.end);
            assert!(
                line.is_char_boundary(range.start) && line.is_char_boundary(range.end),
                "range {range:?} in the middle of a character"
            );
            last = range.end;
        }
        assert!(found.len() >= 5);
    }

    /// A range with no style is an invisible range: the role would give the
    /// right colour, but the theme would not know the name asked for.
    #[test]
    fn every_scope_resolves_to_a_colour() {
        for theme in [
            HighlightTheme::default_dark(),
            HighlightTheme::default_light(),
        ] {
            for scope in [
                Scope::Comment,
                Scope::Directive,
                Scope::Delimiter,
                Scope::Expression,
                Scope::Component,
            ] {
                assert!(
                    scope.style(&theme).is_some(),
                    "{scope:?} with no colour in \"{}\": tried {:?}",
                    theme.name,
                    scope.candidates()
                );
            }
        }
    }

    /// The case the HTML grammar cannot read: a dotted component name, which it
    /// cuts into a tag and an attribute.
    #[test]
    fn a_dotted_component_name_is_one_range() {
        let mut open = false;
        let line = "<x-layout.app title=\"Quote\">";
        let found = scan(line, &mut open);
        let component: Vec<_> = found
            .iter()
            .filter(|(_, scope)| *scope == Scope::Component)
            .collect();
        assert_eq!(component.len(), 1, "{found:?}");
        assert_eq!(&line[component[0].0.clone()], "x-layout.app");
        // The attribute that follows is not ours: the grammar colours it well.
        assert!(!line[component[0].0.clone()].contains("title"));
    }

    #[test]
    fn a_closing_component_and_livewire_count_too() {
        let mut open = false;
        let line = "</x-forms.input><livewire:counter :n=\"$n\" />";
        let found: Vec<_> = scan(line, &mut open)
            .into_iter()
            .filter(|(_, scope)| *scope == Scope::Component)
            .map(|(range, _)| line[range].to_string())
            .collect();
        assert_eq!(found, vec!["x-forms.input", "livewire:counter"]);
    }

    /// An ordinary tag belongs to the grammar, which reads it perfectly well:
    /// the overlay has no business touching it, at the risk of covering styles
    /// finer than its own.
    #[test]
    fn an_ordinary_tag_is_left_to_the_grammar() {
        let mut open = false;
        for line in ["<div class=\"x\">", "</section>", "<xml-ish>", "a < b"] {
            let found = scan(line, &mut open);
            assert!(
                !found.iter().any(|(_, scope)| *scope == Scope::Component),
                "{line}: {found:?}"
            );
        }
    }

    #[test]
    fn recognizes_a_view_by_its_full_name() {
        assert!(is_blade(Path::new("resources/views/invoice.blade.php")));
        assert!(is_blade(Path::new("layout.Blade.PHP")));
        assert!(!is_blade(Path::new("app/Models/Invoice.php")));
        assert!(!is_blade(Path::new("blade.php")));
    }
}
