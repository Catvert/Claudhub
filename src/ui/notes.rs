//! Les notes de relecture : ce qu'on a à dire sur un bout de code, et de quoi
//! le renvoyer à l'agent qui l'a écrit.
//!
//! ## Pourquoi une note ne s'accroche pas à la sélection
//!
//! `ReviewState::diff_selection` est un couple d'indices dans la liste
//! **affichée**. Il est invalidé par la bascule unifié/deux colonnes, par un
//! changement de contexte, et par tout rechargement du diff — c'est-à-dire par
//! chaque écriture de fichier dans le worktree. Une note qui s'y accrocherait
//! pointerait sur autre chose quelques secondes plus tard.
//!
//! On retient donc des **numéros de ligne** — que git donne déjà — et
//! **l'extrait de code** lui-même. Les numéros replacent la note dans le cas
//! courant ; l'extrait la rattrape quand le fichier a bougé sous elle. Et si
//! les deux échouent, la note est dite *décalée* et **reste dans la liste** :
//! une note perdue en silence est pire que pas de note du tout.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::git::{DiffLineKind, DiffRange};
use crate::ui::diff_view::{Rendered, Row};

/// De quelle version du fichier une note parle.
///
/// Commenter du code supprimé a un sens — « pourquoi avoir enlevé ça ? » est
/// une remarque de relecture aussi légitime qu'une autre — et une ligne
/// supprimée n'a pas de numéro dans la nouvelle version.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Side {
    Old,
    New,
}

/// Une remarque prise sur une plage de lignes.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Note {
    pub id: u64,
    /// Le domaine où la note a été prise. Une remarque sur ce que la branche a
    /// écrit ne se relit pas dans les modifications en cours.
    pub range: DiffRange,
    pub path: PathBuf,
    pub side: Side,
    /// Numéros de ligne, **jamais** des indices de liste : ceux-là ne
    /// survivent pas au rechargement du diff.
    pub start: usize,
    pub end: usize,
    /// Le code cité, tel que `Rendered::copy_text` le rend — sans `+`/`-`,
    /// sans numéros, sans en-tête `@@`. C'est ce qui permet de retrouver la
    /// note quand les numéros ont bougé, et ce qu'on cite dans le prompt.
    pub excerpt: String,
    /// La remarque elle-même.
    pub body: String,
    /// Envoyée à l'agent. Ce n'est pas la même chose que traitée : c'est la
    /// relecture de la réponse qui clôt une note.
    pub sent: bool,
    pub done: bool,
}

impl Default for Note {
    fn default() -> Self {
        Self {
            id: 0,
            range: DiffRange::Working,
            path: PathBuf::new(),
            side: Side::New,
            start: 0,
            end: 0,
            excerpt: String::new(),
            body: String::new(),
            sent: false,
            done: false,
        }
    }
}

impl Note {
    /// `chemin:début-fin`, la forme que tout le monde sait lire — et qu'un
    /// agent sait ouvrir.
    pub fn location(&self) -> String {
        if self.start == self.end {
            format!("{}:{}", self.path.display(), self.start)
        } else {
            format!("{}:{}-{}", self.path.display(), self.start, self.end)
        }
    }
}

/// Où une note se replace dans le diff affiché.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Anchor {
    /// Aux numéros retenus, et le texte concorde : le cas courant.
    At { from: usize, to: usize },
    /// Le fichier a bougé, mais l'extrait s'y retrouve ailleurs.
    Moved { from: usize, to: usize },
    /// Ni les numéros ni l'extrait : la note reste, marquée décalée.
    Drifted,
}

impl Anchor {
    pub fn rows(self) -> Option<(usize, usize)> {
        match self {
            Self::At { from, to } | Self::Moved { from, to } => Some((from, to)),
            Self::Drifted => None,
        }
    }
}

