//! A file's icon and tint, from its name.
//!
//! Three lists use it — the explorer, the changes, the branch review: Claudhub's
//! central gesture is scanning lists of files, and telling a `.php` from a `.md`
//! at a glance is worth the table it costs.
//!
//! **One glyph per language, not one glyph per family.** The Lucide icons
//! shipped with Claudhub know only categories: all code would be the same
//! `file-code` there, and a list of two hundred files would gain nothing. The
//! brand marks therefore come from `simple-icons` (CC0), filed under
//! `assets/icons/lang/` — a separate folder, because they have neither the same
//! licence nor the same drawing: ours are strokes, these are solid shapes. The
//! marks remain the property of their holders; they are visual cues, not a
//! claim.
//!
//! **The tint comes from the theme's syntax highlighting**, not from a palette
//! of our own and not from brand colours. Three reasons: those style names exist
//! in every bundled theme — `theme::tests::keys_of` locks that down — they agree
//! with the diff shown beside them, and they follow the light or dark theme
//! without our doing anything. A fixed brand colour, on the other hand,
//! disappears on half the themes: a black logo on a dark background is a hole.
//! The tint therefore has **no semantic meaning** — a `.rs` is not "a type" — it
//! is a display convention, accepted as such.
//!
//! A user's theme may not define the whole nomenclature, hence the list of
//! candidate names, from the most accurate to the most surely present — the same
//! device as `blade::Scope::candidates`, and for the same reason: without a
//! fallback, the icon would take the text's colour and the distinction would
//! vanish.

use std::path::Path;

use gpui::{App, Hsla};
use gpui_component::ActiveTheme;

/// What a row shows of a file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FileLook {
    /// The icon's name, as `icons::icon` expects it — `lang/php` for a brand
    /// mark, `file-text` for a category.
    pub icon: &'static str,
    /// Highlight style names to try, from the most accurate to the most surely
    /// present. Empty: the ambient text colour.
    pub scopes: &'static [&'static str],
}

