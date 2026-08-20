//! Coloration syntaxique des diffs.
//!
//! Le contenu d'un diff est du code, et c'est le code qu'on relit — pas les
//! marqueurs `+`/`-`. Claudhub colore donc les lignes avec la grammaire du
//! *fichier*, pas avec la grammaire `diff`.
//!
//! Le problème que cela pose : un hunk n'est pas un fichier. Il commence au
//! milieu d'une fonction, saute des dizaines de lignes, et mêle deux versions
//! du texte. La parade est de reconstruire les deux versions — l'ancienne
//! (contexte + lignes supprimées) et la nouvelle (contexte + lignes ajoutées)
//! — de colorer chacune **une seule fois**, puis de redistribuer les styles
//! ligne par ligne. Le parse reste imparfait aux frontières des hunks, où il
//! manque au parseur ce qui a été élidé ; en pratique il s'en remet, parce que
//! les grammaires tree-sitter récupèrent sur erreur.
//!
//! Le coût est payé une fois par fichier ouvert, à l'arrivée du diff, jamais
//! pendant un rendu : `SyntaxHighlighter::new` compile les requêtes de la
//! grammaire, ce qui n'a rien à faire dans une frame.

use std::ops::Range;
use std::path::Path;

use gpui::HighlightStyle;
use gpui_component::highlighter::{
    HighlightTheme, LanguageConfig, LanguageRegistry, SyntaxHighlighter,
};
use gpui_component::input::Rope;

use super::blade;
use crate::git::{DiffLineKind, FileDiff};

/// Enregistre les grammaires que gpui-component n'embarque pas.
///
/// PHP en est absent, alors que c'est le langage de la moitié des dépôts que
/// Claudhub sert à relire ; sa grammaire est donc liée en direct et déclarée dans
/// le registre partagé, d'où le reste de la bibliothèque la retrouvera sous le
/// nom `php` comme n'importe quelle autre.
///
/// À appeler une fois au démarrage, avant tout rendu : le registre est un
/// singleton verrouillé, et l'enregistrer sous une frappe reviendrait à le
/// faire pendant qu'un highlighter le lit.
pub fn register_languages() {
    // Les injections décrivent le HTML qui entoure le PHP et le SQL des
    // chaînes de requête : sans elles, un fichier Blade ou une vue n'aurait de
    // couleurs que dans ses balises `<?php`.
    let php = LanguageConfig::new(
        "php",
        tree_sitter_php::LANGUAGE_PHP.into(),
        vec!["html".into(), "sql".into()],
        tree_sitter_php::HIGHLIGHTS_QUERY,
        tree_sitter_php::INJECTIONS_QUERY,
        "",
    );
    LanguageRegistry::singleton().register("php", &php);
}

/// Les styles d'une ligne, en décalages d'octets relatifs à son texte.
pub type LineStyles = Vec<(Range<usize>, HighlightStyle)>;

/// Les styles d'un diff entier, indexés `[hunk][ligne]`.
#[derive(Default)]
pub struct DiffHighlights {
    hunks: Vec<Vec<LineStyles>>,
}

impl DiffHighlights {
    /// Les styles d'une ligne, ou une tranche vide si le fichier n'a pas de
    /// grammaire connue — auquel cas la vue affiche du texte nu.
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

