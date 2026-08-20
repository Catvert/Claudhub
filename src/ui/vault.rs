//! Les notes de relecture sur le disque, en Markdown.
//!
//! Une note est du texte qu'on écrit à propos d'un bout de code : c'est une
//! note au sens d'Obsidian, et la ranger dans un JSON d'état revenait à
//! l'enfermer là où rien ne sait la lire. Un **fichier par note**, dans un
//! dossier qu'on choisit — celui d'un coffre, si l'on en tient un — et le
//! suivi de relecture dans un index à côté.
//!
//! Ce module ne fait **aucune entrée-sortie** : il rend du texte et le relit.
//! Les fichiers sont écrits et lus par un worker (`Cmd::ReadNotes`,
//! `Cmd::WriteNotes`), parce qu'un coffre peut vivre sur un disque lent — un
//! montage drvfs de WSL, un dossier synchronisé — et qu'une lecture de
//! répertoire dans le thread d'interface s'y paierait en fenêtre figée.
//!
//! ## Ce que le format doit tenir
//!
//! Le dossier est la **source de vérité** : ce qu'on corrige dans Obsidian
//! revient dans Claudhub au prochain chargement du worktree. Le format doit
//! donc être relu, et pas seulement écrit — d'où le frontmatter, plat et sans
//! surprise, et l'extrait cité dans un bloc de code dont on choisit la
//! clôture.
//!
//! On ne réécrit **que ce qui porte notre marque** (`claudhub:` en tête du
//! frontmatter) : un dossier de coffre contient les notes de son propriétaire,
//! et rien de ce que nous n'avons pas écrit ne doit disparaître parce qu'une
//! note a changé de nom.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use crate::git::DiffRange;
use crate::ui::notes::{Note, Side};

/// Un fichier relu, et le volume qu'il avait au moment où on l'a coché.
///
/// Le volume est ce qui **périme** la coche : un agent qui réécrit un fichier
/// annule sa relecture, faute de quoi la case dirait « relu » d'un contenu que
/// personne n'a lu. C'est le même garde que l'empreinte repassée à l'écriture
/// d'un fichier, et la même approximation assumée qu'ailleurs : une
/// modification qui laisse `+n −m` inchangé passe au travers.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Reviewed {
    pub range: DiffRange,
    pub path: PathBuf,
    pub added: usize,
    pub removed: usize,
}

/// Une tâche de `TODO.md` : une case à cocher Markdown, et sa ligne.
///
/// La ligne est retenue parce que c'est **elle** qu'on modifie : cocher ne
/// réécrit pas le fichier, il retourne un caractère à un endroit connu. Tout
/// ce qu'il y a autour — le texte d'un agent, ses sous-listes, ses liens —
/// survit intact, ce qu'un rendu à partir de nos seules structures ne
/// garantirait jamais.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Task {
    pub done: bool,
    pub label: String,
    /// Indice de la ligne dans le fichier, à partir de zéro.
    pub line: usize,
    /// Profondeur d'imbrication, en niveaux de deux espaces.
    pub depth: usize,
}

/// `TODO.md` tel qu'il est sur le disque, et les tâches qu'on y a reconnues.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Todo {
    pub text: String,
    pub tasks: Vec<Task>,
}

impl Todo {
    pub fn done(&self) -> usize {
        self.tasks.iter().filter(|task| task.done).count()
    }
}

/// Les cases à cocher d'un `TODO.md`.
///
/// Tout le reste est ignoré et **conservé** : un titre, un paragraphe, une
/// liste ordinaire. On ne lit pas un format à nous, on repère des cases dans
/// du Markdown que quelqu'un d'autre écrit.
pub fn parse_todo(text: &str) -> Todo {
    let mut tasks = Vec::new();
    for (line, raw) in text.lines().enumerate() {
        let indent = raw.len() - raw.trim_start().len();
        let trimmed = raw.trim_start();
        let Some(rest) = trimmed
            .strip_prefix("- ")
            .or_else(|| trimmed.strip_prefix("* "))
            .or_else(|| trimmed.strip_prefix("+ "))
        else {
            continue;
        };
        let done = match rest.get(..3) {
            Some("[ ]") => false,
            Some("[x]") | Some("[X]") => true,
            _ => continue,
        };
        tasks.push(Task {
            done,
            label: rest[3..].trim().to_string(),
            line,
            depth: indent / 2,
        });
    }
    Todo {
        text: text.to_string(),
        tasks,
    }
}

