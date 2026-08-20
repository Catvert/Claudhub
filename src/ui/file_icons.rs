//! L'icône et la teinte d'un fichier, d'après son nom.
//!
//! Trois listes s'en servent — l'explorateur, les modifications, la revue de
//! branche — et une quatrième les suivra : le geste central de Claudhub est de
//! parcourir des listes de fichiers, et reconnaître un `.php` d'un `.md` d'un
//! coup d'œil vaut la centaine de lignes de table que cela coûte.
//!
//! **La forme dit la famille, la couleur dit le langage.** Les icônes livrées
//! sont des traits monochromes à la Lucide : dessiner un logo par langage
//! serait un autre métier, et une trentaine de glyphes presque identiques ne
//! se distingueraient pas mieux. La teinte fait ce travail-là.
//!
//! **Les teintes viennent de la coloration syntaxique du thème**, pas d'une
//! palette à nous. Trois raisons : elles existent dans tous les thèmes livrés
//! — `theme::tests::keys_of` le verrouille —, elles s'accordent au diff affiché
//! à côté, et elles suivent le thème clair ou sombre sans qu'on s'en occupe.
//! Elles n'ont pas de sens sémantique : un `.rs` n'est pas « un type ». C'est
//! une convention d'affichage, assumée comme telle.
//!
//! Un thème de l'utilisateur peut ne pas définir toute la nomenclature, d'où
//! la liste de noms candidats, du plus juste au plus sûrement présent — le même
//! procédé que `blade::Scope::candidates`, et pour la même raison : sans repli,
//! l'icône prendrait la couleur du texte et la distinction disparaîtrait.

use std::path::Path;

use gpui::{App, Hsla};
use gpui_component::ActiveTheme;

/// Ce qu'une ligne montre d'un fichier.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FileLook {
    /// Nom de l'icône, tel que `icons::icon` l'attend.
    pub icon: &'static str,
    /// Noms de style de coloration à essayer, du plus juste au plus sûrement
    /// présent. Vide : la couleur du texte ambiant.
    pub scopes: &'static [&'static str],
}

impl FileLook {
    const fn new(icon: &'static str, scopes: &'static [&'static str]) -> Self {
        Self { icon, scopes }
    }

    /// La teinte, prise dans la coloration du thème.
    pub fn color(&self, cx: &App) -> Option<Hsla> {
        let theme = &cx.theme().highlight_theme;
        self.scopes
            .iter()
            .find_map(|name| theme.style(name))
            .and_then(|style| style.color)
    }
}

/// Le fichier ordinaire : ni forme ni couleur particulière.
const PLAIN: FileLook = FileLook::new("file", &[]);

// Les familles. Nommées plutôt qu'écrites en clair dans la table : la même
// apparence sert à une dizaine d'extensions, et une faute de frappe dans un
// nom d'icône ne se voit qu'à l'exécution, sous la forme d'une case vide.
const CODE_TYPE: FileLook = FileLook::new("file-code", &["type", "constructor"]);
const CODE_KEYWORD: FileLook = FileLook::new("file-code", &["keyword"]);
const CODE_FUNCTION: FileLook = FileLook::new("file-code", &["function"]);
const CODE_PROPERTY: FileLook = FileLook::new("file-code", &["property", "variable.special"]);
const MARKUP: FileLook = FileLook::new("file-code", &["tag"]);
const STYLE: FileLook = FileLook::new("palette", &["attribute", "property"]);
const DATA: FileLook = FileLook::new("file-json", &["string"]);
const DOC: FileLook = FileLook::new("file-text", &["title", "text.literal"]);
const PLAIN_TEXT: FileLook = FileLook::new("file-text", &["comment"]);
const IMAGE: FileLook = FileLook::new("image", &["number"]);
const AUDIO: FileLook = FileLook::new("file-audio", &["number"]);
const VIDEO: FileLook = FileLook::new("file-video", &["number"]);
const SHEET: FileLook = FileLook::new("file-spreadsheet", &["number"]);
const ARCHIVE: FileLook = FileLook::new("archive", &["comment"]);
const FONT: FileLook = FileLook::new("file-type", &["comment"]);
const DATABASE: FileLook = FileLook::new("database", &["keyword"]);
const SHELL: FileLook = FileLook::new("terminal", &["string"]);
const BUILD: FileLook = FileLook::new("settings", &["keyword"]);
const VCS: FileLook = FileLook::new("git-branch", &["comment"]);
const LEGAL: FileLook = FileLook::new("book-open", &["comment"]);

/// L'apparence d'un fichier.
///
/// Le **nom entier** est regardé avant l'extension : `Dockerfile`,
/// `Makefile` et `.gitignore` n'en ont pas, et `.env.production` en a une qui
/// ne veut rien dire. C'est aussi ce qui permet à un `.blade.php` d'être du
/// balisage plutôt que du PHP — la coloration fait déjà cette distinction, et
/// l'icône n'a pas à la contredire.
pub fn look_of(path: &Path) -> FileLook {
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().to_ascii_lowercase())
        .unwrap_or_default();