    /// Colore un diff. Rend un ensemble vide si l'extension n'est associée à
    /// aucune grammaire, ce qui est le cas le plus fréquent (fichiers de
    /// données, textes, binaires).
    pub fn compute(path: &Path, diff: &FileDiff, theme: &HighlightTheme) -> Self {
        let Some(language) = language_for_path(path) else {
            return Self::default();
        };
        if diff.hunks.is_empty() {
            return Self::default();
        }

        // Deux passes : l'ancienne version puis la nouvelle. Les lignes de
        // contexte appartiennent aux deux, et reçoivent les styles de la
        // seconde — elles sont identiques des deux côtés, donc le choix est
        // sans conséquence, mais il faut en faire un.
        let mut styles: Vec<Vec<LineStyles>> = diff
            .hunks
            .iter()
            .map(|hunk| vec![LineStyles::new(); hunk.lines.len()])
            .collect();

        // Une vue Blade est du HTML avant d'être du PHP : lui préfixer une
        // balise d'ouverture ferait lire ses balises comme du code. Ses
        // constructions propres — directives, échos, commentaires — sont
        // ajoutées après coup, la grammaire PHP n'en connaissant aucune.
        let blade = blade::is_blade(path);

        // Une seule instance pour les deux passes : `SyntaxHighlighter::new`
        // compile les requêtes de la grammaire — près de quarante
        // millisecondes pour JavaScript — alors que `update` ne fait que
        // reparser un texte. En créer deux doublait le coût fixe de chaque
        // fichier ouvert.
        let mut highlighter = SyntaxHighlighter::new(language);
        for side in [Side::Old, Side::New] {
            let (mut text, mut spans) = build_side(diff, side);
            if spans.is_empty() {
                continue;
            }
            // Le fragment reçoit d'abord de quoi être reconnu par sa
            // grammaire. Les positions des lignes suivent le décalage : le
            // prologue n'appartient à aucune d'elles, ses styles sont donc
            // ignorés d'eux-mêmes.
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

/// Ce qu'il faut écrire devant un fragment pour que sa grammaire le
/// reconnaisse.
///
/// PHP est le cas qui l'impose : sans `<?php`, sa grammaire lit **tout** le
/// fragment comme du texte HTML, et pas une couleur n'en sort. Or un hunk
/// commence presque toujours au milieu du fichier, donc après la balise
/// d'ouverture — c'est le cas courant, pas l'exception, ce qui explique que la
/// coloration paraissait cassée « très souvent ».
///
/// Le prologue n'est ajouté que s'il manque : une vue dont le hunk contient le
/// début du fichier commence bien par `<?php` ou par du HTML, et lui en
/// préfixer un second casserait le parse. Les vues Blade n'en reçoivent jamais
/// — voir `blade`.
fn prologue(language: &str, fragment: &str) -> &'static str {
    match language {
        "php" if !opens_php(fragment) => "<?php\n",
        _ => "",
    }
}

/// Vrai si le fragment porte déjà une balise d'ouverture PHP, ou du HTML qui
/// en attend une plus loin.
///
/// Le HTML compte : une vue commence par `<div>` et bascule en PHP ensuite,
/// et la grammaire complète est faite pour ce mélange.
fn opens_php(fragment: &str) -> bool {
    fragment
        .lines()
        .take(PROLOGUE_LOOKAHEAD)
        .any(|line| line.contains("<?"))
        || fragment.trim_start().starts_with('<')
}

/// Combien de lignes on examine avant de conclure qu'il manque la balise.
///
/// Tout le fragment serait inutile : une balise qui n'apparaît qu'à la
/// cinquantième ligne laisse de toute façon les précédentes sans couleur, et
/// c'est justement ce qu'on veut corriger.
const PROLOGUE_LOOKAHEAD: usize = 3;

/// Où se trouve une ligne du diff dans le texte reconstruit.
struct Span {
    hunk: usize,
    line: usize,
    range: Range<usize>,
}

/// Reconstruit une version du fichier et note la position de chaque ligne.
fn build_side(diff: &FileDiff, side: Side) -> (String, Vec<Span>) {
    let mut text = String::new();
    let mut spans = Vec::new();
    for (h, hunk) in diff.hunks.iter().enumerate() {
        for (l, line) in hunk.lines.iter().enumerate() {
            if !side.keeps(line.kind) {
                continue;
            }
            let start = text.len();
            text.push_str(&line.text);
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

/// Redistribue les styles du texte reconstruit vers les lignes du diff.
///
/// Les deux listes sont triées par décalage croissant, ce qui permet un seul
/// parcours conjoint : un style qui déborde d'une ligne sur la suivante est
/// coupé à la frontière plutôt que jeté — c'est le cas d'une chaîne
/// multiligne, dont chaque morceau doit rester coloré.
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
        // Une ligne de contexte appartient aux deux versions et est donc
        // visitée deux fois : la seconde passe remplace la première au lieu de
        // s'y ajouter. Accumuler produirait des plages en double et non
        // triées, ce que le rendu traduit en décalage silencieux de toute la
        // coloration à partir du doublon.
        target.clear();
        // Avance jusqu'au premier style qui touche cette ligne. Les deux
        // listes étant triées, ce curseur ne recule jamais : le parcours est
        // linéaire et non quadratique, ce qui compte sur un diff de plusieurs
        // milliers de lignes.
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

/// Grammaire associée à une extension.
///
/// La liste ne couvre que ce que `gpui-component` embarque avec la
/// caractéristique `tree-sitter-languages` : une extension absente rend
/// `None`, et la vue affiche du texte nu plutôt qu'une coloration fausse.
/// Certains langages embarqués (`swift`, `csharp`, `proto`, `cmake`,
/// `graphql`) ont une requête de coloration vide en amont ; les lister
/// n'apporterait rien, ils sont donc omis.
pub fn language_for_path(path: &Path) -> Option<&'static str> {
    // Quelques fichiers se reconnaissent à leur nom, pas à leur extension.
    if let Some(name) = path.file_name().and_then(|n| n.to_str()) {
        match name {
            "Makefile" | "makefile" | "GNUmakefile" => return Some("make"),
            "Dockerfile" => return None,
            _ => {}
        }
    }
    let extension = path.extension()?.to_str()?.to_ascii_lowercase();
    Some(match extension.as_str() {
        "rs" => "rust",
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
        // Les vues Blade sont du PHP entrecoupé de HTML : la grammaire PHP les
        // couvre par ses injections, et sa directive `@if` non reconnue coûte
        // moins qu'un fichier entier sans couleurs.
        "php" | "phtml" | "blade" => "php",
        "css" | "scss" => "css",
        "html" | "htm" => "html",
        "json" => "json",
        "toml" => "toml",
        "yaml" | "yml" => "yaml",
        "sql" => "sql",
        "md" | "markdown" => "markdown",
        "diff" | "patch" => "diff",
        _ => return None,
    })
}

/// Pose des fonds par-dessus une coloration existante.
///
/// C'est ce qui rend visible une occurrence de recherche **dans** du code
/// coloré : le fond marque la trouvaille, la grammaire garde ses couleurs de
/// texte. Repeindre la ligne entière ferait perdre l'un ou l'autre.
///
/// Les deux invariants de `with_highlights` valent ici aussi, et gpui n'en
/// vérifie aucun : les plages rendues sont **triées et disjointes** — la
/// fonction convertit en longueurs de runs consécutives, et une plage
/// désordonnée décale tout ce qui suit — et les décalages sont en **octets**.
/// `base` et `marks` doivent l'être également, chacun de son côté ; ils
/// peuvent en revanche se chevaucher entre eux, c'est même le cas courant.
pub fn overlay(
    base: &[(Range<usize>, HighlightStyle)],
    marks: &[(Range<usize>, gpui::Hsla)],
) -> Vec<(Range<usize>, HighlightStyle)> {
    if marks.is_empty() {
        return base.to_vec();
    }
    // Toutes les frontières des deux découpages : entre deux d'entre elles, ni
    // le style de fond ni celui du texte ne change, et le segment est donc
    // uniforme par construction.
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
            // Ni coloration ni occurrence : le texte reste au style ambiant,
            // et une plage sans effet n'a rien à faire dans la liste.
            continue;
        };
        if let Some(color) = mark {
            style.background_color = Some(color);
        }
        // Deux segments voisins de même style se recollent : `with_highlights`
        // en fait des runs, et deux runs identiques côte à côte sont un coût
        // de mise en page pour rien.
        match out.last_mut() {
            Some((last, previous)) if last.end == start && *previous == style => last.end = end,
            _ => out.push((start..end, style)),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    /// Un fond posé au milieu d'un mot coloré doit découper ce mot en trois,
    /// et non remplacer sa couleur.
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

    /// Sans coloration dessous, l'occurrence est le seul style de la ligne.
    #[test]
    fn a_mark_on_bare_text_stands_alone() {
        let yellow = gpui::yellow();
        let out = overlay(&[], &[(2..4, yellow)]);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].0, 2..4);
        assert_eq!(out[0].1.background_color, Some(yellow));
    }

    /// L'invariant que gpui ne vérifie pas : trié et disjoint.
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
            assert!(range.start >= previous, "plages désordonnées : {out:?}");
            assert!(range.start < range.end, "plage vide : {out:?}");
            previous = range.end;
        }
        // Le trou entre les deux plages colorées est couvert par l'occurrence,
        // et n'est donc pas perdu.
        assert!(out.iter().any(|(range, _)| range == &(4..8)));
    }