/// Coche ou décoche la tâche d'une ligne, et rend le fichier entier.
///
/// `None` quand la ligne n'est plus une case à cocher : le fichier a changé
/// sous nos pieds — un agent écrit dedans pendant qu'on le regarde — et
/// écrire au jugé retournerait la mauvaise case.
pub fn toggle_task(text: &str, line: usize, done: bool) -> Option<String> {
    let raw = text.lines().nth(line)?;
    let at = ["[ ]", "[x]", "[X]"]
        .iter()
        .filter_map(|box_| raw.find(box_))
        .min()?;
    let replaced = format!(
        "{}{}{}",
        &raw[..at],
        if done { "[x]" } else { "[ ]" },
        &raw[at + 3..]
    );
    let mut out = String::with_capacity(text.len() + 1);
    for (i, current) in text.lines().enumerate() {
        if i > 0 {
            out.push('\n');
        }
        out.push_str(if i == line { &replaced } else { current });
    }
    if text.ends_with('\n') {
        out.push('\n');
    }
    Some(out)
}

/// Le `TODO.md` qu'on pose quand il n'y en a pas.
///
/// Il explique son propre format : le fichier finit dans un coffre, ouvert par
/// quelqu'un — ou par un agent — qui n'a pas lu notre documentation.
pub fn seed_todo(worktree: &Path) -> String {
    format!(
        "---\nclaudhub: todo\nworktree: {}\n---\n\n# À faire\n\n\
         Une tâche est une case à cocher Markdown — `- [ ] …` — et rien d'autre \
         n'est interprété : le texte autour est à qui l'écrit. Claudhub affiche \
         ces cases dans son panneau de notes, et l'agent de ce worktree tient la \
         liste à jour ; le fichier lui est donné par `$CLAUDHUB_TODO`.\n\n",
        scalar(&worktree.display().to_string())
    )
}

/// Le nom du fichier d'index, dans le dossier d'un worktree.
pub const INDEX: &str = "Relecture.md";

/// La liste de tâches d'un worktree, dans le même dossier.
///
/// Elle n'est **pas** écrite par Claudhub au fil de l'eau, contrairement aux
/// notes et à l'index : elle appartient à qui la tient — l'agent qui coche ce
/// qu'il vient de faire, ou soi-même dans Obsidian. Claudhub la lit, l'affiche,
/// et ne touche qu'à la case qu'on clique. C'est ce qui permet à un agent d'y
/// écrire ce qu'il veut — des sous-listes, des liens, du texte entre les
/// tâches — sans que la prochaine écriture de note l'efface.
pub const TODO: &str = "TODO.md";

/// Le dossier d'un worktree : `<racine>/<dépôt>/<worktree>`.
///
/// Deux niveaux et non un chemin aplati : un coffre se parcourt à la main, et
/// `Acetics/fix-login` s'y lit là où
/// `home-finch-Projects-Acetics-fix-login` ne se lit pas. Deux dépôts de même
/// nom se retrouveraient au même endroit ; c'est le prix de la lisibilité, et
/// les notes y resteraient distinguées par leur `worktree`.
pub fn dir_for(root: &Path, repo: &Path, worktree: &Path) -> PathBuf {
    root.join(leaf(repo)).join(leaf(worktree))
}

fn leaf(path: &Path) -> String {
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let name = sanitize(&name);
    if name.is_empty() {
        "sans-nom".into()
    } else {
        name
    }
}

/// Ce qu'un nom de fichier ne peut pas porter.
///
/// La liste est celle de Windows, la plus stricte des trois : un coffre se
/// synchronise, et un nom qui passe ici doit passer là-bas.
fn sanitize(text: &str) -> String {
    text.chars()
        .map(|c| match c {
            '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|' | '#' | '^' | '[' | ']' => '-',
            c if (c as u32) < 0x20 => '-',
            c => c,
        })
        .collect::<String>()
        .trim()
        .trim_matches('.')
        .to_string()
}