/// Replace une note dans un diff qui vient d'arriver.
///
/// Les indices rendus sont ceux de la liste **unifiée** (`Rendered::rows`),
/// qui seule porte l'ordre du fichier ; la vue en deux colonnes s'y ramène
/// comme elle le fait déjà pour la copie.
pub fn relocate(rendered: &Rendered, note: &Note) -> Anchor {
    if let Some((from, to)) = by_numbers(rendered, note) {
        if copied(rendered, from, to) == note.excerpt {
            return Anchor::At { from, to };
        }
    }
    match by_excerpt(rendered, note) {
        Some((from, to)) => Anchor::Moved { from, to },
        None => Anchor::Drifted,
    }
}

/// La plage de lignes qui porte les numéros retenus, du côté retenu.
fn by_numbers(rendered: &Rendered, note: &Note) -> Option<(usize, usize)> {
    let mut bounds: Option<(usize, usize)> = None;
    for (index, row) in rendered.rows.iter().enumerate() {
        let Row::Line { hunk, line } = *row else {
            continue;
        };
        let source = rendered.file.hunks.get(hunk)?.lines.get(line)?;
        let number = match note.side {
            Side::Old => source.old_no,
            Side::New => source.new_no,
        };
        // Une ligne sans numéro de ce côté-là — un ajout vu depuis l'ancienne
        // version — n'appartient pas à la plage : elle n'existe pas dans le
        // fichier dont la note parle.
        let Some(number) = number else { continue };
        if number < note.start || number > note.end {
            continue;
        }
        bounds = Some(match bounds {
            Some((a, b)) => (a.min(index), b.max(index)),
            None => (index, index),
        });
    }
    bounds
}

/// Cherche l'extrait, ligne à ligne, dans tout le diff.
///
/// Une comparaison de lignes entières et non de sous-chaînes : c'est ce que la
/// note a cité, et une correspondance partielle replacerait la remarque au
/// milieu d'une ligne où elle ne veut rien dire.
fn by_excerpt(rendered: &Rendered, note: &Note) -> Option<(usize, usize)> {
    let needle: Vec<&str> = note.excerpt.lines().collect();
    if needle.is_empty() {
        return None;
    }
    // Les entrées telles que la copie les rend : ni en-têtes, ni annotation
    // « \ No newline ». L'indice de la ligne dans la liste unifiée est gardé à
    // côté, seul moyen de revenir de l'une à l'autre.
    let hay: Vec<(usize, &str)> = rendered
        .rows
        .iter()
        .enumerate()
        .filter_map(|(index, row)| match *row {
            Row::Line { hunk, line } => {
                let source = rendered.file.hunks.get(hunk)?.lines.get(line)?;
                (source.kind != DiffLineKind::NoNewline).then_some((index, source.text.as_str()))
            }
            Row::Header { .. } => None,
        })
        .collect();
    if needle.len() > hay.len() {
        return None;
    }
    for start in 0..=hay.len() - needle.len() {
        if hay[start..start + needle.len()]
            .iter()
            .zip(&needle)
            .all(|((_, text), wanted)| text == wanted)
        {
            return Some((hay[start].0, hay[start + needle.len() - 1].0));
        }
    }
    None
}

fn copied(rendered: &Rendered, from: usize, to: usize) -> String {
    rendered.copy_text(from, to, false)
}

/// Les lignes annotées d'un diff, une case par entrée de liste.
///
/// Deux vecteurs et non un seul : la liste unifiée et la liste en deux
/// colonnes ne comptent pas les mêmes entrées, et la vue en change d'un
/// raccourci. Les calculer tous les deux à l'arrivée du diff coûte deux
/// parcours, contre un test par ligne visible et par frame si on le faisait
/// dans la fermeture de rendu.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Marks {
    pub unified: Vec<bool>,
    pub split: Vec<bool>,
}

impl Marks {
    pub fn at(&self, index: usize, split: bool) -> bool {
        let marks = if split { &self.split } else { &self.unified };
        marks.get(index).copied().unwrap_or(false)
    }
}