    /// Sans occurrence, rien ne change : c'est le cas de presque toutes les
    /// lignes de presque toutes les frames.
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

    #[test]
    fn recognizes_languages_by_extension_and_by_name() {
        assert_eq!(language_for_path(Path::new("src/main.rs")), Some("rust"));
        assert_eq!(language_for_path(Path::new("app/index.tsx")), Some("tsx"));
        assert_eq!(language_for_path(Path::new("Makefile")), Some("make"));
        assert_eq!(language_for_path(Path::new("Cargo.toml")), Some("toml"));
        // Insensible à la casse de l'extension.
        assert_eq!(language_for_path(Path::new("SCRIPT.SH")), Some("bash"));
        // Inconnu : pas de coloration plutôt qu'une mauvaise.
        assert_eq!(language_for_path(Path::new("data.bin")), None);
        assert_eq!(language_for_path(Path::new("LICENSE")), None);
    }

    #[test]
    fn each_side_keeps_the_lines_that_belong_to_it() {
        let d = diff(vec![
            line(DiffLineKind::Context, "fn a() {}"),
            line(DiffLineKind::Removed, "let x = 1;"),
            line(DiffLineKind::Added, "let y = 2;"),
        ]);

        let (old_text, old_spans) = build_side(&d, Side::Old);
        assert_eq!(old_text, "fn a() {}\nlet x = 1;\n");
        assert_eq!(old_spans.len(), 2);
        assert_eq!(old_spans[1].line, 1, "la ligne supprimée garde son indice");

        let (new_text, new_spans) = build_side(&d, Side::New);
        assert_eq!(new_text, "fn a() {}\nlet y = 2;\n");
        assert_eq!(new_spans[1].line, 2, "la ligne ajoutée garde le sien");
    }