/// Le nom du fichier d'une note.
///
/// Fonction pure de son identifiant et de son fichier, et de rien d'autre :
/// une note qui glisse de dix lignes ne doit pas changer de nom, sinon chaque
/// écriture casserait les liens que le coffre porte vers elle. L'identifiant
/// est en tête et rembourré, pour que l'ordre alphabétique du dossier soit
/// l'ordre où les notes ont été prises.
pub fn note_file(note: &Note) -> String {
    let stem = note
        .path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| "note".into());
    format!("{:04} {}.md", note.id, sanitize(&stem))
}

/// Une note en Markdown : les propriétés d'Obsidian, le code cité, la remarque.
pub fn render_note(note: &Note) -> String {
    let mut front = BTreeMap::new();
    front.insert("claudhub", "note".to_string());
    front.insert("id", note.id.to_string());
    front.insert("file", note.path.display().to_string());
    front.insert(
        "side",
        match note.side {
            Side::Old => "old".into(),
            Side::New => "new".to_string(),
        },
    );
    front.insert("lines", format!("{}-{}", note.start, note.end));
    match &note.range {
        DiffRange::Working => {
            front.insert("range", "working".into());
        }
        DiffRange::Branch { base } => {
            front.insert("range", "branch".into());
            front.insert("base", base.clone());
        }
        DiffRange::Commit { id, parent } => {
            front.insert("range", "commit".into());
            front.insert("commit", id.clone());
            if let Some(parent) = parent {
                front.insert("parent", parent.clone());
            }
        }
    }
    front.insert("sent", note.sent.to_string());
    front.insert("done", note.done.to_string());

    let mut out = String::from("---\n");
    for (key, value) in &front {
        out.push_str(&format!("{key}: {}\n", scalar(value)));
    }
    out.push_str("---\n\n");
    out.push_str(&format!("# {}\n\n", note.location()));
    let fence = fence_for(&note.excerpt);
    out.push_str(&format!(
        "{fence}{}\n{}\n{fence}\n\n",
        language(&note.path),
        note.excerpt.trim_end_matches('\n')
    ));
    out.push_str(note.body.trim_end());
    out.push('\n');
    out
}

/// Relit une note écrite par `render_note`, ou celle qu'on a retouchée.
///
/// Rend `None` sur tout ce qui ne porte pas notre marque : un dossier de
/// coffre contient d'autres notes, et les avaler comme des remarques de
/// relecture en ferait disparaître à la première écriture.
pub fn parse_note(text: &str) -> Option<Note> {
    let (front, body) = front_matter(text)?;
    if front.get("claudhub").map(String::as_str) != Some("note") {
        return None;
    }
    let (start, end) = lines_of(front.get("lines")?)?;
    let range = match front.get("range").map(String::as_str) {
        Some("branch") => DiffRange::Branch {
            base: front.get("base")?.clone(),
        },
        Some("commit") => DiffRange::Commit {
            id: front.get("commit")?.clone(),
            parent: front.get("parent").cloned(),
        },
        _ => DiffRange::Working,
    };
    let (excerpt, remark) = split_excerpt(body);
    Some(Note {
        id: front.get("id").and_then(|v| v.parse().ok()).unwrap_or(0),
        range,
        path: PathBuf::from(front.get("file")?),
        side: match front.get("side").map(String::as_str) {
            Some("old") => Side::Old,
            _ => Side::New,
        },
        start,
        end,
        excerpt,
        body: remark,
        sent: front.get("sent").map(String::as_str) == Some("true"),
        done: front.get("done").map(String::as_str) == Some("true"),
    })
}

