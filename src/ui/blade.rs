//! Coloration des vues Blade.
//!
//! Blade n'est pas un langage que tree-sitter connaît : aucune grammaire n'en
//! est publiée, et la grammaire PHP ne voit dans `@foreach` ou `{{ $x }}` que
//! du texte HTML. Une vue Blade arrivait donc dans la revue avec ses balises
//! colorées et tout son propre vocabulaire en gris.
//!
//! La parade est une surcouche : la grammaire PHP colore ce qu'elle sait lire
//! — HTML, attributs, blocs `<?php` —, puis ce module repasse dessus les
//! constructions de Blade. C'est un scanner à la main, pas un parseur, ce qui
//! est assumé : la syntaxe de Blade tient en trois formes, et un parseur
//! complet coûterait bien plus que ce qu'il rendrait.
//!
//! Ce que la surcouche reconnaît : les directives (`@if`, `@endforeach`, avec
//! leur argument entre parenthèses), les échos (`{{ }}`, `{!! !!}`) et les
//! commentaires (`{{-- --}}`), y compris sur plusieurs lignes.

use std::ops::Range;
use std::path::Path;

use gpui_component::highlighter::HighlightTheme;

use super::highlight::LineStyles;
use crate::git::FileDiff;

/// Vrai pour une vue Blade.
///
/// Le nom complet, pas l'extension : `facture.blade.php` a `php` pour
/// extension, et c'est bien du PHP — mais avec un dialecte en plus.
pub fn is_blade(path: &Path) -> bool {
    path.file_name()
        .and_then(|n| n.to_str())
        .is_some_and(|n| n.to_ascii_lowercase().ends_with(".blade.php"))
}

/// Repasse les constructions Blade par-dessus les styles de la grammaire PHP.
pub fn overlay(diff: &FileDiff, theme: &HighlightTheme, styles: &mut [Vec<LineStyles>]) {
    for (h, hunk) in diff.hunks.iter().enumerate() {
        // L'état des commentaires repart de zéro à chaque hunk : ce qui les
        // sépare a été élidé, et rien ne dit qu'un `{{--` resté ouvert plus
        // haut n'a pas été refermé dans le trou.
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

/// Remplace dans `target` ce que la surcouche recouvre.
///
/// Les styles de la grammaire qui touchent une plage Blade sont retirés plutôt
/// que superposés : le rendu attend des plages triées et disjointes, et un
/// mot-clé à moitié recouvert ne veut rien dire de toute façon.
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

/// Découpe une ligne en plages Blade, chacune avec le nom de style qu'elle
/// mérite. Les plages rendues sont triées et disjointes.
///
/// `open_comment` porte le seul état qui traverse les lignes : un `{{--` non
/// refermé.
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
            // `@@if` est la façon d'écrire un `@if` littéral : ce n'est pas
            // une directive, et le signaler comme telle serait faux.
            i += 2;
        } else if rest.starts_with('@') && starts_a_directive(line, i) {
            i += directive(rest, &mut out, i);
        } else {
            i += next_char(rest);
        }
    }
    out
}

/// Ce qu'une plage Blade est, indépendamment du nom que le thème donne à sa
/// couleur.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Scope {
    Comment,
    /// `@if`, `@endforeach` : le vocabulaire de Blade lui-même.
    Directive,
    /// Ce qui distingue `{{ $x }}` du texte autour.
    Delimiter,
    /// Ce que Blade fait évaluer par PHP — l'intérieur d'un écho, l'argument
    /// d'une directive.
    Expression,
    /// Le nom d'une balise de composant : `<x-forms.input>`, `<livewire:…>`.
    Component,
}

impl Scope {
    /// Noms de style à essayer, du plus juste au plus sûrement présent.
    ///
    /// Un thème n'a pas à définir toute la nomenclature, et les nôtres n'ont
    /// ni `punctuation` ni `operator` : sans repli, les délimiteurs d'un écho
    /// restaient de la couleur du texte, c'est-à-dire invisibles.
    fn candidates(self) -> &'static [&'static str] {
        match self {
            Scope::Comment => &["comment"],
            Scope::Directive => &["keyword"],
            Scope::Delimiter => &["punctuation.special", "tag"],
            Scope::Expression => &["embedded", "variable"],
            // La couleur d'une balise, et non une couleur à eux : un
            // composant *est* une balise pour qui lit la vue, et lui en
            // donner une autre ferait croire à une construction différente.
            Scope::Component => &["tag", "keyword"],
        }
    }

    fn style(self, theme: &HighlightTheme) -> Option<gpui::HighlightStyle> {
        self.candidates().iter().find_map(|name| theme.style(name))
    }
}