impl FileLook {
    const fn new(icon: &'static str, scopes: &'static [&'static str]) -> Self {
        Self { icon, scopes }
    }

    /// The tint, taken from the theme's highlighting.
    pub fn color(&self, cx: &App) -> Option<Hsla> {
        let theme = &cx.theme().highlight_theme;
        self.scopes
            .iter()
            .find_map(|name| theme.style(name))
            .and_then(|style| style.color)
    }
}

// The tints, named once: the table has two hundred entries, and a scope written
// out on every line gets it wrong sooner or later.
const S_NONE: &[&str] = &[];
const S_TYPE: &[&str] = &["type", "constructor"];
const S_KEYWORD: &[&str] = &["keyword"];
const S_FUNCTION: &[&str] = &["function"];
const S_PROPERTY: &[&str] = &["property", "variable.special"];
const S_TAG: &[&str] = &["tag"];
const S_ATTRIBUTE: &[&str] = &["attribute", "property"];
const S_STRING: &[&str] = &["string"];
const S_TITLE: &[&str] = &["title", "text.literal"];
const S_NUMBER: &[&str] = &["number"];
const S_COMMENT: &[&str] = &["comment"];

/// The file we do not recognise: no particular shape or colour.
const PLAIN: FileLook = FileLook::new("file", S_NONE);

static BY_EXTENSION: &[(&str, FileLook)] = &[
    ("7z", FileLook::new("archive", S_COMMENT)),
    ("aac", FileLook::new("file-audio", S_NUMBER)),
    ("adoc", FileLook::new("file-text", S_TITLE)),
    ("ai", FileLook::new("image", S_NUMBER)),
    ("apk", FileLook::new("archive", S_COMMENT)),
    ("asciidoc", FileLook::new("file-text", S_TITLE)),
    ("astro", FileLook::new("lang/astro", S_TAG)),
    ("avi", FileLook::new("file-video", S_NUMBER)),
    ("avif", FileLook::new("image", S_NUMBER)),
    ("awk", FileLook::new("terminal", S_STRING)),
    ("bash", FileLook::new("lang/gnubash", S_STRING)),
    ("bat", FileLook::new("terminal", S_STRING)),
    ("bmp", FileLook::new("image", S_NUMBER)),
    ("bz2", FileLook::new("archive", S_COMMENT)),
    ("bzl", FileLook::new("settings", S_KEYWORD)),
    ("c", FileLook::new("lang/c", S_TYPE)),
    ("cc", FileLook::new("lang/cplusplus", S_TYPE)),
    ("cfg", FileLook::new("file-json", S_STRING)),
    ("cjs", FileLook::new("lang/javascript", S_FUNCTION)),
    ("clj", FileLook::new("lang/clojure", S_FUNCTION)),
    ("cljs", FileLook::new("lang/clojure", S_FUNCTION)),
    ("cmake", FileLook::new("settings", S_KEYWORD)),
    ("cmd", FileLook::new("terminal", S_STRING)),
    ("conf", FileLook::new("file-json", S_STRING)),
    ("cpp", FileLook::new("lang/cplusplus", S_TYPE)),
    ("cs", FileLook::new("lang/dotnet", S_KEYWORD)),
    ("cshtml", FileLook::new("lang/dotnet", S_TAG)),
    ("csproj", FileLook::new("lang/dotnet", S_KEYWORD)),
    ("css", FileLook::new("lang/css", S_ATTRIBUTE)),
    ("csv", FileLook::new("file-spreadsheet", S_NUMBER)),
    ("cts", FileLook::new("lang/typescript", S_TYPE)),
    ("cxx", FileLook::new("lang/cplusplus", S_TYPE)),
    ("dart", FileLook::new("lang/dart", S_TYPE)),
    ("db", FileLook::new("lang/sqlite", S_KEYWORD)),
    ("deb", FileLook::new("archive", S_COMMENT)),
    ("diff", FileLook::new("file-diff", S_COMMENT)),
    ("dmg", FileLook::new("archive", S_COMMENT)),
    ("doc", FileLook::new("book-open", S_COMMENT)),
    ("docx", FileLook::new("book-open", S_COMMENT)),
    ("dump", FileLook::new("database", S_KEYWORD)),
    ("editorconfig", FileLook::new("file-json", S_STRING)),
    ("edn", FileLook::new("lang/clojure", S_STRING)),
    ("ejs", FileLook::new("lang/html5", S_TAG)),
    ("env", FileLook::new("file-json", S_STRING)),
    ("eot", FileLook::new("file-type", S_COMMENT)),
    ("epub", FileLook::new("book-open", S_COMMENT)),
    ("erb", FileLook::new("lang/ruby", S_TAG)),
    ("erl", FileLook::new("lang/erlang", S_FUNCTION)),
    ("ex", FileLook::new("lang/elixir", S_FUNCTION)),
    ("exs", FileLook::new("lang/elixir", S_FUNCTION)),
    ("fish", FileLook::new("terminal", S_STRING)),
    ("flac", FileLook::new("file-audio", S_NUMBER)),
    ("flv", FileLook::new("file-video", S_NUMBER)),
    ("fs", FileLook::new("lang/dotnet", S_KEYWORD)),
    ("gemspec", FileLook::new("lang/ruby", S_KEYWORD)),
    ("gif", FileLook::new("image", S_NUMBER)),
    ("go", FileLook::new("lang/go", S_TYPE)),
    ("gql", FileLook::new("lang/graphql", S_TYPE)),
    ("gradle", FileLook::new("settings", S_KEYWORD)),
    ("graphql", FileLook::new("lang/graphql", S_TYPE)),
    ("gz", FileLook::new("archive", S_COMMENT)),
    ("h", FileLook::new("lang/c", S_TYPE)),
    ("haml", FileLook::new("lang/html5", S_TAG)),
    ("hbs", FileLook::new("lang/html5", S_TAG)),
    ("heex", FileLook::new("lang/elixir", S_TAG)),
    ("heic", FileLook::new("image", S_NUMBER)),
    ("hh", FileLook::new("lang/cplusplus", S_TYPE)),
    ("hpp", FileLook::new("lang/cplusplus", S_TYPE)),
    ("hrl", FileLook::new("lang/erlang", S_FUNCTION)),
    ("hs", FileLook::new("lang/haskell", S_FUNCTION)),
    ("htm", FileLook::new("lang/html5", S_TAG)),
    ("html", FileLook::new("lang/html5", S_TAG)),
    ("icns", FileLook::new("image", S_NUMBER)),
    ("ico", FileLook::new("image", S_NUMBER)),
    ("ini", FileLook::new("file-json", S_STRING)),
    ("iso", FileLook::new("archive", S_COMMENT)),
    ("j2", FileLook::new("lang/html5", S_TAG)),
    ("jar", FileLook::new("lang/openjdk", S_COMMENT)),
    ("java", FileLook::new("lang/openjdk", S_KEYWORD)),
    ("jinja", FileLook::new("lang/html5", S_TAG)),
    ("jl", FileLook::new("lang/julia", S_FUNCTION)),
    ("jpeg", FileLook::new("image", S_NUMBER)),
    ("jpg", FileLook::new("image", S_NUMBER)),
    ("js", FileLook::new("lang/javascript", S_FUNCTION)),
    ("json", FileLook::new("lang/json", S_STRING)),
    ("json5", FileLook::new("lang/json", S_STRING)),
    ("jsonc", FileLook::new("lang/json", S_STRING)),
    ("jsx", FileLook::new("lang/react", S_TAG)),
    ("ksh", FileLook::new("lang/gnubash", S_STRING)),
    ("kt", FileLook::new("lang/kotlin", S_KEYWORD)),
    ("kts", FileLook::new("lang/kotlin", S_KEYWORD)),
    ("less", FileLook::new("lang/less", S_ATTRIBUTE)),
    ("liquid", FileLook::new("lang/html5", S_TAG)),
    ("lock", FileLook::new("file-json", S_COMMENT)),
    ("log", FileLook::new("file-text", S_COMMENT)),
    ("lua", FileLook::new("lang/lua", S_FUNCTION)),
    ("m4a", FileLook::new("file-audio", S_NUMBER)),
    ("m4v", FileLook::new("file-video", S_NUMBER)),
    ("markdown", FileLook::new("lang/markdown", S_TITLE)),
    ("md", FileLook::new("lang/markdown", S_TITLE)),
    ("mdx", FileLook::new("lang/markdown", S_TITLE)),
    ("mid", FileLook::new("file-audio", S_NUMBER)),
    ("midi", FileLook::new("file-audio", S_NUMBER)),
    ("mjs", FileLook::new("lang/javascript", S_FUNCTION)),
    ("mk", FileLook::new("settings", S_KEYWORD)),
    ("mkv", FileLook::new("file-video", S_NUMBER)),
    ("ml", FileLook::new("lang/ocaml", S_FUNCTION)),
    ("mli", FileLook::new("lang/ocaml", S_FUNCTION)),
    ("mov", FileLook::new("file-video", S_NUMBER)),
    ("mp3", FileLook::new("file-audio", S_NUMBER)),
    ("mp4", FileLook::new("file-video", S_NUMBER)),
    ("mpeg", FileLook::new("file-video", S_NUMBER)),
    ("mpg", FileLook::new("file-video", S_NUMBER)),
    ("mts", FileLook::new("lang/typescript", S_TYPE)),
    ("mustache", FileLook::new("lang/html5", S_TAG)),
    ("nim", FileLook::new("lang/nim", S_TYPE)),
    ("ninja", FileLook::new("settings", S_KEYWORD)),
    ("nix", FileLook::new("lang/nixos", S_PROPERTY)),
    ("nu", FileLook::new("terminal", S_STRING)),
    ("ods", FileLook::new("file-spreadsheet", S_NUMBER)),
    ("odt", FileLook::new("book-open", S_COMMENT)),
    ("oga", FileLook::new("file-audio", S_NUMBER)),
    ("ogg", FileLook::new("file-audio", S_NUMBER)),
    ("opus", FileLook::new("file-audio", S_NUMBER)),
    ("org", FileLook::new("file-text", S_TITLE)),
    ("otf", FileLook::new("file-type", S_COMMENT)),
    ("parquet", FileLook::new("file-spreadsheet", S_NUMBER)),
    ("patch", FileLook::new("file-diff", S_COMMENT)),
    ("pdf", FileLook::new("book-open", S_COMMENT)),
    ("phar", FileLook::new("lang/php", S_COMMENT)),
    ("php", FileLook::new("lang/php", S_KEYWORD)),
    ("pl", FileLook::new("lang/perl", S_FUNCTION)),
    ("plist", FileLook::new("file-json", S_STRING)),
    ("pm", FileLook::new("lang/perl", S_FUNCTION)),
    ("png", FileLook::new("image", S_NUMBER)),
    ("po", FileLook::new("file-text", S_COMMENT)),
    ("postcss", FileLook::new("lang/css", S_ATTRIBUTE)),
    ("pot", FileLook::new("file-text", S_COMMENT)),
    ("properties", FileLook::new("file-json", S_STRING)),
    ("ps1", FileLook::new("terminal", S_STRING)),
    ("psd", FileLook::new("image", S_NUMBER)),
    ("psm1", FileLook::new("terminal", S_STRING)),
    ("psql", FileLook::new("database", S_KEYWORD)),
    ("pug", FileLook::new("lang/html5", S_TAG)),
    ("py", FileLook::new("lang/python", S_FUNCTION)),
    ("pyi", FileLook::new("lang/python", S_FUNCTION)),
    ("pyw", FileLook::new("lang/python", S_FUNCTION)),
    ("rake", FileLook::new("lang/ruby", S_KEYWORD)),
    ("rar", FileLook::new("archive", S_COMMENT)),
    ("razor", FileLook::new("lang/dotnet", S_TAG)),
    ("rb", FileLook::new("lang/ruby", S_KEYWORD)),
    ("rpm", FileLook::new("archive", S_COMMENT)),
    ("rs", FileLook::new("lang/rust", S_TYPE)),
    ("rst", FileLook::new("file-text", S_TITLE)),
    ("rtf", FileLook::new("book-open", S_COMMENT)),
    ("sass", FileLook::new("lang/sass", S_ATTRIBUTE)),
    ("sbt", FileLook::new("lang/scala", S_KEYWORD)),
    ("scala", FileLook::new("lang/scala", S_KEYWORD)),
    ("scss", FileLook::new("lang/sass", S_ATTRIBUTE)),
    ("sh", FileLook::new("lang/gnubash", S_STRING)),
    ("slim", FileLook::new("lang/html5", S_TAG)),
    ("sln", FileLook::new("lang/dotnet", S_KEYWORD)),
    ("sql", FileLook::new("database", S_KEYWORD)),
    ("sqlite", FileLook::new("lang/sqlite", S_KEYWORD)),
    ("sqlite3", FileLook::new("lang/sqlite", S_KEYWORD)),
    ("styl", FileLook::new("palette", S_ATTRIBUTE)),
    ("svelte", FileLook::new("lang/svelte", S_TAG)),
    ("svg", FileLook::new("lang/xml", S_TAG)),
    ("swift", FileLook::new("lang/swift", S_TYPE)),
    ("tar", FileLook::new("archive", S_COMMENT)),
    ("tex", FileLook::new("file-text", S_TITLE)),
    ("text", FileLook::new("file-text", S_COMMENT)),
    ("tif", FileLook::new("image", S_NUMBER)),
    ("tiff", FileLook::new("image", S_NUMBER)),
    ("toml", FileLook::new("lang/toml", S_STRING)),
    ("ts", FileLook::new("lang/typescript", S_TYPE)),
    ("tsv", FileLook::new("file-spreadsheet", S_NUMBER)),
    ("tsx", FileLook::new("lang/react", S_TAG)),
    ("ttf", FileLook::new("file-type", S_COMMENT)),
    ("twig", FileLook::new("lang/html5", S_TAG)),
    ("txt", FileLook::new("file-text", S_COMMENT)),
    ("typ", FileLook::new("file-text", S_TITLE)),
    ("vb", FileLook::new("lang/dotnet", S_KEYWORD)),
    ("vue", FileLook::new("lang/vuedotjs", S_TAG)),
    ("war", FileLook::new("lang/openjdk", S_COMMENT)),
    ("wav", FileLook::new("file-audio", S_NUMBER)),
    ("webm", FileLook::new("file-video", S_NUMBER)),
    ("webp", FileLook::new("image", S_NUMBER)),
    ("wmv", FileLook::new("file-video", S_NUMBER)),
    ("woff", FileLook::new("file-type", S_COMMENT)),
    ("woff2", FileLook::new("file-type", S_COMMENT)),
    ("xcf", FileLook::new("image", S_NUMBER)),
    ("xhtml", FileLook::new("lang/html5", S_TAG)),
    ("xls", FileLook::new("file-spreadsheet", S_NUMBER)),
    ("xlsx", FileLook::new("file-spreadsheet", S_NUMBER)),
    ("xml", FileLook::new("lang/xml", S_TAG)),
    ("xz", FileLook::new("archive", S_COMMENT)),
    ("yaml", FileLook::new("lang/yaml", S_STRING)),
    ("yml", FileLook::new("lang/yaml", S_STRING)),
    ("zig", FileLook::new("lang/zig", S_TYPE)),
    ("zip", FileLook::new("archive", S_COMMENT)),
    ("zsh", FileLook::new("lang/gnubash", S_STRING)),
    ("zst", FileLook::new("archive", S_COMMENT)),
];

static BY_NAME: &[(&str, FileLook)] = &[
    (".dockerignore", FileLook::new("lang/docker", S_COMMENT)),
    (".gitattributes", FileLook::new("lang/git", S_COMMENT)),
    (".gitignore", FileLook::new("lang/git", S_COMMENT)),
    (".gitkeep", FileLook::new("lang/git", S_COMMENT)),
    (".gitmodules", FileLook::new("lang/git", S_COMMENT)),
    (".mailmap", FileLook::new("lang/git", S_COMMENT)),
    ("artisan", FileLook::new("lang/laravel", S_KEYWORD)),
    ("authors", FileLook::new("book-open", S_COMMENT)),
    ("brewfile", FileLook::new("settings", S_KEYWORD)),
    ("bun.lockb", FileLook::new("lang/bun", S_COMMENT)),
    ("cargo.lock", FileLook::new("lang/rust", S_COMMENT)),
    ("cargo.toml", FileLook::new("lang/rust", S_TYPE)),
    ("changelog", FileLook::new("lang/markdown", S_TITLE)),
    ("changelog.md", FileLook::new("lang/markdown", S_TITLE)),
    ("cmakelists.txt", FileLook::new("settings", S_KEYWORD)),
    ("compose.yaml", FileLook::new("lang/docker", S_KEYWORD)),
    ("compose.yml", FileLook::new("lang/docker", S_KEYWORD)),
    ("composer.json", FileLook::new("lang/composer", S_STRING)),
    ("composer.lock", FileLook::new("lang/composer", S_COMMENT)),
    ("containerfile", FileLook::new("lang/docker", S_KEYWORD)),
    ("copying", FileLook::new("book-open", S_COMMENT)),
    ("default.nix", FileLook::new("lang/nixos", S_PROPERTY)),
    ("deno.json", FileLook::new("lang/deno", S_STRING)),
    ("deno.lock", FileLook::new("lang/deno", S_COMMENT)),
    (
        "docker-compose.yaml",
        FileLook::new("lang/docker", S_KEYWORD),
    ),
    (
        "docker-compose.yml",
        FileLook::new("lang/docker", S_KEYWORD),
    ),
    ("dockerfile", FileLook::new("lang/docker", S_KEYWORD)),
    ("flake.lock", FileLook::new("lang/nixos", S_COMMENT)),
    ("flake.nix", FileLook::new("lang/nixos", S_PROPERTY)),
    ("gemfile", FileLook::new("lang/ruby", S_KEYWORD)),
    ("gemfile.lock", FileLook::new("lang/ruby", S_COMMENT)),
    ("gnumakefile", FileLook::new("settings", S_KEYWORD)),
    ("justfile", FileLook::new("settings", S_KEYWORD)),
    ("licence", FileLook::new("book-open", S_COMMENT)),
    ("license", FileLook::new("book-open", S_COMMENT)),
    ("license.md", FileLook::new("book-open", S_COMMENT)),
    ("makefile", FileLook::new("settings", S_KEYWORD)),
    ("notice", FileLook::new("book-open", S_COMMENT)),
    ("package-lock.json", FileLook::new("lang/npm", S_COMMENT)),
    ("package.json", FileLook::new("lang/npm", S_STRING)),
    ("pnpm-lock.yaml", FileLook::new("lang/pnpm", S_COMMENT)),
    ("procfile", FileLook::new("settings", S_KEYWORD)),
    ("rakefile", FileLook::new("lang/ruby", S_KEYWORD)),
    ("readme", FileLook::new("lang/markdown", S_TITLE)),
    ("readme.md", FileLook::new("lang/markdown", S_TITLE)),
    ("shell.nix", FileLook::new("lang/nixos", S_PROPERTY)),
    ("todo", FileLook::new("file-text", S_TITLE)),
    ("todo.md", FileLook::new("lang/markdown", S_TITLE)),
    ("vagrantfile", FileLook::new("settings", S_KEYWORD)),
    ("yarn.lock", FileLook::new("lang/yarn", S_COMMENT)),
];

static BY_PREFIX: &[(&str, FileLook)] = &[
    (".env", FileLook::new("file-json", S_STRING)),
    (".eslintrc", FileLook::new("lang/eslint", S_STRING)),
    ("eslint.config", FileLook::new("lang/eslint", S_STRING)),
    (".prettierrc", FileLook::new("lang/prettier", S_STRING)),
    ("prettier.config", FileLook::new("lang/prettier", S_STRING)),
    (
        "tailwind.config",
        FileLook::new("lang/tailwindcss", S_ATTRIBUTE),
    ),
    ("vite.config", FileLook::new("lang/vite", S_FUNCTION)),
    ("webpack.config", FileLook::new("lang/webpack", S_FUNCTION)),
    ("tsconfig", FileLook::new("lang/typescript", S_STRING)),
    ("jsconfig", FileLook::new("lang/javascript", S_STRING)),
];

/// The double extensions, tried before the plain one.
static BY_SUFFIX: &[(&str, FileLook)] = &[
    (".blade.php", FileLook::new("lang/laravel", S_TAG)),
    (".d.ts", FileLook::new("lang/typescript", S_COMMENT)),
    (".tar.gz", FileLook::new("archive", S_COMMENT)),
    (".tar.bz2", FileLook::new("archive", S_COMMENT)),
    (".tar.xz", FileLook::new("archive", S_COMMENT)),
];

/// A file's look.
///
/// Three passes, from the most precise to the widest. The **whole name** comes
/// first: `Dockerfile` and `.gitignore` have no extension, `package.json` has
/// one that does not say it belongs to npm, and `Cargo.toml` deserves Rust's
/// logo rather than TOML's. Then come the **tool families**, which have
/// variants (`.eslintrc`, `.eslintrc.json`, `eslint.config.js`), then the
/// **extension**.
pub fn look_of(path: &Path) -> FileLook {
    // Lowercased only when it has to be: this runs for every visible row of
    // the tree, at every frame, and most file names are lowercase already.
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy())
        .unwrap_or_default();
    let name = match name.bytes().any(|byte| byte.is_ascii_uppercase()) {
        true => std::borrow::Cow::Owned(name.to_ascii_lowercase()),
        false => name,
    };