/// L'index de relecture : les fichiers cochés, en cases à cocher Markdown.
///
/// Des cases et non une liste : Obsidian les rend cliquables, et décocher là
/// rend le fichier à relire ici. Seuls les fichiers **cochés** y figurent —
/// l'autre liste n'a pas de bord, une revue de branche touchant couramment
/// plusieurs centaines de fichiers.
///
/// Le titre d'une section est la clé du domaine, pas son libellé traduit :
/// changer la langue de l'interface ne doit pas rendre illisible ce qu'on a
/// déjà écrit.
pub fn render_index(worktree: &Path, reviewed: &[Reviewed]) -> String {
    let mut out = String::from("---\nclaudhub: review\n");
    out.push_str(&format!(
        "worktree: {}\n",
        scalar(&worktree.display().to_string())
    ));
    out.push_str("---\n\n# Relecture\n\n");
    out.push_str(
        "Les fichiers relus, cochés depuis Claudhub. Décocher une case ici les rend à relire.\n",
    );

    let mut sorted: Vec<&Reviewed> = reviewed.iter().collect();
    sorted.sort_by(|a, b| {
        range_key(&a.range)
            .cmp(&range_key(&b.range))
            .then(a.path.cmp(&b.path))
    });

    let mut current = String::new();
    for item in sorted {
        let key = range_key(&item.range);
        if key != current {
            out.push_str(&format!("\n## {key}\n\n"));
            current = key;
        }
        out.push_str(&format!(
            "- [x] {} +{} −{}\n",
            item.path.display(),
            item.added,
            item.removed
        ));
    }
    out
}

/// Relit l'index. Une case décochée est une relecture qu'on annule.
pub fn parse_index(text: &str) -> Vec<Reviewed> {
    let body = front_matter(text).map(|(_, body)| body).unwrap_or(text);
    let mut out = Vec::new();
    let mut range = None;
    for line in body.lines() {
        let line = line.trim();
        if let Some(rest) = line.strip_prefix("## ") {
            range = range_of(rest.trim());
            continue;
        }
        let Some(range) = range.clone() else { continue };
        let Some(rest) = line.strip_prefix("- [x] ").or(line.strip_prefix("- [X] ")) else {
            continue;
        };
        if let Some(item) = entry_of(rest, range) {
            out.push(item);
        }
    }
    out
}

/// `chemin +12 −3`. Le volume est en fin de ligne, un chemin pouvant contenir
/// une espace — on découpe donc par la droite.
fn entry_of(text: &str, range: DiffRange) -> Option<Reviewed> {
    let mut parts: Vec<&str> = text.trim().rsplitn(3, ' ').collect();
    parts.reverse();
    let [path, added, removed] = parts.as_slice() else {
        return None;
    };
    Some(Reviewed {
        range,
        path: PathBuf::from(path.trim()),
        added: added.trim_start_matches('+').parse().ok()?,
        removed: removed.trim_start_matches(['−', '-']).parse().ok()?,
    })
}

fn range_key(range: &DiffRange) -> String {
    match range {
        DiffRange::Working => "working".into(),
        DiffRange::Branch { base } => format!("branch {base}"),
        DiffRange::Commit { id, .. } => format!("commit {id}"),
    }
}

fn range_of(key: &str) -> Option<DiffRange> {
    let (kind, rest) = key.split_once(' ').unwrap_or((key, ""));
    match kind {
        "working" => Some(DiffRange::Working),
        "branch" if !rest.is_empty() => Some(DiffRange::Branch {
            base: rest.to_string(),
        }),
        "commit" if !rest.is_empty() => Some(DiffRange::Commit {
            id: rest.to_string(),
            parent: None,
        }),
        _ => None,
    }
}

/// Le frontmatter, plat, et ce qui le suit.
///
/// Un sous-ensemble de YAML assumé comme tel : nos clés sont des scalaires —
/// des nombres, des booléens, des chemins — et embarquer un analyseur complet
/// pour six lignes coûterait une dépendance de plus que ce qu'on écrit.
fn front_matter(text: &str) -> Option<(BTreeMap<String, String>, &str)> {
    let rest = text.strip_prefix("---\n")?;
    let end = rest.find("\n---")?;
    let (head, tail) = rest.split_at(end);
    let mut map = BTreeMap::new();
    for line in head.lines() {
        if let Some((key, value)) = line.split_once(':') {
            map.insert(key.trim().to_string(), unscalar(value.trim()));
        }
    }
    let tail = tail.trim_start_matches('\n');
    let tail = tail.strip_prefix("---").unwrap_or(tail);
    Some((map, tail.trim_start_matches('\n')))
}