/// Le nom d'une balise de composant : `<x-forms.input>`, `</x-layout.app>`,
/// `<livewire:compteur>`. Rend la longueur consommée, délimiteurs compris.
///
/// **Le point est la raison d'être de ce cas.** La grammaire HTML ne connaît
/// pas de nom de balise pointé : dans `<x-layout.app>` elle lit `x-layout`
/// comme une balise et `.app` comme un **attribut**, si bien que le nom du
/// composant se coupe en deux couleurs en son milieu. Or les composants d'un
/// projet Laravel vivent en sous-dossiers, et le point y est donc la règle
/// plutôt que l'exception.
///
/// Le nom entier est repeint d'un seul tenant, ce qui recouvre au passage la
/// lecture fautive de la grammaire — `apply` retire ce qui chevauche.
fn component(rest: &str, out: &mut Vec<(Range<usize>, Scope)>, at: usize) -> Option<usize> {
    let after_bracket = rest.strip_prefix('<')?;
    let (closing, name) = match after_bracket.strip_prefix('/') {
        Some(name) => (1, name),
        None => (0, after_bracket),
    };
    // Les deux préfixes que Laravel se réserve. Tout le reste est du HTML
    // ordinaire, que la grammaire lit très bien elle-même.
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

/// Un écho `{{ … }}` ou `{!! … !!}`. Rend la longueur consommée.
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
        // Un écho qui ne se referme pas sur sa ligne : le reste lui appartient
        // quand même, et la ligne suivante repartira du texte ordinaire.
        None => {
            if !body.is_empty() {
                out.push((at + open.len()..at + rest.len(), Scope::Expression));
            }
            Some(rest.len())
        }
    }
}

/// Une directive `@nom` et, s'il y en a un, son argument entre parenthèses.
/// Rend la longueur consommée — au moins 1, pour que le scanner avance.
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
    // Blade tolère une espace avant la parenthèse (`@if ($x)`), mais pas un
    // saut de ligne : chercher plus loin attraperait un texte qui n'a rien à
    // voir.
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
        // Parenthèse non refermée sur la ligne : on laisse le reste au texte
        // ordinaire plutôt que de deviner.
        None => i,
    }
}

/// Longueur de `(…)`, parenthèses comprises, en tenant compte de
/// l'imbrication et des chaînes — `@if ($x == ')')` n'est pas rare.
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
                // Ne peut pas arriver depuis `directive`, qui n'appelle
                // qu'avec une parenthèse ouvrante ; rendre `None` plutôt que
                // de soustraire à zéro garde la fonction sûre si elle sert
                // ailleurs.
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

/// Vrai si le `@` en `at` ouvre une directive.
///
/// Ce qui le précède tranche : une adresse électronique dans le corps de la
/// page (`contact@exemple.fr`) et un `@media` de feuille de style ont la même
/// forme, seul le caractère d'avant les distingue.
fn starts_a_directive(line: &str, at: usize) -> bool {
    let before = line[..at].chars().next_back();
    match before {
        None => true,
        Some(c) => !(c.is_alphanumeric() || c == '_' || c == '.' || c == '-' || c == '@'),
    }
}