    if let Some(look) = by_name(&name) {
        return look;
    }
    // `.blade.php`, `.d.ts`, `.spec.ts`, `.tar.gz` : c'est la double extension
    // qui porte le sens, et `Path::extension` n'en voit que la moitié.
    if name.ends_with(".blade.php") {
        return MARKUP;
    }
    if name.ends_with(".tar.gz") || name.ends_with(".tar.bz2") || name.ends_with(".tar.xz") {
        return ARCHIVE;
    }
    let extension = name.rsplit_once('.').map(|(_, ext)| ext).unwrap_or("");
    by_extension(extension)
}

/// Les fichiers qui se reconnaissent à leur nom entier.
fn by_name(name: &str) -> Option<FileLook> {
    // Un nom qui commence par `.env` : `.env`, `.env.local`, `.env.example`.
    if name == ".env" || name.starts_with(".env.") {
        return Some(DATA);
    }
    Some(match name {
        "dockerfile" | "containerfile" | "makefile" | "gnumakefile" | "justfile"
        | "cmakelists.txt" | "vagrantfile" | "procfile" | "rakefile" | "gemfile" | "brewfile" => {
            BUILD
        }
        ".gitignore" | ".gitattributes" | ".gitmodules" | ".mailmap" | ".gitkeep" => VCS,
        "license" | "licence" | "license.md" | "copying" | "notice" | "authors" => LEGAL,
        "readme" | "readme.md" | "changelog" | "changelog.md" | "todo" | "todo.md" => DOC,
        _ => return None,
    })
}

/// La table des extensions. Longue par nature : c'est une liste de faits.
fn by_extension(extension: &str) -> FileLook {
    match extension {
        // Ce que l'on relit le plus souvent, chacun avec sa teinte.
        "rs" => CODE_TYPE,
        "php" => CODE_KEYWORD,
        "js" | "mjs" | "cjs" => CODE_FUNCTION,
        "ts" | "mts" | "cts" => CODE_TYPE,
        "jsx" | "tsx" => MARKUP,
        "py" | "pyi" => CODE_FUNCTION,
        "rb" | "rake" | "gemspec" => CODE_KEYWORD,
        "go" => CODE_TYPE,
        "java" | "kt" | "kts" | "scala" | "groovy" => CODE_KEYWORD,
        "cs" | "fs" | "vb" => CODE_KEYWORD,
        "c" | "h" | "cc" | "cpp" | "cxx" | "hpp" | "hh" | "m" | "mm" => CODE_TYPE,
        "swift" | "dart" => CODE_TYPE,
        "ex" | "exs" | "erl" | "hrl" => CODE_FUNCTION,
        "hs" | "ml" | "mli" | "clj" | "cljs" | "lisp" | "scm" | "lua" | "zig" | "nim" | "v" => {
            CODE_FUNCTION
        }
        "pl" | "pm" | "r" | "jl" | "tcl" => CODE_FUNCTION,
        "nix" => CODE_PROPERTY,
        "vue" | "svelte" | "astro" => MARKUP,

        // Balisage et présentation.
        "html" | "htm" | "xhtml" | "xml" | "svg" | "twig" | "hbs" | "ejs" | "erb" | "jinja"
        | "j2" | "liquid" | "mustache" | "pug" | "haml" | "slim" | "razor" | "cshtml" => MARKUP,
        "css" | "scss" | "sass" | "less" | "styl" | "postcss" => STYLE,

        // Données et configuration.
        "json" | "jsonc" | "json5" | "yaml" | "yml" | "toml" | "ini" | "cfg" | "conf" | "env"
        | "properties" | "plist" | "lock" | "editorconfig" | "npmrc" | "nvmrc" | "babelrc"
        | "eslintrc" | "prettierrc" => DATA,

        // Documents.
        "md" | "markdown" | "mdx" | "rst" | "adoc" | "asciidoc" | "org" | "tex" | "typ" => DOC,
        "txt" | "text" | "log" | "diff" | "patch" | "po" | "pot" => PLAIN_TEXT,
        "pdf" | "doc" | "docx" | "odt" | "rtf" | "epub" => DOC,

        // Bases de données.
        "sql" | "psql" | "db" | "sqlite" | "sqlite3" | "dump" => DATABASE,

        // Shell.
        "sh" | "bash" | "zsh" | "fish" | "ksh" | "ps1" | "psm1" | "bat" | "cmd" | "nu" | "awk"
        | "sed" => SHELL,

        // Médias.
        "png" | "jpg" | "jpeg" | "gif" | "webp" | "avif" | "bmp" | "ico" | "icns" | "tiff"
        | "tif" | "heic" | "psd" | "xcf" | "ai" => IMAGE,
        "mp3" | "wav" | "flac" | "ogg" | "oga" | "m4a" | "aac" | "opus" | "wma" | "mid"
        | "midi" => AUDIO,
        "mp4" | "mkv" | "mov" | "avi" | "webm" | "wmv" | "flv" | "m4v" | "mpg" | "mpeg" => VIDEO,
        "csv" | "tsv" | "xls" | "xlsx" | "ods" | "numbers" | "parquet" => SHEET,

        // Le reste.
        "zip" | "gz" | "bz2" | "xz" | "zst" | "tar" | "7z" | "rar" | "jar" | "war" | "phar"
        | "deb" | "rpm" | "apk" | "dmg" | "iso" => ARCHIVE,
        "ttf" | "otf" | "woff" | "woff2" | "eot" | "fnt" => FONT,
        "dockerfile" | "mk" | "cmake" | "gradle" | "bazel" | "bzl" | "ninja" => BUILD,
        _ => PLAIN,
    }
}