/// L'extrait cité, et la remarque qui le suit.
fn split_excerpt(body: &str) -> (String, String) {
    let mut lines = body.lines().peekable();
    let mut before = Vec::new();
    let mut fence = None;
    for line in lines.by_ref() {
        let trimmed = line.trim_start();
        if trimmed.starts_with("```") {
            fence = Some(
                trimmed
                    .chars()
                    .take_while(|c| *c == '`')
                    .collect::<String>(),
            );
            break;
        }
        before.push(line);
    }
    let Some(fence) = fence else {
        return (String::new(), body.trim().to_string());
    };
    let mut excerpt = Vec::new();
    for line in lines.by_ref() {
        if line.trim_start().starts_with(&fence) {
            break;
        }
        excerpt.push(line);
    }
    let remark: Vec<&str> = lines.collect();
    (excerpt.join("\n"), remark.join("\n").trim().to_string())
}

/// Une clôture plus longue que la plus longue suite d'accents graves du texte.
///
/// Un diff de Markdown en contient, et une clôture à trois y refermerait le
/// bloc au milieu de l'extrait.
fn fence_for(text: &str) -> String {
    let mut longest = 0;
    let mut current = 0;
    for c in text.chars() {
        if c == '`' {
            current += 1;
            longest = longest.max(current);
        } else {
            current = 0;
        }
    }
    "`".repeat(longest.max(2) + 1)
}

/// Le langage du bloc de code, pour qu'Obsidian le colore.
fn language(path: &Path) -> &'static str {
    crate::ui::highlight::language_for_path(path).unwrap_or("")
}

/// Une valeur de frontmatter qui ne se laisse pas relire de travers.
///
/// Un chemin qui commence par `[` ou qui contient `: ` serait lu par Obsidian
/// comme une liste ou comme une clé imbriquée. Les guillemets règlent les deux.
fn scalar(value: &str) -> String {
    let plain = !value.is_empty()
        && !value.contains(": ")
        && !value.starts_with([
            '[', '{', '"', '\'', '&', '*', '!', '|', '>', '%', '@', '`', '#',
        ])
        && !value.ends_with(':');
    if plain {
        value.to_string()
    } else {
        format!("\"{}\"", value.replace('\\', "\\\\").replace('"', "\\\""))
    }
}

fn unscalar(value: &str) -> String {
    let value = value.trim();
    if value.len() >= 2 && value.starts_with('"') && value.ends_with('"') {
        value[1..value.len() - 1]
            .replace("\\\"", "\"")
            .replace("\\\\", "\\")
    } else {
        value.to_string()
    }
}