/// Longueur du prochain caractère : avancer d'un octet couperait un caractère
/// accentué en deux, et les plages rendues ne seraient plus des frontières
/// valides.
fn next_char(rest: &str) -> usize {
    rest.chars().next().map_or(1, char::len_utf8)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scan_line(line: &str) -> Vec<(Range<usize>, Scope)> {
        scan(line, &mut false)
    }

    /// Rend ce qu'une plage recouvre, pour lire les tests sans compter les
    /// octets à la main.
    fn covered<'a>(line: &'a str, found: &[(Range<usize>, Scope)]) -> Vec<(&'a str, Scope)> {
        found
            .iter()
            .map(|(r, scope)| (&line[r.clone()], *scope))
            .collect()
    }

    #[test]
    fn a_directive_and_its_argument_are_recognized() {
        let line = "    @foreach ($factures as $facture)";
        assert_eq!(
            covered(line, &scan_line(line)),
            [
                ("@foreach", Scope::Directive),
                ("$factures as $facture", Scope::Expression),
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
        let line = "<td>{{ $facture->total }}</td>";
        assert_eq!(
            covered(line, &scan_line(line)),
            [
                ("{{", Scope::Delimiter),
                (" $facture->total ", Scope::Expression),
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
            "sans quoi l'écho simple mordrait sur l'écho non échappé, et \
             décalerait tout le reste de la ligne"
        );
    }

    #[test]
    fn a_comment_spans_the_lines_it_needs() {
        let mut open = false;
        let first = "{{-- une explication";
        assert_eq!(
            covered(first, &scan(first, &mut open)),
            [(first, Scope::Comment)]
        );
        assert!(open, "le commentaire reste ouvert");

        let middle = "   qui continue";
        assert_eq!(scan(middle, &mut open).len(), 1);
        assert!(open);

        let last = "  --}} <p>suite</p>";
        let found = scan(last, &mut open);
        assert_eq!(&last[found[0].0.clone()], "  --}}");
        assert!(!open, "et se referme");
    }

    #[test]
    fn a_comment_that_closes_on_its_line_leaves_the_rest_alone() {
        let line = "{{-- caché --}} @if ($x)";
        let found = scan_line(line);
        assert_eq!(
            covered(line, &found),
            [
                ("{{-- caché --}}", Scope::Comment),
                ("@if", Scope::Directive),
                ("$x", Scope::Expression),
            ]
        );
    }

    /// Le piège du scanner naïf : tout ce qui ressemble à `@mot` n'est pas une
    /// directive.
    #[test]
    fn what_looks_like_a_directive_but_is_not() {
        let line = "<a href=\"mailto:contact@exemple.fr\">contact@exemple.fr</a>";
        assert!(
            scan_line(line).is_empty(),
            "une adresse électronique n'est pas une directive"
        );

        let escaped = "@@if n'est pas une directive";
        assert!(scan_line(escaped).is_empty());

        let alone = "@ tout seul";
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
        let line = "<p>Été {{ $n }} — @if ($ok) oui @endif</p>";
        let found = scan_line(line);
        let mut last = 0;
        for (range, _) in &found {
            assert!(range.start >= last, "plages non triées : {found:?}");
            assert!(range.start < range.end);
            assert!(
                line.is_char_boundary(range.start) && line.is_char_boundary(range.end),
                "plage {range:?} au milieu d'un caractère"
            );
            last = range.end;
        }
        assert!(found.len() >= 5);
    }

    /// Une plage sans style est une plage invisible : le rôle rendrait la
    /// bonne couleur, mais le thème ne connaîtrait pas le nom demandé.
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
                    "{scope:?} sans couleur dans « {} » : essayés {:?}",
                    theme.name,
                    scope.candidates()
                );
            }
        }
    }

    /// Le cas que la grammaire HTML ne sait pas lire : un nom de composant
    /// pointé, qu'elle coupe en une balise et un attribut.
    #[test]
    fn a_dotted_component_name_is_one_range() {
        let mut open = false;
        let line = "<x-layout.app title=\"Devis\">";
        let found = scan(line, &mut open);
        let component: Vec<_> = found
            .iter()
            .filter(|(_, scope)| *scope == Scope::Component)
            .collect();
        assert_eq!(component.len(), 1, "{found:?}");
        assert_eq!(&line[component[0].0.clone()], "x-layout.app");
        // L'attribut qui suit n'est pas à nous : la grammaire le colore bien.
        assert!(!line[component[0].0.clone()].contains("title"));
    }

    #[test]
    fn a_closing_component_and_livewire_count_too() {
        let mut open = false;
        let line = "</x-forms.input><livewire:compteur :n=\"$n\" />";
        let found: Vec<_> = scan(line, &mut open)
            .into_iter()
            .filter(|(_, scope)| *scope == Scope::Component)
            .map(|(range, _)| line[range].to_string())
            .collect();
        assert_eq!(found, vec!["x-forms.input", "livewire:compteur"]);
    }

    /// Une balise ordinaire appartient à la grammaire, qui la lit très bien :
    /// la surcouche n'a pas à y toucher, sous peine de recouvrir des styles
    /// plus fins que les siens.
    #[test]
    fn an_ordinary_tag_is_left_to_the_grammar() {
        let mut open = false;
        for line in ["<div class=\"x\">", "</section>", "<xml-ish>", "a < b"] {
            let found = scan(line, &mut open);
            assert!(
                !found.iter().any(|(_, scope)| *scope == Scope::Component),
                "{line} : {found:?}"
            );
        }
    }

    #[test]
    fn recognizes_a_view_by_its_full_name() {
        assert!(is_blade(Path::new("resources/views/facture.blade.php")));
        assert!(is_blade(Path::new("layout.Blade.PHP")));
        assert!(!is_blade(Path::new("app/Models/Facture.php")));
        assert!(!is_blade(Path::new("blade.php")));
    }
}
