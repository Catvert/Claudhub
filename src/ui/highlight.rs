//! Syntax highlighting for diffs.
//!
//! A diff's content is code, and it is the code that gets reviewed — not the
//! `+`/`-` markers. Claudhub therefore colours the lines with the *file's*
//! grammar, not with the `diff` grammar.
//!
//! The problem that raises: a hunk is not a file. It starts in the middle of a
//! function, skips dozens of lines, and mixes two versions of the text. The
//! answer is to rebuild both versions — the old one (context + removed lines)
//! and the new one (context + added lines) — colour each **once only**, then
//! redistribute the styles line by line. The parse stays imperfect at hunk
//! boundaries, where the parser is missing what was elided; in practice it
//! recovers, because tree-sitter grammars recover on error.
//!
//! The cost is paid once per opened file, when the diff arrives, never during a
//! render: `SyntaxHighlighter::new` compiles the grammar's queries, which has no
//! business happening inside a frame.

use std::collections::HashMap;
use std::ops::Range;
use std::path::Path;

use gpui::HighlightStyle;
use gpui_component::highlighter::{
    HighlightTheme, LanguageConfig, LanguageRegistry, SyntaxHighlighter,
};
use gpui_component::input::Rope;

use super::blade;
use crate::git::search::Results;
use crate::git::{DiffLineKind, FileDiff};

/// A compiled grammar, kept from one call to the next.
///
/// `SyntaxHighlighter::new` compiles the grammar's queries — nearly forty
/// milliseconds for JavaScript — where `update` only reparses a text. A file
/// opened, a plugin's excerpt, a diff: each one used to pay that fixed cost
/// again. The pool is per thread and holds one instance per language; a
/// language name is a bounded vocabulary here, but a plugin names its own, so
/// the lot is dropped past a ceiling rather than grown for ever.
///
/// The instance is **taken out** while it is in use: a nested call would
/// otherwise meet a borrow of the map and panic.
fn with_grammar<T>(language: &str, f: impl FnOnce(&mut SyntaxHighlighter) -> T) -> T {
    thread_local! {
        static GRAMMARS: std::cell::RefCell<HashMap<String, SyntaxHighlighter>> =
            std::cell::RefCell::new(HashMap::new());
    }
    const MAX_GRAMMARS: usize = 32;
    let mut highlighter = GRAMMARS
        .with(|grammars| grammars.borrow_mut().remove(language))
        .unwrap_or_else(|| SyntaxHighlighter::new(language));
    let out = f(&mut highlighter);
    GRAMMARS.with(|grammars| {
        let mut grammars = grammars.borrow_mut();
        if grammars.len() >= MAX_GRAMMARS {
            grammars.clear();
        }
        grammars.insert(language.to_string(), highlighter);
    });
    out
}

/// Registers the grammars gpui-component does not embed.
///
/// PHP is missing from it, although it is the language of half the repositories
/// Claudhub serves to review; Nix is missing too, and it is what this repository
/// builds itself with; Dockerfile and `just` likewise, and they are read in
/// nearly every repository served. The four grammars are therefore linked
/// directly and declared in the shared registry, from which the rest of the
/// library will find them under the names `php`, `nix`, `dockerfile` and `just`
/// like any other.
///
/// To be called once at startup, before any render: the registry is a locked
/// singleton, and registering under a keystroke would amount to doing it while a
/// highlighter reads it.
pub fn register_languages() {
    // The injections describe the HTML surrounding the PHP and the SQL of query
    // strings: without them, a Blade file or a view would only have colours in
    // its `<?php` tags.
    let injections = format!("{}\n{HTML_INJECTION}", tree_sitter_php::INJECTIONS_QUERY);
    let highlights = format!("{}\n{PHP_CONSTANTS}", tree_sitter_php::HIGHLIGHTS_QUERY);
    let php = LanguageConfig::new(
        "php",
        tree_sitter_php::LANGUAGE_PHP.into(),
        vec!["html".into(), "sql".into()],
        &highlights,
        &injections,
        "",
    );
    LanguageRegistry::singleton().register("php", &php);

    // Nix injects bash into the phases and hooks of a derivation — `shellHook`,
    // `buildPhase`, `writeShellScript` — which is where the interesting part of
    // a `shell.nix` lives; the grammar embedded by gpui-component answers to
    // that name. The locals query is not registered: the crate ships
    // `queries/locals.scm` but exposes no constant for it, and the
    // `#is-not? local` clauses of the highlights simply keep applying.
    let nix = LanguageConfig::new(
        "nix",
        tree_sitter_nix::LANGUAGE.into(),
        vec!["bash".into()],
        tree_sitter_nix::HIGHLIGHTS_QUERY,
        tree_sitter_nix::INJECTIONS_QUERY,
        "",
    );
    LanguageRegistry::singleton().register("nix", &nix);

    // A Dockerfile is mostly shell: the body of every `RUN`, and the heredocs
    // of a `COPY`, are injected — which is where the grammar earns its place,
    // an instruction line being four coloured words. The languages the registry
    // does not hold (`comment`, `xml`) are skipped rather than an error, so
    // only those it can serve are declared.
    let dockerfile = LanguageConfig::new(
        "dockerfile",
        tree_sitter_containerfile::LANGUAGE.into(),
        vec!["bash".into(), "json".into(), "yaml".into(), "toml".into()],
        tree_sitter_containerfile::HIGHLIGHTS_QUERY,
        tree_sitter_containerfile::INJECTIONS_QUERY,
        "",
    );
    LanguageRegistry::singleton().register("dockerfile", &dockerfile);

    // A recipe body is a shell script, and that injection is most of what one
    // reads in a justfile — the rest is a dozen keywords and the `{{…}}` of an
    // interpolation. The crate is `codebook`'s rather than the official one;
    // the reason is in `Cargo.toml`, and it is a resolution error, not a taste.
    let just = LanguageConfig::new(
        "just",
        codebook_tree_sitter_just::LANGUAGE.into(),
        vec!["bash".into()],
        codebook_tree_sitter_just::HIGHLIGHTS_QUERY,
        codebook_tree_sitter_just::INJECTIONS_QUERY,
        "",
    );
    LanguageRegistry::singleton().register("just", &just);
}

/// A class constant, and with it every enum case.
///
/// The grammar's own query names a constant only when it is written in capitals
/// (`Foo::BAR`), which was the whole convention when it was written. An enum
/// case is not: `ActionColor::Success` matched nothing at all — neither half —
/// and came out grey in the middle of coloured code, in a codebase where enums
/// are everywhere. The node carries no field names, hence the anchor: the first
/// name is the class, the second is what is read from it.
const PHP_CONSTANTS: &str = r#"
(class_constant_access_expression
  (name) @type
  .
  (name) @constant)
"#;