/// Marque les lignes que des notes recouvrent.
pub fn marks(rendered: &Rendered, spans: &[(usize, usize)]) -> Marks {
    let mut unified = vec![false; rendered.rows.len()];
    for (from, to) in spans {
        for mark in unified
            .get_mut(*from..=(*to).min(rendered.rows.len().saturating_sub(1)))
            .unwrap_or_default()
        {
            *mark = true;
        }
    }
    // La vue en colonnes se déduit de l'unifiée : une entrée y est annotée dès
    // que l'une des lignes qu'elle recouvre l'est.
    let split = rendered
        .split
        .iter()
        .map(|row| row.unified().any(|index| unified[index]))
        .collect();
    Marks { unified, split }
}

/// Ce qu'une sélection donne comme note.
///
/// Les numéros sont ceux du **premier et du dernier** de la plage qui en
/// portent un du côté choisi. Un bloc entièrement fait de lignes de l'autre
/// version n'en a aucun : la note s'ancre alors sur le côté opposé, qui est le
/// seul à pouvoir la porter.
pub fn anchor_selection(
    rendered: &Rendered,
    from: usize,
    to: usize,
) -> Option<(Side, usize, usize)> {
    let (from, to) = (from.min(to), from.max(to));
    let mut old: Option<(usize, usize)> = None;
    let mut new: Option<(usize, usize)> = None;
    for index in from..=to {
        let Some(Row::Line { hunk, line }) = rendered.rows.get(index).copied() else {
            continue;
        };
        let Some(source) = rendered
            .file
            .hunks
            .get(hunk)
            .and_then(|hunk| hunk.lines.get(line))
        else {
            continue;
        };
        for (slot, number) in [(&mut old, source.old_no), (&mut new, source.new_no)] {
            let Some(number) = number else { continue };
            *slot = Some(match *slot {
                Some((a, b)) => (a.min(number), b.max(number)),
                None => (number, number),
            });
        }
    }
    // La nouvelle version d'abord : c'est celle qu'on relit, et une remarque
    // porte presque toujours sur le code qui restera.
    match (new, old) {
        (Some((start, end)), _) => Some((Side::New, start, end)),
        (None, Some((start, end))) => Some((Side::Old, start, end)),
        (None, None) => None,
    }
}

/// Le message livré à l'agent.
///
/// C'est la pièce à verrouiller — le reste de l'envoi n'est que plomberie. Le
/// format est celui qu'un agent lit sans instruction supplémentaire : un titre
/// par emplacement, le code cité dans une clôture, la remarque en citation.
pub fn prompt(branch: &str, notes: &[Note]) -> String {
    let mut out = String::new();
    out.push_str(&crate::tr!("notes-prompt-intro", { branch: branch }));
    out.push_str("\n\n");
    for note in notes {
        out.push_str("## ");
        out.push_str(&note.location());
        out.push('\n');
        push_fence(&mut out, &note.excerpt, fence_language(&note.path));
        for line in note.body.lines() {
            out.push_str("> ");
            out.push_str(line);
            out.push('\n');
        }
        out.push('\n');
    }
    // Le dernier saut de ligne n'apporte rien et se voit dans l'invite d'un
    // agent, qui l'affiche tel quel.
    while out.ends_with('\n') {
        out.pop();
    }
    out
}

/// Une question libre posée sur un bout de code, sans passer par une note.
///
/// C'est le geste le plus fréquent en pratique : on relit, quelque chose
/// intrigue, on demande — sans avoir de remarque à consigner.
pub fn ask(location: &str, path: &std::path::Path, excerpt: &str, question: &str) -> String {
    let mut out = String::new();
    out.push_str(&crate::tr!("notes-prompt-ask", { location: location }));
    out.push_str("\n\n");
    push_fence(&mut out, excerpt, fence_language(path));
    out.push_str(question.trim());
    out
}