    #[test]
    fn a_removed_line_is_coloured_from_the_old_side() {
        let d = diff(vec![
            line(DiffLineKind::Removed, "let ancien = 1;"),
            line(DiffLineKind::Added, "let nouveau = 2;"),
        ]);
        let highlights =
            DiffHighlights::compute(Path::new("src/x.rs"), &d, &HighlightTheme::default_dark());

        // Les deux lignes doivent être colorées, chacune depuis sa version :
        // c'est tout l'intérêt des deux passes.
        assert!(
            !highlights.line(0, 0).is_empty(),
            "ligne supprimée non colorée"
        );
        assert!(
            !highlights.line(0, 1).is_empty(),
            "ligne ajoutée non colorée"
        );
    }

    #[test]
    fn styles_are_relative_to_their_own_line() {
        let d = diff(vec![
            line(DiffLineKind::Context, "fn premiere() {}"),
            line(DiffLineKind::Context, "fn seconde() {}"),
        ]);
        let highlights =
            DiffHighlights::compute(Path::new("src/x.rs"), &d, &HighlightTheme::default_dark());

        // Sans le décalage, les styles de la seconde ligne pointeraient
        // au-delà de son texte et le rendu paniquerait ou décalerait tout.
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

    /// Le cas courant, et celui qui paraissait cassé : un hunk pris au milieu
    /// d'un fichier ne contient pas `<?php`, et sans lui la grammaire PHP lit
    /// tout le fragment comme du texte HTML — pas une couleur.
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
            "un fragment sans balise d'ouverture doit être coloré"
        );
    }

    /// Bout à bout : une vue Blade traverse la grammaire PHP *et* la
    /// surcouche, et ses deux vocabulaires ressortent colorés.
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

        // La ligne d'écho porte à la fois la balise HTML, vue par la
        // grammaire, et les délimitateurs, vus par la surcouche.
        let echo = styles.line(0, 2);
        assert!(echo.len() >= 3, "styles de l'écho : {echo:?}");
        let mut last = 0;
        for (range, _) in echo {
            assert!(range.start >= last, "plages non triées : {echo:?}");
            last = range.end;
        }
        let text = &diff.hunks[0].lines[2].text;
        assert!(
            last <= text.len(),
            "un style déborde de sa ligne : {last} > {}",
            text.len()
        );
    }

    /// Et un fichier PHP ordinaire n'y passe pas : `@` y est un opérateur, pas
    /// une directive.
    #[test]
    fn a_plain_php_file_is_not_treated_as_blade() {
        assert!(!crate::ui::blade::is_blade(Path::new("app/Facture.php")));
        assert!(crate::ui::blade::is_blade(Path::new(
            "resources/views/facture.blade.php"
        )));
    }

    /// Et le prologue ne doit pas casser ce qui marchait : un fragment qui
    /// porte déjà la balise, ou du HTML qui l'attend, n'en reçoit pas un
    /// second.
    #[test]
    fn a_fragment_that_opens_php_keeps_its_own_prologue() {
        assert_eq!(prologue("php", "class A {}"), "<?php\n");
        assert_eq!(prologue("php", "<?php\nclass A {}"), "");
        assert_eq!(prologue("php", "<div>\n<?= $x ?>"), "");
        assert_eq!(prologue("php", "  <?php echo 1;"), "");
        // Les autres langages n'ont rien à préfixer.
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
        // La ligne de la déclaration de classe porte au moins un mot-clé.
        assert!(
            !highlights.line(0, 1).is_empty(),
            "la grammaire PHP n'est pas prise en compte"
        );
    }
}
