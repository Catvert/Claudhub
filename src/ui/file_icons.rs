//! L'icône et la teinte d'un fichier, d'après son nom.
//!
//! Trois listes s'en servent — l'explorateur, les modifications, la revue de
//! branche : le geste central de Claudhub est de parcourir des listes de
//! fichiers, et reconnaître un `.php` d'un `.md` d'un coup d'œil vaut la table
//! que cela coûte.
//!
//! **Un glyphe par langage, pas un glyphe par famille.** Les icônes de Lucide
//! livrées avec Claudhub ne connaissent que des catégories : tout le code y
//! serait le même `file-code`, et une liste de deux cents fichiers n'y
//! gagnerait rien. Les marques viennent donc de `simple-icons` (CC0), rangées
//! dans `assets/icons/lang/` — un dossier à part, parce qu'elles n'ont ni la
//! même licence ni le même dessin : les nôtres sont des traits, celles-ci des
//! aplats. Les marques restent la propriété de leurs titulaires ; ce sont des
//! repères visuels, pas une revendication.
//!
//! **La teinte vient de la coloration syntaxique du thème**, pas d'une palette
//! à nous et pas des couleurs de marque. Trois raisons : ces noms de style
//! existent dans tous les thèmes livrés — `theme::tests::keys_of` le
//! verrouille —, ils s'accordent au diff affiché à côté, et ils suivent le
//! thème clair ou sombre sans qu'on s'en occupe. Une couleur de marque figée,
//! elle, disparaît sur la moitié des thèmes : un logo noir sur fond sombre est
//! un trou. La teinte n'a donc **aucun sens sémantique** — un `.rs` n'est pas
//! « un type » — c'est une convention d'affichage, assumée comme telle.
//!
//! Un thème de l'utilisateur peut ne pas définir toute la nomenclature, d'où
//! la liste de noms candidats, du plus juste au plus sûrement présent — le
//! même procédé que `blade::Scope::candidates`, et pour la même raison : sans
//! repli, l'icône prendrait la couleur du texte et la distinction
//! disparaîtrait.

use std::path::Path;

use gpui::{App, Hsla};
use gpui_component::ActiveTheme;

/// Ce qu'une ligne montre d'un fichier.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FileLook {
    /// Nom de l'icône, tel que `icons::icon` l'attend — `lang/php` pour une
    /// marque, `file-text` pour une catégorie.
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

// Les teintes, nommées une fois : la table en compte deux cents entrées, et
// une portée écrite en clair à chaque ligne s'y trompe tôt ou tard.
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

/// Le fichier qu'on ne reconnaît pas : ni forme ni couleur particulière.
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

/// Les doubles extensions, essayées avant l'extension simple.
static BY_SUFFIX: &[(&str, FileLook)] = &[
    (".blade.php", FileLook::new("lang/laravel", S_TAG)),
    (".d.ts", FileLook::new("lang/typescript", S_COMMENT)),
    (".tar.gz", FileLook::new("archive", S_COMMENT)),
    (".tar.bz2", FileLook::new("archive", S_COMMENT)),
    (".tar.xz", FileLook::new("archive", S_COMMENT)),
];