    if let Ok(index) = BY_NAME.binary_search_by_key(&&*name, |(key, _)| key) {
        return BY_NAME[index].1;
    }
    // `strip_prefix` and not a formatted `"{prefix}."`: ten allocations per
    // row per frame, for a comparison that needs none.
    if let Some((_, look)) = BY_PREFIX.iter().find(|(prefix, _)| {
        name.strip_prefix(*prefix)
            .is_some_and(|rest| rest.is_empty() || rest.starts_with('.'))
    }) {
        return *look;
    }
    // The double extensions: it is the first that carries the meaning, and
    // `Path::extension` only sees the second. A Blade view is markup before it
    // is PHP — the highlighting already makes that distinction, and the icon has
    // no business contradicting it.
    for (suffix, look) in BY_SUFFIX {
        if name.ends_with(suffix) {
            return *look;
        }
    }
    let extension = name.rsplit_once('.').map(|(_, ext)| ext).unwrap_or("");
    BY_EXTENSION
        .binary_search_by_key(&extension, |(key, _)| key)
        .map(|index| BY_EXTENSION[index].1)
        .unwrap_or(PLAIN)
}

/// A file's icon, tinted.
///
/// A `div` around the icon rather than a colour set on it: `Icon` inherits the
/// text colour, and it is the container that fixes it.
pub fn file_icon(path: &Path, cx: &App) -> gpui::AnyElement {
    use gpui::prelude::*;
    use gpui_component::Sizable;

    let look = look_of(path);
    gpui::div()
        .flex_none()
        .when_some(look.color(cx), |el, color| el.text_color(color))
        .child(crate::ui::icons::icon(look.icon).xsmall())
        .into_any_element()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn icon_of(name: &str) -> &'static str {
        look_of(&PathBuf::from(name)).icon
    }

    /// Both tables are queried by binary search: a broken alphabetical order
    /// raises no error, it simply makes entries be missed — and a missed entry
    /// shows up as one more generic icon, which nobody notices.
    #[test]
    fn the_tables_are_sorted() {
        for (table, name) in [(BY_EXTENSION, "BY_EXTENSION"), (BY_NAME, "BY_NAME")] {
            let keys: Vec<&str> = table.iter().map(|(key, _)| *key).collect();
            let mut sorted = keys.clone();
            sorted.sort_unstable();
            assert_eq!(keys, sorted, "{name} is not sorted");
            let mut unique = sorted.clone();
            unique.dedup();
            assert_eq!(unique.len(), sorted.len(), "{name} a un doublon");
        }
    }

    /// A missing icon raises no error: gpui renders a blank, and the row loses
    /// its cue without anything saying so. It is exactly the mistake a
    /// two-hundred-entry table attracts, and the only place it shows.
    #[test]
    fn every_icon_named_in_the_tables_is_really_there() {
        let looks = BY_EXTENSION
            .iter()
            .chain(BY_NAME)
            .chain(BY_SUFFIX)
            .chain(BY_PREFIX)
            .map(|(_, look)| look)
            .chain(std::iter::once(&PLAIN));
        for look in looks {
            let name = format!("icons/{}.svg", look.icon);
            assert!(
                std::path::Path::new(&format!("assets/{name}")).exists(),
                "icon missing from the repository: assets/{name}"
            );
            // And above all: embedded. The brand marks live in a subfolder,
            // which `rust-embed`'s include pattern has to cover — a file present
            // on disk but absent from the binary would give the same blank slot,
            // and only in release.
            assert!(
                <crate::ui::Assets as rust_embed::RustEmbed>::get(&name).is_some(),
                "icon not embedded: {name}"
            );
        }
    }

    #[test]
    fn the_whole_name_wins_over_the_extension() {
        // `package.json` belongs to npm before it belongs to JSON, and
        // `Cargo.toml` to Rust before TOML: that is what the eye looks for in a
        // list.
        assert_eq!(icon_of("package.json"), "lang/npm");
        assert_eq!(icon_of("data/fixtures.json"), "lang/json");
        assert_eq!(icon_of("Cargo.toml"), "lang/rust");
        assert_eq!(icon_of("wt.toml"), "lang/toml");
        assert_eq!(icon_of("Dockerfile"), "lang/docker");
        assert_eq!(icon_of(".gitignore"), "lang/git");
        // Case does not count: `dockerfile` and `Dockerfile` both exist in the
        // wild.
        assert_eq!(icon_of("services/api/dockerfile"), "lang/docker");
    }

    #[test]
    fn a_tool_family_is_recognised_through_its_variants() {
        for name in [".eslintrc", ".eslintrc.json", ".eslintrc.cjs"] {
            assert_eq!(icon_of(name), "lang/eslint", "{name}");
        }
        for name in [".env", ".env.local", ".env.production"] {
            assert_eq!(icon_of(name), "file-json", "{name}");
        }
        assert_eq!(icon_of("vite.config.ts"), "lang/vite");
        // A prefix is not a fragment: `environment.ts` is not `.env`.
        assert_eq!(icon_of("environment.ts"), "lang/typescript");
    }

    #[test]
    fn a_double_extension_wins_over_the_last_one() {
        // A Blade view is markup before it is PHP.
        assert_eq!(icon_of("resources/views/quote.blade.php"), "lang/laravel");
        assert_eq!(icon_of("app/Http/Kernel.php"), "lang/php");
        // `Path::extension` would only see `bz2`, which is not in the double
        // table but is in the plain one: both lead to the same place, and that
        // is exactly what we want.
        assert_eq!(icon_of("dist/app.tar.bz2"), "archive");
    }

    #[test]
    fn the_languages_of_a_php_project_are_all_told_apart() {
        // The case that motivated all this: a Laravel branch review.
        let names = [
            "app/Http/Kernel.php",
            "resources/views/devis.blade.php",
            "resources/js/app.js",
            "resources/js/Pages/Devis.vue",
            "resources/css/app.css",
            "tailwind.config.js",
            "database/migrations/create_devis.sql",
            "composer.json",
            "package.json",
            "docker-compose.yml",
            "README.md",
            "public/logo.svg",
            "public/favicon.ico",
        ];
        let icons: Vec<&str> = names.iter().map(|name| icon_of(name)).collect();
        let mut unique = icons.clone();
        unique.sort_unstable();
        unique.dedup();
        assert_eq!(
            unique.len(),
            icons.len(),
            "two files in this list share an icon: {icons:?}"
        );
    }

    #[test]
    fn what_is_not_recognised_stays_a_plain_file() {
        assert_eq!(icon_of("inconnu.qqchose"), "file");
        assert_eq!(icon_of("sans-extension-du-tout"), "file");
        assert_eq!(icon_of(""), "file");
    }
}