/// The HTML injection, written here because the crate does not ship it.
///
/// `tree_sitter_php::INJECTIONS_QUERY` is `queries/injections.scm`, and it only
/// covers phpdoc and heredocs. The HTML that **surrounds** the code — so the
/// whole of a Blade view, and everything outside `<?php` in an ordinary file —
/// lives in a second file, `queries/injections-text.scm`, which the Rust
/// bindings expose under no constant. It therefore had to be copied: without it,
/// a view arrived entirely grey, tags included, and nothing said so — an
/// injection that does not find its grammar raises no error, only bare text.
///
/// `injection.combined` gathers every text fragment into a single tree, which is
/// the only correct reading: a tag opened before a `<?php` closes after it, and
/// treating them separately would give two malformed documents.
const HTML_INJECTION: &str = r#"
((text) @injection.content
 (#set! injection.language "html")
 (#set! injection.combined))
"#;

/// A line's styles, as byte offsets relative to its text.
pub type LineStyles = Vec<(Range<usize>, HighlightStyle)>;

/// A whole diff's styles, indexed `[hunk][line]`.
#[derive(Default)]
pub struct DiffHighlights {
    hunks: Vec<Vec<LineStyles>>,
}

impl DiffHighlights {
    /// A line's styles, or an empty slice if the file has no known grammar — in
    /// which case the view shows bare text.
    pub fn line(&self, hunk: usize, line: usize) -> &[(Range<usize>, HighlightStyle)] {
        self.hunks
            .get(hunk)
            .and_then(|h| h.get(line))
            .map(Vec::as_slice)
            .unwrap_or(&[])
    }

    #[cfg(test)]
    pub fn is_empty(&self) -> bool {
        self.hunks.is_empty()
    }

    /// Colours a diff. Returns an empty set if the extension maps to no
    /// grammar, which is the most frequent case (data files, texts, binaries).
    pub fn compute(path: &Path, diff: &FileDiff, theme: &HighlightTheme) -> Self {
        let Some(language) = language_for_path(path) else {
            return Self::default();
        };
        if diff.hunks.is_empty() {
            return Self::default();
        }

        // Two passes: the old version then the new one. Context lines belong to
        // both, and receive the second's styles — they are identical on both
        // sides, so the choice has no consequence, but one has to be made.
        let mut styles: Vec<Vec<LineStyles>> = diff
            .hunks
            .iter()
            .map(|hunk| vec![LineStyles::new(); hunk.lines.len()])
            .collect();

        // A Blade view is HTML before it is PHP: prefixing an opening tag would
        // make its tags read as code. Its own constructs — directives, echoes,
        // comments — are added afterwards, the PHP grammar knowing none of them.
        let blade = blade::is_blade(path);

        // A single instance for both passes, and the same one from a file to
        // the next: compiling a grammar is the fixed cost, reparsing a text is
        // not. See `with_grammar`.
        with_grammar(language, |highlighter| {
            for side in [Side::Old, Side::New] {
                let (mut text, mut spans) = build_side(diff, side, blade);
                if spans.is_empty() {
                    continue;
                }
                // The fragment first receives what its grammar needs to
                // recognise it. The line positions follow the offset: the
                // prologue belongs to none of them, so its styles are ignored
                // by themselves.
                let prologue = if blade { "" } else { prologue(language, &text) };
                if !prologue.is_empty() {
                    text.insert_str(0, prologue);
                    for span in &mut spans {
                        span.range.start += prologue.len();
                        span.range.end += prologue.len();
                    }
                }
                highlighter.update(None, &Rope::from_str(&text), None);
                let highlighted = highlighter.styles(&(0..text.len()), theme);
                distribute(&highlighted, &spans, &mut styles);
            }
        });
        if blade {
            blade::overlay(diff, theme, &mut styles);
        }

        Self { hunks: styles }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Side {
    Old,
    New,
}

impl Side {
    fn keeps(self, kind: DiffLineKind) -> bool {
        match kind {
            DiffLineKind::Context => true,
            DiffLineKind::Added => self == Side::New,
            DiffLineKind::Removed => self == Side::Old,
            DiffLineKind::NoNewline => false,
        }
    }
}

/// What has to be written in front of a fragment for its grammar to recognise
/// it.
///
/// PHP is the case that forces it: without `<?php`, its grammar reads the
/// **whole** fragment as HTML text, and not one colour comes out. And a hunk
/// almost always starts in the middle of the file, so after the opening tag —
/// that is the common case, not the exception, which explains why the
/// highlighting seemed broken "very often".
///
/// The prologue is only added if it is missing: a view whose hunk contains the
/// start of the file does begin with `<?php` or with HTML, and prefixing a
/// second one would break the parse. Blade views never receive one — see
/// `blade`.
fn prologue(language: &str, fragment: &str) -> &'static str {
    match language {
        "php" if !opens_php(fragment) => "<?php\n",
        _ => "",
    }
}

/// True if the fragment already carries a PHP opening tag, or HTML expecting one
/// further down.
///
/// The HTML counts: a view starts with `<div>` and switches to PHP afterwards,
/// and the full grammar is made for that mixture.
fn opens_php(fragment: &str) -> bool {
    fragment
        .lines()
        .take(PROLOGUE_LOOKAHEAD)
        .any(|line| line.contains("<?"))
        || fragment.trim_start().starts_with('<')
}

/// How many lines are examined before concluding the tag is missing.
///
/// The whole fragment would be pointless: a tag that only appears on the
/// fiftieth line leaves the preceding ones colourless anyway, and that is
/// precisely what we are fixing.
const PROLOGUE_LOOKAHEAD: usize = 3;

/// Where a diff line sits in the rebuilt text.
struct Span {
    hunk: usize,
    line: usize,
    range: Range<usize>,
}

/// Rebuilds one version of the file and records each line's position.
///
/// `blade` masks the markers of the `@php` blocks so the grammar reads their
/// body as PHP — see `blade::mask_php`. The mask keeps every line's byte length,
/// so the spans are those of the real text and nothing has to be shifted back.
fn build_side(diff: &FileDiff, side: Side, blade: bool) -> (String, Vec<Span>) {
    let mut text = String::new();
    let mut spans = Vec::new();
    for (h, hunk) in diff.hunks.iter().enumerate() {
        // Comment state starts again from scratch at each hunk, as it does in
        // the overlay: what separates two hunks has been elided.
        let mut state = blade::State::default();
        for (l, line) in hunk.lines.iter().enumerate() {
            if !side.keeps(line.kind) {
                continue;
            }
            let masked = blade
                .then(|| blade::mask_php(&line.text, &mut state))
                .flatten();
            let start = text.len();
            text.push_str(masked.as_deref().unwrap_or(&line.text));
            spans.push(Span {
                hunk: h,
                line: l,
                range: start..text.len(),
            });
            text.push('\n');
        }
    }
    (text, spans)
}

/// Redistributes the rebuilt text's styles onto the diff's lines.
///
/// Both lists are sorted by increasing offset, which allows a single joint walk:
/// a style spilling from one line onto the next is cut at the boundary rather
/// than thrown away — that is the case of a multi-line string, each piece of
/// which has to stay coloured.
fn distribute(
    highlighted: &[(Range<usize>, HighlightStyle)],
    spans: &[Span],
    out: &mut [Vec<LineStyles>],
) {
    let mut next = 0usize;
    for span in spans {
        let Some(target) = out.get_mut(span.hunk).and_then(|h| h.get_mut(span.line)) else {
            continue;
        };
        // A context line belongs to both versions and is therefore visited
        // twice: the second pass replaces the first instead of adding to it.
        // Accumulating would produce duplicated, unsorted ranges, which
        // rendering turns into a silent shift of the whole highlighting from the
        // duplicate on.
        target.clear();
        // Advance to the first style touching this line. Both lists being
        // sorted, this cursor never goes back: the walk is linear and not
        // quadratic, which matters on a diff of several thousand lines.
        while next < highlighted.len() && highlighted[next].0.end <= span.range.start {
            next += 1;
        }
        for (range, style) in &highlighted[next..] {
            if range.start >= span.range.end {
                break;
            }
            if range.end <= span.range.start || *style == HighlightStyle::default() {
                continue;
            }
            let start = range.start.max(span.range.start) - span.range.start;
            let end = range.end.min(span.range.end) - span.range.start;
            if start < end {
                target.push((start..end, *style));
            }
        }
    }
}

/// A whole file's styles, line by line.
///
/// The diff's highlighting rebuilds two versions out of hunks; a preview has
/// the file itself, so there is nothing to rebuild and no prologue to write —
/// a file starts where the grammar expects it to. What is left is the cut into
/// lines, since the reader is a virtualised list and asks for one row at a
/// time.
///
/// **Computed once, when the content arrives, and never in a render closure**:
/// parsing a file costs milliseconds, and the closure runs for every visible
/// line of every frame. The rule is the diff's, and the reason is the same.
#[derive(Default)]
pub struct DocumentHighlights {
    lines: Vec<LineStyles>,
}

impl DocumentHighlights {
    /// Offsets are **relative to the line** and in bytes, which is what gpui
    /// wants to style a fragment: indexing by characters breaks at the first
    /// accent.
    pub fn line(&self, index: usize) -> &[(Range<usize>, HighlightStyle)] {
        self.lines.get(index).map(Vec::as_slice).unwrap_or_default()
    }

    /// Past this, a file is left plain. A preview is read, not studied, and a
    /// generated file of two megabytes would cost a parse for a page of it.
    pub const MAX_BYTES: usize = 512 * 1024;

    /// The same, for a fragment that has no file around it.
    ///
    /// A plugin's excerpt is named by its language and not by a path — it was
    /// fetched, not read — and it is a **fragment**: what precedes it is not
    /// there, exactly as for a search hit, and a grammar's error recovery is
    /// what makes parsing it on its own worth doing.
    pub fn for_language(language: &str, text: &str, theme: &HighlightTheme) -> Self {
        if text.len() > Self::MAX_BYTES {
            return Self::default();
        }
        // The prologue for the same reason as everywhere else: without `<?php`
        // a PHP fragment is read as HTML text and comes back with no colours at
        // all — and a stack frame is very often PHP.
        let head = prologue(language, text);
        let whole = format!("{head}{text}");
        let styles = with_grammar(language, |highlighter| {
            highlighter.update(None, &Rope::from_str(&whole), None);
            highlighter.styles(&(0..whole.len()), theme)
        });
        let mut lines = cut_into_lines(&whole, &styles);
        // The prologue is one line, and it is not one of the excerpt's: its
        // own lines do not count in the offsets the panel paints against.
        let dropped = head.lines().count();
        lines.drain(..dropped.min(lines.len()));
        Self { lines }
    }

    pub fn compute(path: &Path, text: &str, theme: &HighlightTheme) -> Self {
        if text.len() > Self::MAX_BYTES {
            return Self::default();
        }
        // A Blade view is HTML and directives before it is PHP: its own
        // scanner has to pass over the grammar, exactly as in a diff.
        let styles = if blade::is_blade(path) {
            blade::document_styles(text, theme)
        } else {
            let Some(language) = language_for_path(path) else {
                return Self::default();
            };
            with_grammar(language, |highlighter| {
                highlighter.update(None, &Rope::from_str(text), None);
                highlighter.styles(&(0..text.len()), theme)
            })
        };
        Self {
            lines: cut_into_lines(text, &styles),
        }
    }
}

/// The colouring of a search's result list, indexed `[file][hit]`.
///
/// A hit is **one line torn out of a file**, which is a harder fragment than a
/// hunk: what precedes it is not there, and the next hit may be four hundred
/// lines further down. They are therefore *not* joined into a pseudo-document
/// the way a hunk's two sides are — one unbalanced quote would then colour every
/// hit under it as a string, for a whole file at a time. Each line is parsed
/// **on its own**, which is what a grammar's error recovery is for, and the
/// price is bounded by `git::search::MAX_HITS`.
///
/// **One grammar per language, reused across files**: `SyntaxHighlighter::new`
/// compiles the queries — tens of milliseconds for JavaScript — and a search
/// touching a hundred files would otherwise pay it a hundred times.
///
/// Computed on arrival and never in a render, the rule of the whole module: the
/// list is virtualised, and its closure runs for every visible row of every
/// frame.
#[derive(Default)]
pub struct HitHighlights {
    files: Vec<Vec<LineStyles>>,
}

impl HitHighlights {
    /// A hit's styles, as byte offsets relative to the **trimmed** text — which
    /// is what the row shows, its leading indentation dropped so that a hit six
    /// levels deep is not a column of nothing.
    pub fn line(&self, file: usize, hit: usize) -> &[(Range<usize>, HighlightStyle)] {
        self.files
            .get(file)
            .and_then(|file| file.get(hit))
            .map(Vec::as_slice)
            .unwrap_or(&[])
    }

    pub fn compute(results: &Results, theme: &HighlightTheme) -> Self {
        let mut grammars: HashMap<&'static str, SyntaxHighlighter> = HashMap::new();
        // Blade's own highlighter is kept for exactly the same reason as the
        // map above — it carries a PHP grammar, and building one costs tens of
        // milliseconds. A list of two thousand view lines paid it two thousand
        // times, which is a minute of frozen window.
        let mut blade_grammar: Option<blade::BladeHighlighter> = None;
        let files = results
            .files
            .iter()
            .map(|file| {
                // A Blade view is HTML and directives before it is PHP, here as
                // everywhere else.
                let blade = blade::is_blade(&file.path);
                let language = language_for_path(&file.path);
                if !blade && language.is_none() {
                    return vec![LineStyles::new(); file.hits.len()];
                }
                file.hits
                    .iter()
                    .map(|hit| {
                        let text = hit.text.trim_start();
                        if text.is_empty() {
                            return LineStyles::new();
                        }
                        if blade {
                            let styles = blade_grammar
                                .get_or_insert_with(blade::BladeHighlighter::new)
                                .document_styles(text, theme);
                            return nth_line(text, &styles, 0);
                        }
                        let Some(language) = language else {
                            return LineStyles::new();
                        };
                        let highlighter = grammars
                            .entry(language)
                            .or_insert_with(|| SyntaxHighlighter::new(language));
                        // The line first receives what its grammar needs to
                        // recognise it: without `<?php`, PHP reads the whole
                        // fragment as HTML text and not one colour comes out.
                        let prologue = prologue(language, text);
                        let full = format!("{prologue}{text}");
                        highlighter.update(None, &Rope::from_str(&full), None);
                        let styles = highlighter.styles(&(0..full.len()), theme);
                        nth_line(&full, &styles, prologue.matches('\n').count())
                    })
                    .collect()
            })
            .collect();
        Self { files }
    }
}

/// One line of a coloured fragment, the prologue's own lines skipped.
fn nth_line(text: &str, styles: &[(Range<usize>, HighlightStyle)], skip: usize) -> LineStyles {
    cut_into_lines(text, styles)
        .into_iter()
        .nth(skip)
        .unwrap_or_default()
}

/// Redistributes a document's styles onto its lines.
///
/// Both lists are walked once, jointly: the styles are sorted by offset, so the
/// cursor never goes back. A style spilling over a line break — a multi-line
/// string, a block comment — is **cut at the boundary** rather than dropped,
/// each piece having to stay coloured.
fn cut_into_lines(text: &str, styles: &[(Range<usize>, HighlightStyle)]) -> Vec<LineStyles> {
    let mut out: Vec<LineStyles> = Vec::new();
    let mut next = 0usize;
    let mut at = 0usize;
    for line in text.split('\n') {
        let span = at..at + line.len();
        // The newline that separates them belongs to neither.
        at = span.end + 1;
        let mut target: LineStyles = Vec::new();
        while next < styles.len() && styles[next].0.end <= span.start {
            next += 1;
        }
        for (range, style) in &styles[next..] {
            if range.start >= span.end {
                break;
            }
            if range.end <= span.start || *style == HighlightStyle::default() {
                continue;
            }
            let start = range.start.max(span.start) - span.start;
            let end = range.end.min(span.end) - span.start;
            if start < end {
                target.push((start..end, *style));
            }
        }
        out.push(target);
    }
    out
}

/// The grammar associated with an extension.
///
/// The list only covers what `gpui-component` embeds with the
/// `tree-sitter-languages` feature, plus the four grammars `register_languages`
/// declares itself: a missing extension returns `None`, and the
/// view shows bare text rather than wrong highlighting. Some embedded languages
/// (`swift`, `csharp`, `proto`, `cmake`, `graphql`) have an empty highlight
/// query upstream; listing them would bring nothing, so they are omitted.
pub fn language_for_path(path: &Path) -> Option<&'static str> {
    // A whole family of files carries its kind in its **name** and has no
    // extension at all, or an extension that says something else: `Dockerfile`,
    // `Dockerfile.dev`, `justfile`, `.env.local`. Matched in lowercase, since
    // each of them is written both ways depending on the repository.
    if let Some(name) = path.file_name().and_then(|n| n.to_str()) {
        let name = name.to_ascii_lowercase();
        // A leading dot belongs to the convention and not to the name: `.env`
        // and `.justfile` are the same thing as `env` and `justfile`. What
        // follows therefore reads "the name *is* that word, or carries it as
        // its first or last dotted part" — `Dockerfile.dev` and `dev.dockerfile`
        // are both Dockerfiles, `.env.local` and `prod.env` are both `.env`.
        let bare = name.strip_prefix('.').unwrap_or(&name);
        let named = |stem: &str| {
            bare == stem
                || bare
                    .strip_prefix(stem)
                    .is_some_and(|rest| rest.starts_with('.'))
                || bare
                    .strip_suffix(stem)
                    .is_some_and(|rest| rest.ends_with('.'))
        };
        if matches!(bare, "makefile" | "gnumakefile") {
            return Some("make");
        }
        if named("dockerfile") || named("containerfile") {
            return Some("dockerfile");
        }
        if named("justfile") || named("just") {
            return Some("just");
        }
        // A `.env` is `KEY=value` and `#` comments, which is a shell's own
        // syntax — the file exists to be sourced by one. Colouring it as bash
        // is therefore a convention rather than a lie, like `.rn` below.
        if named("env") {
            return Some("bash");
        }
    }
    let extension = path.extension()?.to_str()?.to_ascii_lowercase();
    Some(match extension.as_str() {
        "rs" => "rust",
        // A plugin's script. There is **no Rune grammar** in the tree, and its
        // syntax is Rust's on purpose — `fn`, `let`, `match`, `?`, `.await`,
        // the same string literals. Colouring it as Rust is therefore a
        // convention and not a lie one has to unlearn: what it gets wrong is
        // the handful of places the two languages differ, against a whole file
        // with no colours at all.
        "rn" => "rust",
        "c" | "h" => "c",
        "cc" | "cpp" | "cxx" | "hpp" | "hh" => "cpp",
        "js" | "mjs" | "cjs" => "javascript",
        "jsx" | "tsx" => "tsx",
        "ts" | "mts" | "cts" => "typescript",
        "py" | "pyi" => "python",
        "rb" | "rake" | "gemspec" => "ruby",
        "go" => "go",
        "java" => "java",
        "scala" | "sc" => "scala",
        "ex" | "exs" => "elixir",
        "zig" => "zig",
        "sh" | "bash" | "zsh" => "bash",
        // Blade views are PHP interspersed with HTML: the PHP grammar covers
        // them through its injections, and its unrecognised `@if` directive
        // costs less than a whole file with no colours.
        "php" | "phtml" | "blade" => "php",
        "css" | "scss" => "css",
        "html" | "htm" => "html",
        "json" => "json",
        "toml" => "toml",
        "nix" => "nix",
        "yaml" | "yml" => "yaml",
        "sql" => "sql",
        "md" | "markdown" => "markdown",
        "diff" | "patch" => "diff",
        _ => return None,
    })
}

/// Lays backgrounds over an existing highlighting.
///
/// It is what makes a search hit visible **inside** coloured code: the
/// background marks the find, the grammar keeps its text colours. Repainting the
/// whole line would lose one or the other.
///
/// `with_highlights`'s two invariants hold here too, and gpui checks neither:
/// the ranges returned are **sorted and disjoint** — the function converts them
/// into consecutive run lengths, and an out-of-order range shifts everything
/// after it — and the offsets are in **bytes**. `base` and `marks` have to be so
/// as well, each on its own; they may on the other hand overlap each other,
/// which is in fact the common case.
pub fn overlay(
    base: &[(Range<usize>, HighlightStyle)],
    marks: &[(Range<usize>, gpui::Hsla)],
) -> Vec<(Range<usize>, HighlightStyle)> {
    if marks.is_empty() {
        return base.to_vec();
    }
    // Every boundary of both partitions: between two of them, neither the
    // background style nor the text style changes, so the segment is uniform by
    // construction.
    let mut cuts: Vec<usize> = Vec::with_capacity((base.len() + marks.len()) * 2);
    for (range, _) in base {
        cuts.push(range.start);
        cuts.push(range.end);
    }
    for (range, _) in marks {
        cuts.push(range.start);
        cuts.push(range.end);
    }
    cuts.sort_unstable();
    cuts.dedup();

    let mut out: Vec<(Range<usize>, HighlightStyle)> = Vec::new();
    for pair in cuts.windows(2) {
        let (start, end) = (pair[0], pair[1]);
        let style = base
            .iter()
            .find(|(range, _)| range.start <= start && end <= range.end)
            .map(|(_, style)| *style);
        let mark = marks
            .iter()
            .find(|(range, _)| range.start <= start && end <= range.end)
            .map(|(_, color)| *color);
        let (Some(mut style), mark) = (style.or(mark.map(|_| HighlightStyle::default())), mark)
        else {
            // Neither highlighting nor hit: the text stays at the ambient style,
            // and a range with no effect has no business in the list.
            continue;
        };
        if let Some(color) = mark {
            style.background_color = Some(color);
        }
        // Two neighbouring segments of the same style are glued back together:
        // `with_highlights` turns them into runs, and two identical runs side by
        // side are a layout cost for nothing.
        match out.last_mut() {
            Some((last, previous)) if last.end == start && *previous == style => last.end = end,
            _ => out.push((start..end, style)),
        }
    }
    out
}

/// Underlines one range on top of an existing highlighting.
///
/// The `Ctrl`-hovered symbol of a diff line: the grammar keeps its colours, the
/// word gains the underline that says it can be followed. The colour is left
/// unset on purpose — an underline takes the colour of the text it is under,
/// which is what makes it read as part of the word rather than a rule drawn
/// near it.
///
/// The same two invariants as `overlay`, and the same reason: sorted, disjoint,
/// and in bytes. Unlike `overlay`, a segment the base says nothing about is
/// **kept** when it falls in the range — here the added style is the whole
/// point, and a line with no grammar at all must underline just the same.
pub fn underline(
    base: &[(Range<usize>, HighlightStyle)],
    word: Range<usize>,
) -> Vec<(Range<usize>, HighlightStyle)> {
    let mut cuts: Vec<usize> = Vec::with_capacity(base.len() * 2 + 2);
    for (range, _) in base {
        cuts.push(range.start);
        cuts.push(range.end);
    }
    cuts.push(word.start);
    cuts.push(word.end);
    cuts.sort_unstable();
    cuts.dedup();

    let mut out: Vec<(Range<usize>, HighlightStyle)> = Vec::new();
    for pair in cuts.windows(2) {
        let (start, end) = (pair[0], pair[1]);
        let inside = word.start <= start && end <= word.end;
        let style = base
            .iter()
            .find(|(range, _)| range.start <= start && end <= range.end)
            .map(|(_, style)| *style);
        // Outside the word and unstyled: nothing to say about it, and a run
        // with no effect has no business in the list.
        let Some(mut style) = style.or_else(|| inside.then(HighlightStyle::default)) else {
            continue;
        };
        if inside {
            style.underline = Some(gpui::UnderlineStyle {
                thickness: gpui::px(1.),
                color: None,
                wavy: false,
            });
        }
        match out.last_mut() {
            Some((last, previous)) if last.end == start && *previous == style => last.end = end,
            _ => out.push((start..end, style)),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::underline;
    use gpui::HighlightStyle;

    /// The hovered word keeps the colour the grammar gave it, and the runs
    /// around it are left exactly as they were.
    #[test]
    fn an_underline_splits_a_run_without_recolouring_it() {
        let red = HighlightStyle {
            color: Some(gpui::red()),
            ..Default::default()
        };
        // `fooba` is red, and `oob` is hovered.
        let runs = underline(&[(0..5, red)], 1..4);
        assert_eq!(runs.len(), 3);
        assert_eq!(runs[0].0, 0..1);
        assert_eq!(runs[1].0, 1..4);
        assert_eq!(runs[2].0, 4..5);
        assert!(runs.iter().all(|(_, style)| style.color == red.color));
        assert!(runs[0].1.underline.is_none());
        assert!(runs[1].1.underline.is_some());
        assert!(runs[2].1.underline.is_none());
    }

    /// A line with no grammar at all still underlines: there is no base run to
    /// carry the style, so the range has to make its own.
    #[test]
    fn a_line_without_highlighting_underlines_all_the_same() {
        let runs = underline(&[], 3..7);
        assert_eq!(runs.len(), 1);
        assert_eq!(runs[0].0, 3..7);
        assert!(runs[0].1.underline.is_some());
    }

    /// A background laid in the middle of a coloured word must split that word
    /// into three, and not replace its colour.
    #[test]
    fn a_mark_splits_the_style_it_falls_inside() {
        let red = HighlightStyle {
            color: Some(gpui::red()),
            ..Default::default()
        };
        let base = vec![(0..10, red)];
        let yellow = gpui::yellow();
        let out = overlay(&base, &[(3..6, yellow)]);
        assert_eq!(out.len(), 3);
        assert_eq!(out[0].0, 0..3);
        assert_eq!(out[1].0, 3..6);
        assert_eq!(out[2].0, 6..10);
        assert_eq!(out[0].1.color, Some(gpui::red()));
        assert_eq!(
            out[1].1.color,
            Some(gpui::red()),
            "le texte garde sa couleur"
        );
        assert_eq!(out[1].1.background_color, Some(yellow));
        assert!(out[0].1.background_color.is_none());
    }

    /// With no highlighting underneath, the hit is the line's only style.
    #[test]
    fn a_mark_on_bare_text_stands_alone() {
        let yellow = gpui::yellow();
        let out = overlay(&[], &[(2..4, yellow)]);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].0, 2..4);
        assert_eq!(out[0].1.background_color, Some(yellow));
    }

    /// The invariant gpui does not check: sorted and disjoint.
    #[test]
    fn the_result_stays_sorted_and_disjoint() {
        let style = |c| HighlightStyle {
            color: Some(c),
            ..Default::default()
        };
        let base = vec![(0..4, style(gpui::red())), (8..12, style(gpui::blue()))];
        let out = overlay(&base, &[(2..10, gpui::yellow())]);
        let mut previous = 0;
        for (range, _) in &out {
            assert!(range.start >= previous, "out-of-order ranges: {out:?}");
            assert!(range.start < range.end, "empty range: {out:?}");
            previous = range.end;
        }
        // The gap between the two coloured ranges is covered by the hit, and is
        // therefore not lost.
        assert!(out.iter().any(|(range, _)| range == &(4..8)));
    }

    /// With no hit, nothing changes: that is the case for almost every line of
    /// almost every frame.
    #[test]
    fn no_mark_returns_the_colouring_untouched() {
        let base = vec![(
            0..3,
            HighlightStyle {
                color: Some(gpui::red()),
                ..Default::default()
            },
        )];
        assert_eq!(overlay(&base, &[]), base);
    }

    use super::*;
    use crate::git::{DiffLine, Hunk};
    use std::path::PathBuf;

    fn line(kind: DiffLineKind, text: &str) -> DiffLine {
        DiffLine {
            kind,
            old_no: None,
            new_no: None,
            text: text.to_string(),
        }
    }

    fn diff(lines: Vec<DiffLine>) -> FileDiff {
        FileDiff {
            hunks: vec![Hunk {
                header: "@@ -1,3 +1,3 @@".into(),
                old_start: 1,
                new_start: 1,
                lines,
            }],
            binary: false,
            empty: false,
        }
    }

    /// A hit is a line on its own, and that is the whole risk: what colours a
    /// line has to survive having no file around it, and its offsets have to
    /// describe the **trimmed** text the row shows — off by the indentation, a
    /// style paints the wrong word.
    #[test]
    fn a_torn_out_line_is_coloured_on_its_own() {
        register_languages();
        let results = crate::git::search::Results {
            files: vec![
                crate::git::search::FileHits {
                    path: PathBuf::from("src/x.rs"),
                    hits: vec![crate::git::search::Hit {
                        line: 12,
                        text: "        let value = 1;".into(),
                    }],
                    capped: false,
                },
                // PHP without its opening tag: the prologue is what makes this
                // one anything but plain text.
                crate::git::search::FileHits {
                    path: PathBuf::from("app/User.php"),
                    hits: vec![crate::git::search::Hit {
                        line: 3,
                        text: "    return $this->name;".into(),
                    }],
                    capped: false,
                },
                // No grammar: bare text rather than a wrong colouring.
                crate::git::search::FileHits {
                    path: PathBuf::from("data.bin"),
                    hits: vec![crate::git::search::Hit {
                        line: 1,
                        text: "let value = 1;".into(),
                    }],
                    capped: false,
                },
            ],
            total: 3,
            truncated: false,
        };
        let hits = HitHighlights::compute(&results, &HighlightTheme::default_dark());

        let rust = hits.line(0, 0);
        assert!(!rust.is_empty(), "a Rust line gets styles");
        let trimmed = "let value = 1;";
        assert!(
            rust.iter().all(|(range, _)| range.end <= trimmed.len()),
            "offsets describe the trimmed text: {rust:?}"
        );
        // `let` is a keyword, and it is the first thing on the line.
        assert_eq!(rust[0].0, 0..3);

        assert!(!hits.line(1, 0).is_empty(), "PHP gets its prologue");
        assert!(
            hits.line(1, 0)
                .iter()
                .all(|(range, _)| range.end <= "return $this->name;".len()),
            "the prologue is not counted in the offsets"
        );

        assert!(hits.line(2, 0).is_empty(), "no grammar, no styles");
        // Out of bounds is an empty slice and not a panic: the list is rebuilt
        // from results that may already have been replaced.
        assert!(hits.line(9, 0).is_empty());
    }

    #[test]
    fn recognizes_languages_by_extension_and_by_name() {
        assert_eq!(language_for_path(Path::new("src/main.rs")), Some("rust"));
        assert_eq!(language_for_path(Path::new("app/index.tsx")), Some("tsx"));
        assert_eq!(language_for_path(Path::new("Makefile")), Some("make"));
        assert_eq!(language_for_path(Path::new("Cargo.toml")), Some("toml"));
        assert_eq!(language_for_path(Path::new("shell.nix")), Some("nix"));
        // Case-insensitive on the extension.
        assert_eq!(language_for_path(Path::new("SCRIPT.SH")), Some("bash"));
        // Unknown: no highlighting rather than a wrong one.
        assert_eq!(language_for_path(Path::new("data.bin")), None);
        assert_eq!(language_for_path(Path::new("LICENSE")), None);
    }

    /// The three families that carry their kind in their name, each written the
    /// several ways repositories actually write it.
    #[test]
    fn recognizes_the_files_named_after_their_kind() {
        for name in [
            "Dockerfile",
            "dockerfile",
            "Dockerfile.dev",
            "dev.Dockerfile",
            "Containerfile",
            "docker/Dockerfile.prod",
        ] {
            assert_eq!(
                language_for_path(Path::new(name)),
                Some("dockerfile"),
                "{name}"
            );
        }
        for name in ["justfile", "Justfile", ".justfile", "tools/deploy.just"] {
            assert_eq!(language_for_path(Path::new(name)), Some("just"), "{name}");
        }
        // A `.env` is read as the shell script it is meant to be sourced by.
        for name in [".env", ".env.local", ".env.example", "prod.env"] {
            assert_eq!(language_for_path(Path::new(name)), Some("bash"), "{name}");
        }
        // Neighbours that must not be swept in with them.
        assert_eq!(
            language_for_path(Path::new("environment.ts")),
            Some("typescript")
        );
        assert_eq!(language_for_path(Path::new("justify.py")), Some("python"));
    }

    /// The grammars `register_languages` links itself: what this proves is that
    /// their queries **compile** against the grammar they are given — a query
    /// naming a node the grammar does not have is rejected as a whole, and the
    /// file then shows no colours at all without a word being said.
    #[test]
    fn the_grammars_we_register_ourselves_colour_their_files() {
        register_languages();
        for (path, text) in [
            ("Dockerfile", "FROM debian:bookworm AS build"),
            ("justfile", "run:"),
            (".env", "APP_ENV=production"),
            ("shell.nix", "{ pkgs ? import <nixpkgs> {} }:"),
            ("app/User.php", "class User {}"),
        ] {
            let d = diff(vec![line(DiffLineKind::Context, text)]);
            let hits =
                DiffHighlights::compute(Path::new(path), &d, &HighlightTheme::default_dark());
            assert!(!hits.line(0, 0).is_empty(), "{path} got no colours");
        }
    }

    /// And that their injections reach the shell inside them, which is most of
    /// what one reads in either file: the body of a `RUN`, the body of a
    /// recipe. Both are one long text node to the outer grammar, so a missing
    /// injection leaves them grey without failing anything.
    ///
    /// `shell_at` is where the shell starts on the second line, and it is what
    /// makes the Dockerfile case a proof: its `RUN` is a keyword of the outer
    /// grammar, so the line has colours either way — only a style *past* the
    /// instruction can have come from bash. A recipe body has none of its own.
    #[test]
    fn a_recipe_body_and_a_run_are_read_as_shell() {
        register_languages();
        for (path, lines, shell_at) in [
            (
                "Dockerfile",
                ["FROM debian AS build", "RUN test -f x && echo yes"],
                "RUN ".len(),
            ),
            ("justfile", ["run:", "    test -f x && echo yes"], 0),
        ] {
            let d = diff(
                lines
                    .iter()
                    .map(|text| line(DiffLineKind::Context, text))
                    .collect(),
            );
            let hits =
                DiffHighlights::compute(Path::new(path), &d, &HighlightTheme::default_dark());
            assert!(
                hits.line(0, 1)
                    .iter()
                    .any(|(range, _)| range.start >= shell_at),
                "{path}: the shell inside it is not coloured"
            );
        }
    }

    #[test]
    fn each_side_keeps_the_lines_that_belong_to_it() {
        let d = diff(vec![
            line(DiffLineKind::Context, "fn a() {}"),
            line(DiffLineKind::Removed, "let x = 1;"),
            line(DiffLineKind::Added, "let y = 2;"),
        ]);

        let (old_text, old_spans) = build_side(&d, Side::Old, false);
        assert_eq!(old_text, "fn a() {}\nlet x = 1;\n");
        assert_eq!(old_spans.len(), 2);
        assert_eq!(old_spans[1].line, 1, "the removed line keeps its index");

        let (new_text, new_spans) = build_side(&d, Side::New, false);
        assert_eq!(new_text, "fn a() {}\nlet y = 2;\n");
        assert_eq!(new_spans[1].line, 2, "the added line keeps its own");
    }

    #[test]
    fn a_removed_line_is_coloured_from_the_old_side() {
        let d = diff(vec![
            line(DiffLineKind::Removed, "let ancien = 1;"),
            line(DiffLineKind::Added, "let nouveau = 2;"),
        ]);
        let highlights =
            DiffHighlights::compute(Path::new("src/x.rs"), &d, &HighlightTheme::default_dark());

        // Both lines must be coloured, each from its own version: that is the
        // whole point of the two passes.
        assert!(
            !highlights.line(0, 0).is_empty(),
            "removed line not coloured"
        );
        assert!(!highlights.line(0, 1).is_empty(), "added line not coloured");
    }

    #[test]
    fn styles_are_relative_to_their_own_line() {
        let d = diff(vec![
            line(DiffLineKind::Context, "fn premiere() {}"),
            line(DiffLineKind::Context, "fn seconde() {}"),
        ]);
        let highlights =
            DiffHighlights::compute(Path::new("src/x.rs"), &d, &HighlightTheme::default_dark());

        // Without the offset, the second line's styles would point past its text
        // and rendering would panic or shift everything.
        for (hunk, count) in [(0usize, 2usize)] {
            for l in 0..count {
                let text_len = d.hunks[hunk].lines[l].text.len();
                for (range, _) in highlights.line(hunk, l) {
                    assert!(
                        range.end <= text_len,
                        "style {range:?} hors de la ligne {l} ({text_len} octets)"
                    );
                }
            }
        }
        assert!(!highlights.line(0, 1).is_empty());
    }

    #[test]
    fn an_unknown_language_yields_nothing() {
        let d = diff(vec![line(DiffLineKind::Added, "n'importe quoi")]);
        let highlights =
            DiffHighlights::compute(Path::new("notes.txt"), &d, &HighlightTheme::default_dark());
        assert!(highlights.is_empty());
        assert!(highlights.line(0, 0).is_empty());
    }
}

#[cfg(test)]
mod php_tests {
    /// **A fragment with no file around it still gets its colours, and its
    /// offsets still describe the fragment.**
    ///
    /// A plugin's excerpt is ten lines torn out of a deployed file: it is named
    /// by its language, never by a path, and PHP needs a `<?php` in front of it
    /// or its grammar reads the lot as HTML text. That prologue is a line, and
    /// a line that stayed in would shift every style down by one — silently,
    /// since a colouring that is wrong is a colouring all the same.
    #[test]
    fn an_excerpt_is_coloured_where_it_actually_is() {
        crate::ui::highlight::register_languages();
        let theme = HighlightTheme::default_dark();
        let excerpt = "    public function store(Request $request)\n    {\n        return $request->quote->total;\n    }";
        let styles = DocumentHighlights::for_language("php", excerpt, &theme);

        let first = styles.line(0);
        assert!(!first.is_empty(), "a PHP fragment with no tag gets styles");
        // The offsets are the fragment's own: `public` is where it is on the
        // first line, not five bytes further along.
        let (range, _) = &first[0];
        assert!(
            range.start >= 4 && range.end <= excerpt.lines().next().unwrap().len(),
            "{first:?}"
        );
        assert!(!styles.line(2).is_empty(), "and so is the third line");
        // A language nobody knows is left plain rather than guessed at.
        assert!(
            DocumentHighlights::for_language("brainfuck", excerpt, &theme)
                .line(0)
                .is_empty()
        );
    }

    fn hunk_of(source: &str) -> FileDiff {
        FileDiff {
            hunks: vec![Hunk {
                header: "@@ -40,4 +40,4 @@".into(),
                old_start: 40,
                new_start: 40,
                lines: source
                    .lines()
                    .map(|text| DiffLine {
                        kind: DiffLineKind::Added,
                        old_no: None,
                        new_no: Some(40),
                        text: text.to_string(),
                    })
                    .collect(),
            }],
            binary: false,
            empty: false,
        }
    }

    fn coloured_lines(diff: &FileDiff) -> usize {
        let styles = DiffHighlights::compute(
            Path::new("Facture.php"),
            diff,
            &HighlightTheme::default_dark(),
        );
        (0..diff.hunks[0].lines.len())
            .filter(|line| !styles.line(0, *line).is_empty())
            .count()
    }

    /// The common case, and the one that seemed broken: a hunk taken from the
    /// middle of a file does not contain `<?php`, and without it the PHP grammar
    /// reads the whole fragment as HTML text — not one colour.
    #[test]
    fn a_hunk_taken_mid_file_is_still_coloured() {
        register_languages();
        let diff = hunk_of(
            "    public function total(): int {\n\
             \x20       return $this->lignes->sum('montant');\n\
             \x20   }",
        );
        assert!(
            coloured_lines(&diff) > 0,
            "a fragment with no opening tag must be coloured"
        );
    }

    /// The body of a `@php` block reaches the grammar as PHP, and that is the
    /// only reason it has colours: without the mask, the whole block is HTML
    /// text and a dozen lines of real code arrive grey.
    #[test]
    fn a_php_block_is_read_as_php() {
        register_languages();
        let diff = hunk_of(
            "<div>\n\
             @php\n\
             \x20   $total = 0;\n\
             \x20   foreach ($lines as $line) { $total += $line->amount; }\n\
             @endphp\n\
             </div>",
        );
        let styles = DiffHighlights::compute(
            Path::new("resources/views/invoice.blade.php"),
            &diff,
            &HighlightTheme::default_dark(),
        );

        let line = &diff.hunks[0].lines[3].text;
        let keyword = styles
            .line(0, 3)
            .iter()
            .find(|(range, _)| &line[range.clone()] == "foreach");
        assert!(
            keyword.is_some(),
            "the block's body stays grey: {:?}",
            styles.line(0, 3)
        );
        assert!(
            !styles.line(0, 1).is_empty() && !styles.line(0, 4).is_empty(),
            "and the markers keep their directive colour"
        );
    }

    /// An enum case is not an all-caps constant, and the grammar's own query
    /// only names those. This is ordinary PHP, not a view: the pattern serves
    /// every diff of the codebase.
    #[test]
    fn an_enum_case_is_coloured() {
        register_languages();
        let diff = hunk_of("$colour = ActionColor::Success;");
        let styles = DiffHighlights::compute(
            Path::new("app/Models/Action.php"),
            &diff,
            &HighlightTheme::default_dark(),
        );
        let line = &diff.hunks[0].lines[0].text;
        let named = |what: &str| {
            let at = line.find(what).expect("the fixture holds it");
            styles
                .line(0, 0)
                .iter()
                .find(|(range, _)| range.start <= at && at < range.end)
                .and_then(|(_, style)| style.color)
        };
        assert!(named("Success").is_some(), "the case has no colour");
        assert!(
            named("ActionColor").is_some() && named("ActionColor") != named("Success"),
            "the class it is read from is not the case"
        );
    }

    /// Nix is registered by hand like PHP, and a registration that does not take
    /// raises no error: it gives a file with no colours, which is exactly what
    /// the absence of a grammar gives.
    #[test]
    fn a_nix_derivation_is_coloured() {
        register_languages();
        let diff = hunk_of(
            "let\n\
             \x20 pkgs = import <nixpkgs> { };\n\
             in\n\
             pkgs.mkShell { buildInputs = [ pkgs.rustc ]; }",
        );
        let styles = DiffHighlights::compute(
            Path::new("shell.nix"),
            &diff,
            &HighlightTheme::default_dark(),
        );
        let coloured = (0..diff.hunks[0].lines.len())
            .filter(|line| !styles.line(0, *line).is_empty())
            .count();
        assert!(coloured > 0, "a nix file must be coloured");
    }

    /// End to end: a Blade view crosses the PHP grammar *and* the overlay, and
    /// both its vocabularies come out coloured.
    #[test]
    fn a_blade_view_is_coloured_by_both_passes() {
        register_languages();
        let source = "<table>\n\
                      @foreach ($factures as $facture)\n\
                      <td>{{ $facture->total }}</td>\n\
                      @endforeach";
        let diff = hunk_of(source);
        let styles = DiffHighlights::compute(
            Path::new("resources/views/factures.blade.php"),
            &diff,
            &HighlightTheme::default_dark(),
        );

        assert!(
            !styles.line(0, 1).is_empty(),
            "la directive @foreach reste grise sans la surcouche"
        );
        assert!(!styles.line(0, 3).is_empty(), "la directive fermante aussi");

        // The echo line carries both the HTML tag, seen by the grammar, and the
        // delimiters, seen by the overlay. The `<td>` is what was missing for a
        // long time without anything saying so: see
        // `html_tags_are_coloured_in_a_view`.
        let echo = styles.line(0, 2);
        let text = &diff.hunks[0].lines[2].text;
        let td = text.find("td").expect("la balise du test");
        assert!(
            echo.iter().any(|(range, _)| range.contains(&td)),
            "the HTML tag is not coloured: {echo:?}"
        );
        assert!(echo.len() >= 3, "styles of the echo: {echo:?}");
        let mut last = 0;
        for (range, _) in echo {
            assert!(range.start >= last, "ranges not sorted: {echo:?}");
            last = range.end;
        }
        assert!(
            last <= text.len(),
            "a style spills out of its line: {last} > {}",
            text.len()
        );
    }

    /// A view's HTML is coloured by the grammar, not by the overlay.
    ///
    /// This test was missing, and its absence cost: the PHP crate's Rust
    /// bindings only expose `injections.scm` — phpdoc and heredocs — so the HTML
    /// injection was never loaded and a whole view arrived grey, tags included.
    /// An injection that does not find its grammar raises no error: only a test
    /// can say so.
    #[test]
    fn html_tags_are_coloured_in_a_view() {
        register_languages();
        let source = "<div class=\"card\">\n\
                      <x-layout.app title=\"Devis\">\n\
                      </x-layout.app>";
        let diff = hunk_of(source);
        let styles = DiffHighlights::compute(
            Path::new("resources/views/devis.blade.php"),
            &diff,
            &HighlightTheme::default_dark(),
        );

        // The tag and its attribute, seen by the grammar.
        let div = styles.line(0, 0);
        let text = &diff.hunks[0].lines[0].text;
        for word in ["div", "class"] {
            let at = text.find(word).expect(word);
            assert!(
                div.iter().any(|(range, _)| range.contains(&at)),
                "« {word} » sans couleur : {div:?}"
            );
        }

        // A dotted component name fits in **one** range and one colour: the
        // grammar, for its part, would read a tag there and then an attribute.
        for line in [1, 2] {
            let text = &diff.hunks[0].lines[line].text;
            let dot = text.find(".app").expect("le point du composant");
            let styled = styles.line(0, line);
            let holding: Vec<_> = styled
                .iter()
                .filter(|(range, _)| range.contains(&dot))
                .collect();
            assert_eq!(holding.len(), 1, "ligne {line} : {styled:?}");
            let name = text.find("x-layout").expect("le nom");
            assert!(
                holding[0].0.contains(&name),
                "the component's name is cut: {styled:?}"
            );
        }
    }

    /// The same benefit outside Blade: an ordinary PHP file mixes HTML into its
    /// code, and it is the same injection that colours it.
    #[test]
    fn html_outside_php_tags_is_coloured_too() {
        register_languages();
        let source = "<ul class=\"liste\">\n\
                      <?php echo $x; ?>\n\
                      </ul>";
        let diff = hunk_of(source);
        let styles = DiffHighlights::compute(
            Path::new("resources/views/legacy.php"),
            &diff,
            &HighlightTheme::default_dark(),
        );
        let text = &diff.hunks[0].lines[0].text;
        let at = text.find("ul").expect("la balise");
        assert!(
            styles.line(0, 0).iter().any(|(r, _)| r.contains(&at)),
            "le HTML hors des balises PHP reste gris : {:?}",
            styles.line(0, 0)
        );
    }

    /// And an ordinary PHP file does not go through it: `@` there is an
    /// operator, not a directive.
    #[test]
    fn a_plain_php_file_is_not_treated_as_blade() {
        assert!(!crate::ui::blade::is_blade(Path::new("app/Facture.php")));
        assert!(crate::ui::blade::is_blade(Path::new(
            "resources/views/facture.blade.php"
        )));
    }

    /// And the prologue must not break what already worked: a fragment already
    /// carrying the tag, or HTML expecting it, does not receive a second one.
    #[test]
    fn a_fragment_that_opens_php_keeps_its_own_prologue() {
        assert_eq!(prologue("php", "class A {}"), "<?php\n");
        assert_eq!(prologue("php", "<?php\nclass A {}"), "");
        assert_eq!(prologue("php", "<div>\n<?= $x ?>"), "");
        assert_eq!(prologue("php", "  <?php echo 1;"), "");
        // The other languages have nothing to prefix.
        assert_eq!(prologue("rust", "fn main() {}"), "");
    }
    use super::*;
    use crate::git::{DiffLine, DiffLineKind, Hunk};

    #[test]
    fn php_is_coloured_once_registered() {
        register_languages();

        let source = "<?php\nclass Facture extends Model {\n    public function total(): int { return 42; }\n}";
        let diff = FileDiff {
            hunks: vec![Hunk {
                header: "@@ -1,4 +1,4 @@".into(),
                old_start: 1,
                new_start: 1,
                lines: source
                    .lines()
                    .map(|text| DiffLine {
                        kind: DiffLineKind::Added,
                        old_no: None,
                        new_no: Some(1),
                        text: text.to_string(),
                    })
                    .collect(),
            }],
            binary: false,
            empty: false,
        };

        assert_eq!(
            language_for_path(Path::new("app/Models/Facture.php")),
            Some("php")
        );
        assert_eq!(
            language_for_path(Path::new("resources/views/facture.blade.php")),
            Some("php"),
            "une vue Blade est du PHP pour nos besoins"
        );

        let highlights = DiffHighlights::compute(
            Path::new("app/Models/Facture.php"),
            &diff,
            &HighlightTheme::default_dark(),
        );
        // The class declaration's line carries at least one keyword.
        assert!(
            !highlights.line(0, 1).is_empty(),
            "la grammaire PHP n'est pas prise en compte"
        );
    }
}

#[cfg(test)]
mod grammar_reuse {
    use super::*;
    use std::path::PathBuf;

    /// A full list of Blade hits stays in the tens of milliseconds.
    ///
    /// This is a **timing** test, which nothing else here is, and the bound is
    /// deliberately a hundred times the real cost: what it locks is not a
    /// performance but the grammar cache. Building a `BladeHighlighter` per hit
    /// compiles the PHP queries per hit — a minute of frozen window for one
    /// search — and it breaks nothing, colours everything correctly, and shows
    /// up nowhere but on the clock.
    #[test]
    fn a_full_blade_list_does_not_rebuild_its_grammar() {
        let results = Results {
            files: (0..100)
                .map(|f| crate::git::search::FileHits {
                    path: PathBuf::from(format!("resources/views/page{f}.blade.php")),
                    hits: (0..(crate::git::search::MAX_HITS / 100))
                        .map(|i| crate::git::search::Hit {
                            line: i as u32 + 1,
                            text: "    <x-layout.app :title=\"$title\">@if($a) {{ $b }} @endif"
                                .into(),
                        })
                        .collect(),
                    capped: false,
                })
                .collect(),
            total: crate::git::search::MAX_HITS,
            truncated: false,
        };
        let started = std::time::Instant::now();
        let hits = HitHighlights::compute(&results, &HighlightTheme::default_dark());
        let elapsed = started.elapsed();
        assert!(!hits.line(0, 0).is_empty(), "a Blade line gets styles");
        assert!(
            elapsed < std::time::Duration::from_secs(5),
            "{} Blade hits took {elapsed:?}: the grammar is being rebuilt",
            crate::git::search::MAX_HITS,
        );
    }
}
