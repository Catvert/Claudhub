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
//! (`{{-- --}}`), including across several lines — plus the two shapes that
//! also begin with an `@` and are **not** directives: Alpine's event bindings
//! (`@click.prevent="…"`) and Blade's escape (`@{{ … }}`, which hands the
//! braces to the JavaScript framework rather than reading them itself).
//!
//! Two things the overlay does not colour itself, because they are not Blade
//! but another language living inside it:
//!
//! - **The body of a `@php` block.** `mask_php` turns the block's markers into
//!   PHP tags of the very same byte length, so the view's own parse reads what
//!   is inside as the code it is.
//! - **The other fragments** — a directive's argument, an echo's body, an Alpine
//!   value. They are unreachable from that parse, sitting inside what the PHP
//!   grammar reads as text, so each is handed to the grammar it belongs to. See
//!   `Tint`.

use std::cell::RefCell;
use std::collections::HashMap;
use std::ops::Range;
use std::path::Path;
use std::rc::Rc;

use gpui::{HighlightStyle, SharedString};
use gpui_component::highlighter::{HighlightTheme, SyntaxHighlighter};
use gpui_component::input::{
    EditorState, FoldRange, HighlightStyleResolver, InputEdit, InputHighlighter,
    InputHighlighterFactory, Rope,
};

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
    // Built once for the whole file and only for the languages it holds — see
    // `Tint`. A diff arrives once; this is not a per-frame cost.
    let mut tint = Tint::default();
    for (h, hunk) in diff.hunks.iter().enumerate() {
        // Comment state starts again from scratch at each hunk: what separates
        // them has been elided, and nothing says a `{{--` left open above was
        // not closed inside the gap.
        let mut state = State::default();
        for (l, line) in hunk.lines.iter().enumerate() {
            let found = scan(&line.text, &mut state);
            let Some(target) = styles.get_mut(h).and_then(|h| h.get_mut(l)) else {
                continue;
            };
            apply(&styled(&found, &line.text, theme, &mut tint), target);
        }
    }
}

/// What a `@php` block's markers become for the grammar, and the reason the
/// code inside one is coloured at all.
///
/// A `@php … @endphp` block holds PHP, and the grammar knows nothing of the two
/// markers: it reads the whole block as HTML text, so a dozen lines of real code
/// arrived grey in the middle of a coloured view. The overlay cannot fix that on
/// its own — colouring the body itself would mean carrying a second PHP parse
/// through a scanner that reads one line at a time.
///
/// So the line handed to the grammar is not quite the line on screen: `@php`
/// becomes `<?` and `@endphp` becomes `?>`, each padded with spaces to **exactly
/// the same number of bytes** — four and seven. That is the whole trick, and the
/// byte count is what makes it safe: every offset the grammar returns still
/// designates the same character of the real line, so nothing has to be shifted
/// back. The grammar then reads the block as the PHP it is — `<?` on its own is
/// a tag it accepts — and the overlay repaints the two markers as directives
/// over what it made of them.
///
/// Returns `None` when the line holds no block marker, which is almost every
/// line: masking must not cost an allocation per line of a view.
///
/// `state` is the scanner's, and for the same reason: a `@php` written inside
/// `{{-- … --}}` is a comment and must not open anything.
pub fn mask_php(line: &str, state: &mut State) -> Option<String> {
    let found = scan(line, state);
    if !found
        .iter()
        .any(|(_, scope)| matches!(scope, Scope::PhpOpen | Scope::PhpClose))
    {
        return None;
    }
    let mut masked = line.to_string();
    for (range, scope) in &found {
        let tag = match scope {
            Scope::PhpOpen => "<?  ",
            Scope::PhpClose => "?>     ",
            _ => continue,
        };
        debug_assert_eq!(range.len(), tag.len(), "the mask changes the offsets");
        if range.len() == tag.len() {
            masked.replace_range(range.clone(), tag);
        }
    }
    Some(masked)
}

/// Replaces in `target` whatever the overlay covers.
///
/// The grammar's styles touching a Blade range are removed rather than layered:
/// rendering expects sorted, disjoint ranges, and a half-covered keyword means
/// nothing anyway.
fn apply(styled: &[(Range<usize>, HighlightStyle)], target: &mut LineStyles) {
    if styled.is_empty() {
        return;
    }
    target.retain(|(range, _)| {
        !styled
            .iter()
            .any(|(over, _)| range.start < over.end && over.start < range.end)
    });
    target.extend(styled.iter().cloned());
    target.sort_by_key(|(range, _)| range.start);
}

/// What one line of a view leaves open for the next.
///
/// Two things do, and both would otherwise be read as ordinary text on the line
/// that continues them: a `{{--` with no `--}}` yet, and an Alpine value whose
/// closing quote is further down — `x-effect="…"` spanning two lines is the
/// common way of writing one.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct State {
    comment: bool,
    /// The quote that will close the attribute value being read, and the
    /// language of what is inside it.
    value: Option<(char, Scope)>,
}