/// L'icône d'un fichier, teintée.
///
/// Un `div` autour de l'icône plutôt qu'une couleur posée dessus : `Icon`
/// hérite de la couleur du texte, et c'est le conteneur qui la fixe.
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

    #[test]
    fn a_double_extension_wins_over_the_last_one() {
        // Une vue Blade est du balisage avant d'être du PHP : la coloration
        // fait déjà cette distinction, l'icône ne doit pas la contredire.
        assert_eq!(icon_of("resources/views/devis.blade.php"), "file-code");
        assert_ne!(
            look_of(&PathBuf::from("a.blade.php")),
            look_of(&PathBuf::from("a.php")),
            "la teinte doit distinguer une vue d'un fichier PHP"
        );
        // `Path::extension` ne voit que `gz`, ce qui suffit ici, mais la règle
        // vaut aussi pour `tar.bz2`, dont `bz2` seul n'est pas dans la table.
        assert_eq!(icon_of("dist/app.tar.bz2"), "archive");
    }

    #[test]
    fn files_without_an_extension_are_recognised_by_name() {
        assert_eq!(icon_of("Dockerfile"), "settings");
        assert_eq!(icon_of("Makefile"), "settings");
        assert_eq!(icon_of(".gitignore"), "git-branch");
        assert_eq!(icon_of("LICENSE"), "book-open");
        // La casse ne compte pas : `dockerfile` et `Dockerfile` existent tous
        // les deux dans la nature.
        assert_eq!(icon_of("services/api/dockerfile"), "settings");
    }

    #[test]
    fn every_env_file_is_configuration() {
        for name in [".env", ".env.local", ".env.production", ".env.example"] {
            assert_eq!(icon_of(name), "file-json", "{name}");
        }
    }

    #[test]
    fn the_common_extensions_get_their_family() {
        for (name, expected) in [
            ("src/main.rs", "file-code"),
            ("app/Http/Kernel.php", "file-code"),
            ("assets/app.js", "file-code"),
            ("README.md", "file-text"),
            ("package.json", "file-json"),
            ("style.css", "palette"),
            ("index.html", "file-code"),
            ("schema.sql", "database"),
            ("deploy.sh", "terminal"),
            ("logo.png", "image"),
            ("theme.woff2", "file-type"),
            ("dump.zip", "archive"),
            ("data.csv", "file-spreadsheet"),
            ("inconnu.qqchose", "file"),
            ("sans-extension-du-tout", "file"),
        ] {
            assert_eq!(icon_of(name), expected, "{name}");
        }
    }

    #[test]
    fn languages_of_the_same_family_are_told_apart_by_their_tint() {
        // La forme ne les distingue pas — c'est assumé —, la couleur si.
        let php = look_of(&PathBuf::from("a.php"));
        let js = look_of(&PathBuf::from("a.js"));
        let rs = look_of(&PathBuf::from("a.rs"));
        assert_eq!(
            (php.icon, js.icon, rs.icon),
            ("file-code", "file-code", "file-code")
        );
        assert_ne!(php.scopes, js.scopes);
        assert_ne!(js.scopes, rs.scopes);
    }

    /// Une icône absente ne provoque aucune erreur : gpui rend un vide, et la
    /// ligne perd son repère sans que rien ne le dise. C'est exactement le
    /// genre de faute qu'une table de deux cents entrées attire.
    #[test]
    fn every_icon_named_here_is_really_embedded() {
        let names = [
            PLAIN,
            CODE_TYPE,
            CODE_KEYWORD,
            CODE_FUNCTION,
            CODE_PROPERTY,
            MARKUP,
            STYLE,
            DATA,
            DOC,
            PLAIN_TEXT,
            IMAGE,
            AUDIO,
            VIDEO,
            SHEET,
            ARCHIVE,
            FONT,
            DATABASE,
            SHELL,
            BUILD,
            VCS,
            LEGAL,
        ];
        for look in names {
            let path = format!("assets/icons/{}.svg", look.icon);
            assert!(
                std::path::Path::new(&path).exists(),
                "icône absente du dépôt : {path}"
            );
        }
    }
}