/// Une clôture de code, dont le nombre de tildes dépasse la plus longue suite
/// que l'extrait contient.
///
/// Des accents graves suffiraient presque toujours ; presque, parce qu'un
/// extrait de Markdown — ou ce fichier-ci — en contient, et refermerait la
/// clôture au milieu du code cité.
fn push_fence(out: &mut String, excerpt: &str, language: &str) {
    let longest = excerpt.split(|c| c != '`').map(str::len).max().unwrap_or(0);
    let fence = "`".repeat(longest.max(2) + 1);
    out.push_str(&fence);
    out.push_str(language);
    out.push('\n');
    out.push_str(excerpt);
    if !excerpt.ends_with('\n') {
        out.push('\n');
    }
    out.push_str(&fence);
    out.push('\n');
}

/// Le nom de langage d'une clôture Markdown.
///
/// La même table que la coloration, à un détail près : ce qu'elle ne connaît
/// pas donne une clôture nue, et non une absence de clôture.
fn fence_language(path: &std::path::Path) -> &'static str {
    crate::ui::highlight::language_for_path(path).unwrap_or("")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::git::{DiffLine, FileDiff, Hunk};

    fn line(kind: DiffLineKind, old: Option<usize>, new: Option<usize>, text: &str) -> DiffLine {
        DiffLine {
            kind,
            old_no: old,
            new_no: new,
            text: text.into(),
        }
    }

    /// Un diff minuscule mais complet : du contexte, une suppression, un
    /// ajout, et l'annotation de fin de fichier.
    fn sample() -> Rendered {
        let hunk = Hunk {
            header: "@@ -1,3 +1,3 @@".into(),
            old_start: 1,
            new_start: 1,
            lines: vec![
                line(DiffLineKind::Context, Some(1), Some(1), "fn main() {"),
                line(DiffLineKind::Removed, Some(2), None, "    old();"),
                line(DiffLineKind::Added, None, Some(2), "    new();"),
                line(DiffLineKind::Context, Some(3), Some(3), "}"),
                line(
                    DiffLineKind::NoNewline,
                    None,
                    None,
                    "No newline at end of file",
                ),
            ],
        };
        let file = FileDiff {
            hunks: vec![hunk],
            binary: false,
            empty: false,
        };
        Rendered::new(
            std::path::Path::new("src/main.rs"),
            file,
            &gpui_component::highlighter::HighlightTheme::default_light(),
        )
    }

    fn note_of(rendered: &Rendered, from: usize, to: usize) -> Note {
        let (side, start, end) = anchor_selection(rendered, from, to).expect("une ancre");
        Note {
            id: 1,
            path: "src/main.rs".into(),
            side,
            start,
            end,
            excerpt: rendered.copy_text(from, to, false),
            body: "à revoir".into(),
            ..Default::default()
        }
    }

    #[test]
    fn a_note_returns_to_the_rows_it_was_taken_on() {
        let rendered = sample();
        // Les lignes 2 et 3 de la liste : la suppression et l'ajout.
        let note = note_of(&rendered, 2, 3);
        assert_eq!(note.side, Side::New);
        assert_eq!((note.start, note.end), (2, 2));
        // L'aller-retour rend exactement la plage qui porte ce numéro-là — la
        // ligne supprimée n'a pas de numéro dans la nouvelle version.
        assert_eq!(relocate(&rendered, &note), Anchor::Moved { from: 2, to: 3 });
    }

    #[test]
    fn a_context_range_anchors_on_its_own_numbers() {
        let rendered = sample();
        let note = note_of(&rendered, 1, 1);
        assert_eq!((note.side, note.start, note.end), (Side::New, 1, 1));
        assert_eq!(relocate(&rendered, &note), Anchor::At { from: 1, to: 1 });
    }

    #[test]
    fn a_removed_line_anchors_on_the_old_side() {
        let rendered = sample();
        // La seule suppression : elle n'a pas de numéro dans la nouvelle
        // version, donc la note ne peut s'ancrer que sur l'ancienne.
        let note = note_of(&rendered, 2, 2);
        assert_eq!((note.side, note.start, note.end), (Side::Old, 2, 2));
        assert_eq!(relocate(&rendered, &note), Anchor::At { from: 2, to: 2 });
    }

    #[test]
    fn the_no_newline_marker_is_never_part_of_an_excerpt() {
        let rendered = sample();
        // La sélection va jusqu'au bout, annotation comprise : elle ne doit
        // pas se retrouver dans le code cité, sinon la note ne se replace
        // jamais — cette ligne n'est pas dans le fichier.
        let note = note_of(&rendered, 4, 5);
        assert!(!note.excerpt.contains("No newline"));
        assert_eq!(relocate(&rendered, &note), Anchor::At { from: 4, to: 4 });
    }

    #[test]
    fn a_note_follows_its_excerpt_when_the_file_has_moved() {
        let rendered = sample();
        let mut note = note_of(&rendered, 3, 3);
        // Vingt lignes ajoutées plus haut : les numéros ne valent plus rien,
        // mais le code cité, lui, est toujours là.
        note.start += 20;
        note.end += 20;
        assert_eq!(relocate(&rendered, &note), Anchor::Moved { from: 3, to: 3 });
    }

    #[test]
    fn a_note_whose_code_is_gone_stays_and_says_so() {
        let rendered = sample();
        let mut note = note_of(&rendered, 3, 3);
        note.excerpt = "    disparu();\n".into();
        note.start = 99;
        note.end = 99;
        // Décalée, et non supprimée : une note perdue en silence est pire que
        // pas de note du tout.
        assert_eq!(relocate(&rendered, &note), Anchor::Drifted);
    }

    #[test]
    fn the_marks_of_the_two_layouts_agree() {
        let rendered = sample();
        let note = note_of(&rendered, 2, 3);
        let span = relocate(&rendered, &note).rows().expect("replacée");
        let marks = marks(&rendered, &[span]);
        // Une case par entrée de chaque liste : la fermeture de rendu indexe
        // sans vérifier, et une longueur trop courte ferait disparaître les
        // marqueurs du bas du fichier.
        assert_eq!(marks.unified.len(), rendered.rows.len());
        assert_eq!(marks.split.len(), rendered.split.len());
        assert!(marks.at(2, false) && marks.at(3, false));
        assert!(!marks.at(1, false));
        // Appariées, la suppression et l'ajout tiennent sur une seule entrée
        // de la vue en colonnes : elle est annotée elle aussi.
        let annotated_pairs = marks.split.iter().filter(|mark| **mark).count();
        assert_eq!(annotated_pairs, 1);
    }

    #[test]
    fn the_prompt_quotes_the_code_and_the_remark() {
        let notes = vec![Note {
            id: 1,
            path: "src/ui/app.rs".into(),
            side: Side::New,
            start: 120,
            end: 121,
            excerpt: "let x = 1;\nlet y = 2;\n".into(),
            body: "deux lignes\npour rien".into(),
            ..Default::default()
        }];
        let text = prompt("agent/fix", &notes);
        assert!(text.contains("agent/fix"), "{text}");
        assert!(text.contains("## src/ui/app.rs:120-121"), "{text}");
        assert!(
            text.contains("```rust\nlet x = 1;\nlet y = 2;\n```"),
            "{text}"
        );
        assert!(text.contains("> deux lignes\n> pour rien"), "{text}");
        // Le prompt part dans une invite d'agent, qui affiche les blancs de
        // fin tels quels.
        assert!(!text.ends_with('\n'), "{text:?}");
    }

    #[test]
    fn a_fence_survives_an_excerpt_that_contains_one() {
        // Un extrait de Markdown — ou de ce dépôt-ci — contient des accents
        // graves ; une clôture de trois refermerait le bloc au milieu du code.
        let notes = vec![Note {
            excerpt: "voir ```rust``` plus haut\n".into(),
            body: "hm".into(),
            path: "README.md".into(),
            ..Default::default()
        }];
        let text = prompt("main", &notes);
        assert!(text.contains("````"), "{text}");
    }
}