/// Cuts a line into Blade ranges, each with the style name it deserves. The
/// ranges returned are sorted and disjoint.
fn scan(line: &str, state: &mut State) -> Vec<(Range<usize>, Scope)> {
    let mut out = Vec::new();
    let mut i = 0;

    if state.comment {
        match line.find("--}}") {
            Some(end) => {
                out.push((0..end + 4, Scope::Comment));
                state.comment = false;
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

    if let Some((quote, scope)) = state.value {
        match line.find(quote) {
            Some(end) => {
                if end > 0 {
                    out.push((0..end, scope));
                }
                state.value = None;
                i = end + quote.len_utf8();
            }
            None => {
                if !line.is_empty() {
                    out.push((0..line.len(), scope));
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
                    state.comment = true;
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
        } else if rest.starts_with("@{") {
            // Blade's escape: `@{{ count }}` outputs the braces instead of
            // reading them, which is how an Alpine or Vue expression survives a
            // view. Only the `@` is consumed — enough for what follows not to be
            // taken for an echo, which is the whole point of writing it.
            i += 2;
        } else if let Some(len) = binding(rest, line, i, &mut out, state) {
            i += len;
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
    /// A `@php` opening a block, and the `@endphp` closing it. The colour is a
    /// directive's — they are directives — but they are told apart because
    /// `mask_php` has to find them again.
    PhpOpen,
    PhpClose,
    /// An Alpine or Livewire event binding: `@click`, `@keydown.escape.window`.
    /// It begins with an `@` and is not a directive at all.
    Attribute,
    /// The JavaScript inside an Alpine value: `x-data`, `x-effect`, `@click`.
    /// The colour is only the fallback — what is readable is handed to the
    /// JavaScript grammar, and this is what shows through where it says nothing.
    Script,
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
            Scope::Directive | Scope::PhpOpen | Scope::PhpClose => &["keyword"],
            // The colour the HTML grammar gives the attributes next to it: an
            // `@click` *is* an attribute, and the grammar itself reads `x-data`
            // and `:class` that way.
            Scope::Attribute => &["attribute", "property", "tag"],
            // The colour an attribute's value already had: what the grammar
            // makes of a quoted string.
            Scope::Script => &["string"],
            Scope::Delimiter => &["punctuation.special", "tag"],
            Scope::Expression => &["embedded", "variable"],
            // A tag's colour, and not a colour of their own: a component *is* a
            // tag to whoever reads the view, and giving it another would suggest
            // a different construct.
            Scope::Component => &["tag", "keyword"],
        }
    }

    /// A theme is a resolver: the diff holds one, the editor is handed one, and
    /// both ask the same names in the same order.
    fn resolve(self, resolver: &dyn HighlightStyleResolver) -> Option<HighlightStyle> {
        self.candidates()
            .iter()
            .find_map(|name| resolver.style(name))
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

/// An attribute whose value is not text but code, and the language it is in:
/// `@click.prevent="open = !open"`, `x-data="{ tab: 1 }"` and `x-on:click="…"`
/// hold **JavaScript**, `:label="__('…')"` holds **PHP**. Returns the length
/// consumed.
///
/// **For an `@` name, it is the `=` that tells it from a directive**, and
/// nothing else: `@click` and `@if` have exactly the same shape, and only an
/// attribute is immediately followed by its value. Painting it as a directive
/// was wrong twice over — it took the keyword colour in the middle of a tag, and
/// it *removed* the attribute colour the HTML grammar had given it, `apply`
/// replacing what it covers.
///
/// The value is marked with its language so it can be handed to the right
/// grammar, and an unclosed quote is left open in `state`: an `x-effect` running
/// over two lines is how anyone writes one.
///
/// **`:name="…"` is read as PHP, and that is a convention, not a deduction.**
/// The shorthand is ambiguous by construction: on a component tag it is a Blade
/// prop, on an ordinary tag it is Alpine's `x-bind`. Telling them apart means
/// knowing which tag is open, which this line-by-line scanner does not. Blade
/// wins because Alpine has a spelling that says so — `x-bind:class` — and a
/// project that uses it has nothing ambiguous left; one that writes `:class` for
/// Alpine gets a PHP reading of a JavaScript expression, which comes back
/// looking like the string it was, `fragment` colouring nothing it cannot read.
fn binding(
    rest: &str,
    line: &str,
    at: usize,
    out: &mut Vec<(Range<usize>, Scope)>,
    state: &mut State,
) -> Option<usize> {
    if !starts_a_directive(line, at) {
        return None;
    }
    let event = rest.starts_with('@');
    // Which language the value is in, decided by the prefix alone.
    let inside = if event || rest.starts_with("x-") {
        Scope::Script
    } else if rest.starts_with(':') {
        Scope::Expression
    } else {
        return None;
    };
    // The modifiers are part of the name — `@click.prevent.stop` is one
    // attribute — and so is the `:` of `x-on:click`.
    let head = usize::from(event || inside == Scope::Expression);
    let name = rest[head..]
        .find(|c: char| !(c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.' | ':')))
        .unwrap_or(rest.len() - head);
    if name == 0 || !rest[head + name..].starts_with('=') {
        return None;
    }
    // An `@` name is ours to paint; `x-data` the grammar already reads as the
    // attribute it is.
    if event {
        out.push((at..at + 1 + name, Scope::Attribute));
    }
    let mut i = head + name + 1;
    let Some(quote) = rest[i..].chars().next().filter(|c| matches!(c, '"' | '\'')) else {
        return Some(i);
    };
    i += quote.len_utf8();
    match rest[i..].find(quote) {
        Some(end) => {
            if end > 0 {
                out.push((at + i..at + i + end, inside));
            }
            Some(i + end + quote.len_utf8())
        }
        None => {
            if i < rest.len() {
                out.push((at + i..at + rest.len(), inside));
            }
            state.value = Some((quote, inside));
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
    let mut i = 1 + name;
    // Blade tolerates a space before the parenthesis (`@if ($x)`), but not a
    // newline: looking further would catch text that has nothing to do with it.
    let spaces: usize = rest[i..].chars().take_while(|c| *c == ' ').count();
    let argued = rest[i + spaces..].starts_with('(');
    // `@php($total = 0)` is a statement, not a block: only the block form has an
    // `@endphp` in front of it, and only it is masked.
    let scope = match &rest[1..1 + name] {
        "php" if !argued => Scope::PhpOpen,
        "endphp" => Scope::PhpClose,
        _ => Scope::Directive,
    };
    out.push((at..at + 1 + name, scope));
    if !argued {
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

/// The two grammars a view holds inside itself, kept between calls.
///
/// A directive's argument and an echo's body are **PHP**; an Alpine value is
/// **JavaScript**. Neither is reachable from the view's own parse: they live
/// inside what the PHP grammar reads as text, so the only way to colour them is
/// to hand each fragment to the grammar it belongs to.
///
/// The reason this is a struct and not two calls is `SyntaxHighlighter::new`,
/// which compiles a grammar's queries — tens of milliseconds. It is paid once
/// per view, and only for a language the view actually holds: a page with no
/// Alpine never builds the JavaScript one.
#[derive(Default)]
pub struct Tint {
    php: Option<SyntaxHighlighter>,
    js: Option<SyntaxHighlighter>,
}

impl Tint {
    /// Colours one fragment, in offsets relative to it.
    fn of(
        &mut self,
        scope: Scope,
        code: &str,
        resolver: &dyn HighlightStyleResolver,
    ) -> Vec<(Range<usize>, HighlightStyle)> {
        // What has to be written in front of the fragment for its grammar to
        // recognise it, and what has to be written after — the same reasoning as
        // `highlight::prologue`, one level down.
        let (before, after) = match scope {
            // Without an opening tag, the PHP grammar reads the fragment as
            // HTML text and not one colour comes out. And without the
            // semicolon, a fragment that is a bare expression — `true`, an enum
            // case, anything a `:prop` holds — is not a statement, so the parse
            // is an `ERROR` node and the query matches nothing inside it. The
            // one character is the difference between a coloured value and a
            // grey one.
            Scope::Expression => ("<?php ", ";"),
            // `x-data="{ tab: 1 }"` is an expression, and a JavaScript program
            // beginning with a brace is a *block*: `tab:` would be a label. The
            // parentheses are what make it the object it is. Anything else is
            // statements, which take no wrapping.
            Scope::Script if code.trim_start().starts_with('{') => ("(", ")"),
            Scope::Script => ("", ""),
            _ => return Vec::new(),
        };
        let highlighter = match scope {
            Scope::Expression => self
                .php
                .get_or_insert_with(|| SyntaxHighlighter::new("php")),
            _ => self
                .js
                .get_or_insert_with(|| SyntaxHighlighter::new("javascript")),
        };

        let source = format!("{before}{code}{after}");
        highlighter.update(None, &Rope::from_str(&source), None);
        highlighter
            .styles(&(0..source.len()), resolver)
            .into_iter()
            // The prologue belongs to no part of the fragment; what straddles
            // its end is clipped back to it.
            .filter(|(range, style)| range.end > before.len() && style.color.is_some())
            .map(|(range, style)| {
                let start = range.start.saturating_sub(before.len());
                let end = (range.end - before.len()).min(code.len());
                (start..end, style)
            })
            .filter(|(range, _)| range.start < range.end)
            .collect()
    }
}

/// One line's Blade ranges turned into styles, fragments handed to their own
/// grammar on the way.
///
/// `text` is what the ranges are offsets into — a diff line for the overlay, the
/// whole document for the editor — and what comes back is in the same
/// coordinates, sorted and disjoint.
fn styled(
    found: &[(Range<usize>, Scope)],
    text: &str,
    resolver: &dyn HighlightStyleResolver,
    tint: &mut Tint,
) -> Vec<(Range<usize>, HighlightStyle)> {
    let mut out = Vec::with_capacity(found.len());
    for (range, scope) in found {
        let Some(fallback) = scope.resolve(resolver) else {
            continue;
        };
        if matches!(scope, Scope::Expression | Scope::Script) {
            out.extend(fragment(tint, *scope, range, text, fallback, resolver));
        } else {
            out.push((range.clone(), fallback));
        }
    }
    out
}

/// One fragment's styles, in the containing text's offsets, **with no holes**.
///
/// It is all or nothing, and the two cases are what makes the attempt safe:
///
/// - **The grammar said something**, so the fragment is code and reads as code.
///   What it did not name — an identifier, an operator, a space — takes the
///   plain text colour, exactly as it would in a file of that language. Leaving
///   those bytes the value's own colour was what made `id = $wire.` and
///   `prevEditId !==` come back green in the middle of coloured JavaScript: the
///   string colour spread over everything the grammar had no word for, and the
///   value read as a string with a few keywords in it.
/// - **It said nothing at all** — an expression it cannot parse, a value that is
///   one bare identifier — and the fragment keeps the single colour it had. Not
///   trying is exactly what happened before, so nothing can look worse than it
///   did.
fn fragment(
    tint: &mut Tint,
    scope: Scope,
    range: &Range<usize>,
    text: &str,
    fallback: HighlightStyle,
    resolver: &dyn HighlightStyleResolver,
) -> Vec<(Range<usize>, HighlightStyle)> {
    let inner = tint.of(scope, &text[range.clone()], resolver);
    if inner.is_empty() {
        return vec![(range.clone(), fallback)];
    }
    let plain = HighlightStyle::default();
    let mut out = Vec::with_capacity(inner.len() * 2 + 1);
    let mut at = range.start;
    for (piece, style) in inner {
        let (start, end) = (range.start + piece.start, range.start + piece.end);
        if start > at {
            out.push((at..start, plain));
        }
        out.push((start..end, style));
        at = end;
    }
    if at < range.end {
        out.push((at..range.end, plain));
    }
    out
}

/// The highlighter a Blade view gets in the **editor**.
///
/// The overlay above serves the diff, which owns its own painting; the editor
/// does not — `EditorState` asks a highlighter for styled runs and paints them
/// itself. A view opened there therefore had nothing of Blade at all: the PHP
/// grammar read the whole file as HTML text, so the body of a `@php` block, the
/// directives, the echoes and the comments all arrived grey, while the tags
/// around them were coloured. The seam that fixes it is
/// `gpui_base::input::InputHighlighter`, which `set_highlighter_factory`
/// installs — that is why `gpui-base` is a direct dependency here as it is in
/// `theme.rs`.
///
/// What it does is what the diff does, in the same order and with the same two
/// functions: hand the grammar a text whose `@php` markers are masked into PHP
/// tags, then lay Blade's own ranges over what comes back.
pub struct BladeHighlighter {
    /// The PHP grammar, with the HTML injection our registry gives it.
    inner: SyntaxHighlighter,
    /// The document as it stands, which the fragments are slices of.
    source: String,
    /// Blade's ranges over the whole document, sorted and disjoint, in the
    /// **real** text's byte offsets — the mask preserves them.
    blade: Vec<(Range<usize>, Scope)>,
    /// The styles those ranges resolve to, kept from one frame to the next.
    painted: RefCell<Painted>,
}

/// The fragments already coloured, kept from one frame to the next.
///
/// `styles` is called for every visible group of lines, at every frame, and
/// colouring a fragment means parsing it: a view of three hundred echoes costs
/// seventy milliseconds to paint whole, which is a keystroke's worth of freeze
/// on every keystroke. Two things bring it back to nothing.
///
/// **Only what is asked for is painted.** The editor asks for the lines it is
/// about to draw, so an edit costs the forty fragments on screen, not the twelve
/// hundred in the file.
///
/// **And each is kept**, keyed by where it starts. What invalidates the lot: an
/// edit, which `refresh` reports — the offsets have moved — and a change of
/// theme, which nobody reports, the resolver arriving as a bare `&dyn` with no
/// identity to compare. So a **witness** is kept: the style of one name,
/// resolved again at each call. A theme that paints keywords differently is a
/// theme that changed, and one hash lookup a frame is the whole cost of
/// noticing.
#[derive(Default)]
struct Painted {
    witness: Option<HighlightStyle>,
    tint: Tint,
    done: HashMap<usize, Vec<(Range<usize>, HighlightStyle)>>,
}

/// The name whose colour stands for the theme. Any would do; a keyword is the
/// one every theme in this repository defines.
const WITNESS: &str = "keyword";

/// The factory to hand `EditorState::set_highlighter_factory`.
///
/// It ignores the language it is given: it is only installed on a file
/// `is_blade` recognises, and the language `EditorState` carries there is `php`.
pub fn input_highlighter_factory() -> InputHighlighterFactory {
    Rc::new(|_| Some(Box::new(BladeHighlighter::new()) as Box<dyn InputHighlighter>))
}

/// Beyond this, the file is left to the grammar alone: rescanning and reparsing
/// a whole document at every keystroke is only cheap while the document is
/// small, and a Blade view is. The editor refuses anything over
/// `files::MAX_LINES` anyway; this is the second bound, in bytes.
const MAX_BYTES: usize = 256 * 1024;

impl Default for BladeHighlighter {
    fn default() -> Self {
        Self::new()
    }
}

impl BladeHighlighter {
    pub fn new() -> Self {
        Self {
            inner: SyntaxHighlighter::new("php"),
            source: String::new(),
            blade: Vec::new(),
            painted: RefCell::default(),
        }
    }

    /// Rescans and reparses a document. Apart from the two bounds, this is all
    /// `update` does — and it is what a test can call, `update` asking for a
    /// window and a context that no test has.
    fn refresh(&mut self, source: &str) {
        self.blade = scan_document(source);
        let masked = mask_document(source);
        self.inner.update(
            None,
            &Rope::from_str(masked.as_deref().unwrap_or(source)),
            None,
        );
        self.painted.borrow_mut().done.clear();
        self.source.clear();
        self.source.push_str(source);
    }

    /// Blade's styles over a window, the fragments in it painted or recalled —
    /// see `Painted`.
    fn painted(
        &self,
        window: &Range<usize>,
        resolver: &dyn HighlightStyleResolver,
    ) -> Vec<(Range<usize>, HighlightStyle)> {
        let mut painted = self.painted.borrow_mut();
        let witness = resolver.style(WITNESS);
        if painted.witness != witness {
            // The colours are stale, the grammars are not: rebuilding them here
            // would pay their query compilation again at every change of theme.
            painted.witness = witness;
            painted.done.clear();
        }
        // Split so the cache and the grammars can be borrowed at once.
        let Painted { tint, done, .. } = &mut *painted;

        let mut out = Vec::new();
        for (range, scope) in &self.blade {
            if range.end <= window.start || window.end <= range.start {
                continue;
            }
            let Some(fallback) = scope.resolve(resolver) else {
                continue;
            };
            if !matches!(scope, Scope::Expression | Scope::Script) {
                out.push((range.clone(), fallback));
                continue;
            }
            let styles = done
                .entry(range.start)
                .or_insert_with(|| fragment(tint, *scope, range, &self.source, fallback, resolver));
            out.extend(styles.iter().cloned());
        }
        out
    }
}

/// The styles of a whole view, for a reader that is not the editor.
///
/// The search preview shows a file it does not edit: it has no `EditorState`,
/// so no `InputHighlighter` is installed for it, and a Blade view would arrive
/// with its directives, echoes and `@php` blocks read as plain HTML text. This
/// is the same object doing the same work, asked once for the whole document
/// rather than once per visible group of lines — a preview is read, not typed
/// in, so there is nothing to keep from one frame to the next.
pub fn document_styles(text: &str, theme: &HighlightTheme) -> Vec<(Range<usize>, HighlightStyle)> {
    if text.is_empty() || text.len() > MAX_BYTES {
        return Vec::new();
    }
    BladeHighlighter::new().document_styles(text, theme)
}

impl BladeHighlighter {
    /// The same, on a highlighter one keeps between documents.
    ///
    /// `new` compiles the PHP queries — tens of milliseconds — and a search
    /// list holds up to `git::search::MAX_HITS` lines, each of them its own
    /// little document. Rebuilding the grammar for every one of them is the
    /// grammar-per-language rule of `HitHighlights` broken one level down, and
    /// it is a minute of frozen window rather than a slow list.
    pub fn document_styles(
        &mut self,
        text: &str,
        theme: &HighlightTheme,
    ) -> Vec<(Range<usize>, HighlightStyle)> {
        if text.is_empty() || text.len() > MAX_BYTES {
            return Vec::new();
        }
        self.refresh(text);
        InputHighlighter::styles(self, &(0..text.len()), theme)
    }
}

impl InputHighlighter for BladeHighlighter {
    fn language(&self) -> SharedString {
        self.inner.language().clone()
    }

    /// **The edit is dropped and the document reparsed whole**, which is the one
    /// thing here that looks like a waste and is not.
    ///
    /// An incremental edit describes a change against the text tree-sitter last
    /// saw — and what it last saw is the *masked* text. The two agree byte for
    /// byte until the very keystroke that completes a `@php`, at which point
    /// four bytes far from the caret change meaning and the edit no longer
    /// describes anything real. The parse would stay wrong, and nothing would
    /// ever come back to correct it. A whole view is a few tens of kilobytes,
    /// which tree-sitter parses in a millisecond or two.
    fn update(
        &mut self,
        _edit: Option<InputEdit>,
        text: &Rope,
        _folding: bool,
        _window: &mut gpui::Window,
        _cx: &mut gpui::Context<EditorState>,
    ) {
        if text.len() > MAX_BYTES {
            return;
        }
        self.refresh(&text.to_string());
    }

    fn styles(
        &self,
        range: &Range<usize>,
        resolver: &dyn HighlightStyleResolver,
    ) -> Vec<(Range<usize>, HighlightStyle)> {
        let base = self.inner.styles(range, resolver);
        let over = self.painted(range, resolver);
        merge(range, base, &over)
    }

    fn fold_ranges(&self, _: &Rope) -> Vec<FoldRange> {
        self.inner.tree().map(folds).unwrap_or_default()
    }
}

/// Lays the overlay over the grammar's runs, keeping them sorted, disjoint and
/// **covering `range` whole** — the contract `InputHighlighter` states and
/// nothing checks.
///
/// Both lists are sorted, so a single walk over the range suffices: at every
/// position the overlay wins where it reaches, the grammar answers everywhere
/// else, and what neither covers takes the default style. Walking the range
/// rather than the grammar's runs is what keeps a Blade range that falls in a
/// gap — the grammar leaves them — from being dropped in silence.
fn merge(
    range: &Range<usize>,
    base: Vec<(Range<usize>, HighlightStyle)>,
    over: &[(Range<usize>, HighlightStyle)],
) -> Vec<(Range<usize>, HighlightStyle)> {
    if over.is_empty() {
        return base;
    }
    let mut out: Vec<(Range<usize>, HighlightStyle)> = Vec::with_capacity(base.len() + over.len());
    // An overlay range covering several of the grammar's runs would otherwise
    // leave one run per cut, all carrying the same style: `@php` came out in two
    // pieces, the mask having made `<?` a token of its own.
    {
        let mut push = |range: Range<usize>, style: HighlightStyle| match out.last_mut() {
            Some((last, last_style)) if last.end == range.start && *last_style == style => {
                last.end = range.end;
            }
            _ => out.push((range, style)),
        };

        let (mut at, mut b, mut o) = (range.start, 0usize, 0usize);
        while at < range.end {
            // A range spanning several of the other list's is only left behind once
            // it has been consumed whole.
            while b < base.len() && base[b].0.end <= at {
                b += 1;
            }
            while o < over.len() && over[o].0.end <= at {
                o += 1;
            }
            if let Some((covering, style)) = over.get(o).filter(|(r, _)| r.start <= at) {
                let end = covering.end.min(range.end);
                push(at..end, *style);
                at = end;
                continue;
            }
            // Nothing is painted past the next overlay range: it is what cuts the
            // grammar's run in two.
            let stop = over
                .get(o)
                .map_or(range.end, |(r, _)| r.start.min(range.end));
            let (end, style) = match base.get(b) {
                Some((run, style)) if run.start <= at => (run.end.min(stop), *style),
                Some((run, _)) => (run.start.min(stop), HighlightStyle::default()),
                None => (stop, HighlightStyle::default()),
            };
            push(at..end.min(range.end), style);
            at = end.min(range.end);
        }
    }
    out
}

/// The fold ranges of a parsed tree: every named node spanning at least three
/// lines.
///
/// It is `gpui-component`'s own rule, rewritten here because the function that
/// carries it is private to its adapter — and installing a highlighter of our
/// own means taking on everything the default one provided, folds included. The
/// tree is the masked text's, whose lines are the real ones.
fn folds(tree: &tree_sitter::Tree) -> Vec<FoldRange> {
    fn collect(node: tree_sitter::Node, out: &mut Vec<FoldRange>) {
        let (start, end) = (node.start_position().row, node.end_position().row);
        if end.saturating_sub(start) < 2 {
            return;
        }
        out.push(FoldRange::new(start, end));
        let mut cursor = node.walk();
        for child in node.named_children(&mut cursor) {
            collect(child, out);
        }
    }

    let root = tree.root_node();
    let mut out = Vec::new();
    let mut cursor = root.walk();
    for child in root.named_children(&mut cursor) {
        collect(child, &mut out);
    }
    out.sort_by_key(|range| range.start_line);
    out.dedup_by_key(|range| range.start_line);
    out
}

/// Blade's ranges over a whole document, in absolute byte offsets.
fn scan_document(text: &str) -> Vec<(Range<usize>, Scope)> {
    let mut out = Vec::new();
    let mut state = State::default();
    for (at, line) in lines(text) {
        out.extend(
            scan(line, &mut state)
                .into_iter()
                .map(|(range, scope)| (at + range.start..at + range.end, scope)),
        );
    }
    out
}

/// The whole document with its `@php` markers masked, or `None` if it holds
/// none — see `mask_php`.
fn mask_document(text: &str) -> Option<String> {
    let mut masked: Option<String> = None;
    let mut state = State::default();
    for (at, line) in lines(text) {
        let Some(tagged) = mask_php(line, &mut state) else {
            continue;
        };
        let whole = masked.get_or_insert_with(|| text.to_string());
        whole.replace_range(at..at + line.len(), &tagged);
    }
    masked
}

/// The lines of a text with their byte offset, terminator excluded.
///
/// The terminator is left out on purpose: a comment left open would otherwise
/// carry a range over the newline, and a style is easier to reason about when it
/// stops where the line does.
fn lines(text: &str) -> impl Iterator<Item = (usize, &str)> {
    let mut at = 0;
    text.split_inclusive('\n').map(move |line| {
        let start = at;
        at += line.len();
        (start, line.trim_end_matches('\n').trim_end_matches('\r'))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scan_line(line: &str) -> Vec<(Range<usize>, Scope)> {
        scan(line, &mut State::default())
    }

    /// Returns what a range covers, so the tests can be read without counting
    /// bytes by hand.
    fn covered<'a>(line: &'a str, found: &[(Range<usize>, Scope)]) -> Vec<(&'a str, Scope)> {
        found
            .iter()
            .map(|(r, scope)| (&line[r.clone()], *scope))
            .collect()
    }

    /// The editor's whole point, and what a screenshot showed missing: a view
    /// opened there had the tags around a `@php` block coloured and everything
    /// inside it grey.
    #[test]
    fn the_editor_colours_a_view_the_way_the_diff_does() {
        crate::ui::highlight::register_languages();
        let source = "@php\n\
                      use App\\Http\\Livewire\\Forms\\ContractForm;\n\
                      $can = ActionPermissionHelper::can();\n\
                      @endphp\n\
                      {{-- a note --}}\n\
                      <div x-data=\"{ tab: 1 }\" @click=\"go()\">\n\
                      @if ($can)\n\
                      <x-form-model-dialog />\n\
                      @endif\n\
                      </div>\n";
        let theme = HighlightTheme::default_dark();
        let mut highlighter = BladeHighlighter::new();
        highlighter.refresh(source);
        let styles = highlighter.styles(&(0..source.len()), &*theme);

        let coloured = |what: &str| {
            let at = source.find(what).expect("the fixture holds it");
            styles
                .iter()
                .any(|(r, style)| r.start <= at && at < r.end && style.color.is_some())
        };
        // The block's body, which is the whole reason this highlighter exists.
        assert!(coloured("use"), "the `use` of a @php block stays grey");
        assert!(coloured("ActionPermissionHelper"));
        // And Blade's own vocabulary, which the grammar knows nothing of.
        for what in ["@php", "@endphp", "{{-- a note --}}", "@if", "@endif"] {
            assert!(coloured(what), "{what} has no colour");
        }
        // What the grammar reads well is still its own.
        assert!(coloured("x-form-model-dialog") && coloured("x-data"));
    }

    /// What is inside a directive's argument and inside an echo is PHP, and it
    /// arrived flat: one colour for `$invoice->total()` as for `'net'`.
    #[test]
    fn a_directive_argument_and_an_echo_are_php() {
        crate::ui::highlight::register_languages();
        let source = "@if (count($lines) > 0)\n<td>{{ $invoice->total() }}</td>\n@endif\n";
        let theme = HighlightTheme::default_dark();
        let mut highlighter = BladeHighlighter::new();
        highlighter.refresh(source);
        let styles = highlighter.styles(&(0..source.len()), &*theme);

        let colour = |what: &str| {
            let at = source.find(what).expect("the fixture holds it");
            styles
                .iter()
                .find(|(r, _)| r.start <= at && at < r.end)
                .and_then(|(_, style)| style.color)
        };
        // A call and a number are not the same thing as the text around them.
        assert!(colour("count") != colour(" > "), "the argument stays flat");
        assert!(colour("0") != colour(" > "));
        assert!(colour("total") != colour(" }}"), "the echo stays flat");
    }

    /// And what is inside an Alpine value is JavaScript.
    #[test]
    fn an_alpine_value_is_javascript() {
        crate::ui::highlight::register_languages();
        let source = "<div x-data=\"{ tab: 1, open: false }\" x-effect=\"if (open) { go(); }\">\n";
        let theme = HighlightTheme::default_dark();
        let mut highlighter = BladeHighlighter::new();
        highlighter.refresh(source);
        let styles = highlighter.styles(&(0..source.len()), &*theme);

        let colour = |what: &str| {
            let at = source.find(what).expect("the fixture holds it");
            styles
                .iter()
                .find(|(r, _)| r.start <= at && at < r.end)
                .and_then(|(_, style)| style.color)
        };
        // `x-data` is an expression: parenthesised, its braces are an object and
        // `tab` a key. Read as a program it would be a block, and `tab` a label.
        assert!(
            colour("tab") != colour(", "),
            "x-data is not read as an object"
        );
        assert!(
            colour("if") != colour(" (open)"),
            "x-effect keeps its keywords"
        );
    }

    /// Once a value is read as code it reads as code whole: what the grammar
    /// names is coloured, the rest is plain text — never the value's own colour,
    /// which spread over every identifier and made JavaScript look like a string
    /// with keywords in it.
    #[test]
    fn a_value_read_as_code_leaves_no_colour_on_the_rest() {
        crate::ui::highlight::register_languages();
        let source = "<x-input x-model=\"data.schemeCode\" x-on:click=\"go('x')\" />\n";
        let theme = HighlightTheme::default_dark();
        let mut highlighter = BladeHighlighter::new();
        highlighter.refresh(source);
        let styles = highlighter.styles(&(0..source.len()), &*theme);

        let colour = |what: &str| {
            let at = source.find(what).expect("the fixture holds it");
            styles
                .iter()
                .find(|(r, _)| r.start <= at && at < r.end)
                .expect("every byte is covered")
                .1
                .color
        };
        assert!(colour("schemeCode").is_some(), "a property is named");
        assert!(colour("data").is_none(), "an identifier is not");
        assert!(colour("'x'").is_some(), "a string is");
        // And the value's own colour is nowhere in it any more.
        let string_colour = Scope::Script.resolve(&*theme).and_then(|s| s.color);
        assert!(string_colour.is_some() && colour("data") != string_colour);
    }

    /// The shorthand `:name="…"` is Blade, by convention: Alpine has a spelling
    /// that says otherwise, and it is the one to use.
    #[test]
    fn the_shorthand_binding_is_php_and_x_bind_is_javascript() {
        crate::ui::highlight::register_languages();
        let source =
            "<x-dialog :items=\"$invoice->lines\" x-bind:class=\"{ open: tab === 1 }\" />\n";
        let theme = HighlightTheme::default_dark();
        let mut highlighter = BladeHighlighter::new();
        highlighter.refresh(source);
        let styles = highlighter.styles(&(0..source.len()), &*theme);

        let colour = |what: &str| {
            let at = source.find(what).expect("the fixture holds it");
            styles
                .iter()
                .find(|(r, _)| r.start <= at && at < r.end)
                .expect("every byte is covered")
                .1
                .color
        };
        // PHP: an arrow reads a property, and `$invoice` is a plain variable.
        assert!(colour("lines").is_some() && colour("$invoice").is_none());
        // JavaScript, on the other side of the same tag.
        assert!(colour("1").is_some(), "a number in an x-bind expression");
        // Neither is the string colour the value had before.
        let string_colour = Scope::Script.resolve(&*theme).and_then(|s| s.color);
        assert!(colour("lines") != string_colour);
    }

    /// What a `:prop` holds is most often a bare expression — a boolean, an
    /// enum case — and a bare expression is not a statement: the parse is an
    /// `ERROR` and the query matches nothing inside it. Hence the semicolon
    /// `Tint::of` appends, and hence this test.
    #[test]
    fn a_bare_expression_is_coloured_like_the_code_it_is() {
        crate::ui::highlight::register_languages();
        let source = "<x-btn :fullHeight=\"true\" :color=\"ActionColor::Success\" />\n";
        let theme = HighlightTheme::default_dark();
        let mut highlighter = BladeHighlighter::new();
        highlighter.refresh(source);
        let styles = highlighter.styles(&(0..source.len()), &*theme);

        let colour = |what: &str| {
            let at = source.find(what).expect("the fixture holds it");
            styles
                .iter()
                .find(|(r, _)| r.start <= at && at < r.end)
                .expect("every byte is covered")
                .1
                .color
        };
        let flat = Scope::Expression.resolve(&*theme).and_then(|s| s.color);
        assert!(colour("true").is_some() && colour("true") != flat);
        assert!(colour("Success").is_some() && colour("Success") != flat);
        assert!(
            colour("ActionColor") != colour("Success"),
            "a class is not a case"
        );
    }

    /// A value its grammar cannot read keeps the single colour it had: not
    /// trying is what happened before, so nothing can look worse than it did.
    #[test]
    fn a_value_that_says_nothing_is_left_as_it_was() {
        crate::ui::highlight::register_languages();
        let source = "<div x-show=\"open\">\n";
        let theme = HighlightTheme::default_dark();
        let mut highlighter = BladeHighlighter::new();
        highlighter.refresh(source);
        let styles = highlighter.styles(&(0..source.len()), &*theme);

        let at = source.find("open").expect("the fixture holds it");
        let style = styles
            .iter()
            .find(|(r, _)| r.start <= at && at < r.end)
            .expect("every byte is covered");
        assert_eq!(
            style.1.color,
            Scope::Script.resolve(&*theme).and_then(|s| s.color),
            "a bare identifier is no reason to bleach the value"
        );
    }

    /// An `x-effect` over two lines is how anyone writes one, and the second
    /// line is not markup.
    #[test]
    fn a_value_may_span_two_lines() {
        let mut state = State::default();
        let first = "<div x-effect=\"const id = 1;";
        let found = scan(first, &mut state);
        assert_eq!(covered(first, &found), [("const id = 1;", Scope::Script)]);
        assert_eq!(
            state.value,
            Some(('"', Scope::Script)),
            "the value stays open, and stays JavaScript"
        );

        let second = "if (id) { go(); }\">";
        let found = scan(second, &mut state);
        assert_eq!(
            covered(second, &found),
            [("if (id) { go(); }", Scope::Script)]
        );
        assert_eq!(state.value, None, "and closes");
    }

    /// The contract `InputHighlighter` states and nothing checks. A run out of
    /// order, or a hole, shifts every colour after it.
    #[test]
    fn the_merged_runs_are_sorted_disjoint_and_cover_the_range() {
        crate::ui::highlight::register_languages();
        let source = "<p>{{ $x }}</p>\n@if ($ok)\n@php\n$y = 1;\n@endphp\n@endif\n";
        let theme = HighlightTheme::default_dark();
        let mut highlighter = BladeHighlighter::new();
        highlighter.refresh(source);

        // A window that starts and ends in the middle of things, as the editor
        // asks for the lines it is about to paint.
        for range in [0..source.len(), 4..30, 17..source.len() - 1] {
            let styles = highlighter.styles(&range, &*theme);
            let mut at = range.start;
            for (run, _) in &styles {
                assert_eq!(run.start, at, "a hole or an overlap in {range:?}");
                assert!(run.start < run.end);
                at = run.end;
            }
            assert_eq!(at, range.end, "the range is not covered whole");
        }
    }

    /// The document walk keeps the offsets of the real text, which is what lets
    /// the styles be read against it.
    #[test]
    fn a_document_is_scanned_and_masked_in_place() {
        let source = "<p>\n@php\n$x = 1;\n@endphp\n";
        let masked = mask_document(source).expect("a block opens");
        assert_eq!(masked, "<p>\n<?  \n$x = 1;\n?>     \n");
        assert_eq!(masked.len(), source.len(), "the offsets must not move");

        let found = scan_document(source);
        let covered: Vec<_> = found.iter().map(|(r, _)| &source[r.clone()]).collect();
        assert_eq!(covered, ["@php", "@endphp"]);

        assert_eq!(mask_document("<p>ordinary</p>\n"), None);
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
        let mut open = State::default();
        let first = "{{-- an explanation";
        assert_eq!(
            covered(first, &scan(first, &mut open)),
            [(first, Scope::Comment)]
        );
        assert!(open.comment, "the comment stays open");

        let middle = "   that carries on";
        assert_eq!(scan(middle, &mut open).len(), 1);
        assert!(open.comment);

        let last = "  --}} <p>rest</p>";
        let found = scan(last, &mut open);
        assert_eq!(&last[found[0].0.clone()], "  --}}");
        assert!(!open.comment, "and closes again");
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

    /// The shape a directive shares with an Alpine binding, and the single
    /// character that tells them apart.
    #[test]
    fn an_alpine_binding_is_an_attribute_and_not_a_directive() {
        let line = "<button @click.prevent=\"open = !open\" @if=\"x\">";
        let found = scan_line(line);
        assert_eq!(
            covered(line, &found),
            [
                ("@click.prevent", Scope::Attribute),
                ("open = !open", Scope::Script),
                // Even spelled `@if`: what follows is a value, so it is an
                // attribute — a directive is never followed by an `=`.
                ("@if", Scope::Attribute),
                ("x", Scope::Script),
            ]
        );

        let directive = "@if ($ok) @click @endif";
        assert!(
            covered(directive, &scan_line(directive))
                .iter()
                .all(|(_, scope)| *scope != Scope::Attribute),
            "with no value, an `@word` is Blade's"
        );
    }

    /// `@{{ … }}` is how a view hands the braces to Alpine or Vue: they are
    /// output as they are, so they are not an echo.
    #[test]
    fn the_escape_hands_the_braces_to_the_javascript() {
        let line = "<span>@{{ count }}</span>";
        assert!(
            scan_line(line).is_empty(),
            "neither an echo nor a directive: {:?}",
            scan_line(line)
        );
    }

    /// The masking's whole safety is here: the same number of bytes, so the
    /// styles the grammar returns still designate the real line.
    #[test]
    fn masking_a_php_block_keeps_every_offset() {
        let mut open = State::default();
        let masked = mask_php("  @php", &mut open).expect("a block opens");
        assert_eq!(masked, "  <?  ");
        let masked = mask_php("@endphp <p>", &mut open).expect("and closes");
        assert_eq!(masked, "?>      <p>");
        assert_eq!(masked.len(), "@endphp <p>".len());
    }

    /// Two `@php` that do not open a block, and masking either would put a PHP
    /// tag in the middle of a view — which the grammar would keep reading as
    /// code until the end of the file.
    #[test]
    fn what_is_not_a_php_block_is_not_masked() {
        let mut open = State::default();
        assert_eq!(mask_php("@php($total = 0)", &mut open), None);
        assert_eq!(mask_php("{{-- @php --}}", &mut open), None);
        assert_eq!(mask_php("<p>ordinary</p>", &mut open), None);
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
                Scope::PhpOpen,
                Scope::PhpClose,
                Scope::Attribute,
                Scope::Script,
            ] {
                assert!(
                    scope.resolve(&*theme).is_some(),
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
        let mut open = State::default();
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
        let mut open = State::default();
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
        let mut open = State::default();
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