/// L'apparence d'un fichier.
///
/// Trois passes, de la plus précise à la plus large. Le **nom entier** vient
/// en premier : `Dockerfile` et `.gitignore` n'ont pas d'extension,
/// `package.json` en a une qui ne dit pas qu'il s'agit de npm, et
/// `Cargo.toml` mérite le logo de Rust plutôt que celui de TOML. Viennent
/// ensuite les **familles d'outils**, qui se déclinent (`.eslintrc`,
/// `.eslintrc.json`, `eslint.config.js`), puis l'**extension**.
pub fn look_of(path: &Path) -> FileLook {
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().to_ascii_lowercase())
        .unwrap_or_default();

    if let Ok(index) = BY_NAME.binary_search_by_key(&name.as_str(), |(key, _)| key) {
        return BY_NAME[index].1;
    }
    if let Some((_, look)) = BY_PREFIX
        .iter()
        .find(|(prefix, _)| name == *prefix || name.starts_with(&format!("{prefix}.")))
    {
        return *look;
    }
    // Les doubles extensions : c'est la première qui porte le sens, et
    // `Path::extension` n'en voit que la seconde. Une vue Blade est du
    // balisage avant d'être du PHP — la coloration fait déjà cette
    // distinction, l'icône n'a pas à la contredire.
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

    /// Les deux tables sont interrogées par recherche binaire : un ordre
    /// alphabétique cassé ne provoque aucune erreur, il fait simplement rater
    /// des entrées — et une entrée ratée se voit comme une icône générique de
    /// plus, ce que personne ne remarque.
    #[test]
    fn the_tables_are_sorted() {
        for (table, name) in [(BY_EXTENSION, "BY_EXTENSION"), (BY_NAME, "BY_NAME")] {
            let keys: Vec<&str> = table.iter().map(|(key, _)| *key).collect();
            let mut sorted = keys.clone();
            sorted.sort_unstable();
            assert_eq!(keys, sorted, "{name} n'est pas trié");
            let mut unique = sorted.clone();
            unique.dedup();
            assert_eq!(unique.len(), sorted.len(), "{name} a un doublon");
        }
    }

    /// Une icône absente ne provoque aucune erreur : gpui rend un vide, et la
    /// ligne perd son repère sans que rien ne le dise. C'est exactement la
    /// faute qu'attire une table de deux cents entrées, et le seul endroit où
    /// elle se voie.
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
                "icône absente du dépôt : assets/{name}"
            );
            // Et surtout : embarquée. Les marques vivent dans un
            // sous-dossier, que le motif d'inclusion de `rust-embed` doit
            // couvrir — un fichier présent sur le disque mais absent du
            // binaire donnerait la même case vide, et seulement en release.
            assert!(
                <crate::ui::Assets as rust_embed::RustEmbed>::get(&name).is_some(),
                "icône non embarquée : {name}"
            );
        }
    }

    #[test]
    fn the_whole_name_wins_over_the_extension() {
        // `package.json` est de npm avant d'être du JSON, et `Cargo.toml` de
        // Rust avant d'être du TOML : c'est ce que l'œil cherche dans une
        // liste.
        assert_eq!(icon_of("package.json"), "lang/npm");
        assert_eq!(icon_of("data/fixtures.json"), "lang/json");
        assert_eq!(icon_of("Cargo.toml"), "lang/rust");
        assert_eq!(icon_of("wt.toml"), "lang/toml");
        assert_eq!(icon_of("Dockerfile"), "lang/docker");
        assert_eq!(icon_of(".gitignore"), "lang/git");
        // La casse ne compte pas : `dockerfile` et `Dockerfile` existent tous
        // les deux dans la nature.
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
        // Un préfixe n'est pas un fragment : `environment.ts` n'est pas `.env`.
        assert_eq!(icon_of("environment.ts"), "lang/typescript");
    }

    #[test]
    fn a_double_extension_wins_over_the_last_one() {
        // Une vue Blade est du balisage avant d'être du PHP.
        assert_eq!(icon_of("resources/views/devis.blade.php"), "lang/laravel");
        assert_eq!(icon_of("app/Http/Kernel.php"), "lang/php");
        // `Path::extension` ne verrait que `bz2`, qui n'est pas dans la table
        // des doubles mais l'est dans celle des simples : les deux mènent au
        // même endroit, et c'est bien ce qu'on veut.
        assert_eq!(icon_of("dist/app.tar.bz2"), "archive");
    }

    #[test]
    fn the_languages_of_a_php_project_are_all_told_apart() {
        // Le cas qui a motivé tout ceci : une revue de branche Laravel.
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
            "deux fichiers de cette liste partagent une icône : {icons:?}"
        );
    }

    #[test]
    fn what_is_not_recognised_stays_a_plain_file() {
        assert_eq!(icon_of("inconnu.qqchose"), "file");
        assert_eq!(icon_of("sans-extension-du-tout"), "file");
        assert_eq!(icon_of(""), "file");
    }
}