fn lines_of(text: &str) -> Option<(usize, usize)> {
    match text.split_once('-') {
        Some((start, end)) => Some((start.trim().parse().ok()?, end.trim().parse().ok()?)),
        None => {
            let only = text.trim().parse().ok()?;
            Some((only, only))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn note() -> Note {
        Note {
            id: 3,
            range: DiffRange::Branch {
                base: "master".into(),
            },
            path: PathBuf::from("app/Http/Kernel.php"),
            side: Side::New,
            start: 42,
            end: 44,
            excerpt: "public function handle()\n{\n    return null;\n}".into(),
            body: "Le retour nul remonte jusqu'au contrôleur.".into(),
            sent: true,
            done: false,
        }
    }

    #[test]
    fn a_note_survives_the_round_trip() {
        let note = note();
        let text = render_note(&note);
        assert_eq!(parse_note(&text), Some(note));
    }

    /// Le nom ne porte que l'identifiant et le fichier : une note qui glisse
    /// de dix lignes garderait sinon un nom différent à chaque écriture, et
    /// les liens du coffre pointeraient dans le vide.
    #[test]
    fn the_file_name_ignores_the_lines() {
        let mut note = note();
        let before = note_file(&note);
        note.start += 10;
        note.end += 10;
        assert_eq!(note_file(&note), before);
        assert_eq!(before, "0003 Kernel.php.md");
    }

    /// Ce que nous n'avons pas écrit ne nous appartient pas.
    #[test]
    fn a_foreign_note_is_not_ours() {
        assert!(parse_note("---\ntags: [idée]\n---\n\nUne note à moi.").is_none());
        assert!(parse_note("Pas de frontmatter du tout.").is_none());
    }

    /// Un extrait de Markdown contient des accents graves, et une clôture à
    /// trois refermerait le bloc au milieu.
    #[test]
    fn an_excerpt_full_of_backticks_stays_whole() {
        let mut note = note();
        note.excerpt = "Voir ```rust\nlet x = 1;\n```".into();
        let text = render_note(&note);
        assert_eq!(parse_note(&text).unwrap().excerpt, note.excerpt);
    }

    #[test]
    fn a_remark_written_in_obsidian_comes_back() {
        let text = render_note(&note());
        let edited = text.replace(
            "Le retour nul remonte jusqu'au contrôleur.",
            "Corrigé dans le coffre.\n\n- et sur deux lignes",
        );
        let parsed = parse_note(&edited).unwrap();
        assert_eq!(
            parsed.body,
            "Corrigé dans le coffre.\n\n- et sur deux lignes"
        );
        assert_eq!(parsed.excerpt, note().excerpt);
    }

    #[test]
    fn the_index_survives_the_round_trip() {
        let reviewed = vec![
            Reviewed {
                range: DiffRange::Branch {
                    base: "master".into(),
                },
                path: PathBuf::from("app/Http/Kernel.php"),
                added: 12,
                removed: 3,
            },
            Reviewed {
                range: DiffRange::Working,
                path: PathBuf::from("un fichier avec une espace.rs"),
                added: 0,
                removed: 7,
            },
        ];
        let text = render_index(Path::new("/tmp/wt"), &reviewed);
        let mut back = parse_index(&text);
        back.sort_by(|a, b| a.path.cmp(&b.path));
        let mut want = reviewed;
        want.sort_by(|a, b| a.path.cmp(&b.path));
        assert_eq!(back, want);
    }

    /// Décocher dans Obsidian rend le fichier à relire.
    #[test]
    fn an_unchecked_box_is_no_longer_reviewed() {
        let text = "## working\n- [ ] a.rs +1 −0\n- [x] b.rs +2 −0\n";
        let back = parse_index(text);
        assert_eq!(back.len(), 1);
        assert_eq!(back[0].path, PathBuf::from("b.rs"));
    }

    /// Sans section, une ligne cochée ne désigne aucun domaine : la prendre
    /// pour des modifications en cours ferait apparaître une coche là où
    /// personne ne l'a mise.
    #[test]
    fn a_line_without_a_section_is_ignored() {
        assert!(parse_index("- [x] a.rs +1 −0\n").is_empty());
    }

    /// Ce qui entoure les cases appartient à qui l'écrit, et le rendu ne
    /// passe pas par nos structures : cocher retourne un caractère, tout le
    /// reste du fichier est recopié tel quel.
    #[test]
    fn checking_a_box_leaves_the_rest_of_the_file_alone() {
        let text = "# À faire\n\nUn paragraphe d'agent.\n\n- [ ] lire le diff\n  - [x] sous-tâche\n- [ ] écrire le test\n";
        let todo = parse_todo(text);
        assert_eq!(todo.tasks.len(), 3);
        assert_eq!(todo.done(), 1);
        assert_eq!(todo.tasks[0].label, "lire le diff");
        assert_eq!(todo.tasks[1].depth, 1);

        let after = toggle_task(text, todo.tasks[0].line, true).expect("la case existe");
        assert_eq!(
            after,
            text.replace("- [ ] lire le diff", "- [x] lire le diff")
        );
        assert_eq!(parse_todo(&after).done(), 2);
    }

    /// Une ligne qui n'est plus une case veut dire que le fichier a changé
    /// sous nos pieds : mieux vaut ne rien écrire que retourner la mauvaise.
    #[test]
    fn a_line_that_is_no_longer_a_box_refuses_the_toggle() {
        assert!(toggle_task("- [ ] une tâche\ndu texte\n", 1, true).is_none());
        assert!(toggle_task("- [ ] une tâche\n", 9, true).is_none());
    }

    /// Le fichier qu'on pose doit se relire : c'est le même contrat que les
    /// notes, et il n'y a personne pour le vérifier à notre place.
    #[test]
    fn the_seeded_list_reads_back_as_an_empty_one() {
        let todo = parse_todo(&seed_todo(Path::new("/tmp/wt")));
        assert!(todo.tasks.is_empty());
        assert!(todo.text.contains("claudhub: todo"));
    }

    #[test]
    fn a_path_with_colons_stays_readable() {
        let mut note = note();
        note.path = PathBuf::from("app/a: b.php");
        let text = render_note(&note);
        assert_eq!(parse_note(&text).unwrap().path, note.path);
    }
}
