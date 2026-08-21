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
    let at = box_at(raw)?;
    let replaced = format!(
        "{}{}{}",
        &raw[..at],
        if done { "[x]" } else { "[ ]" },
        &raw[at + 3..]
    );
    Some(replace_line(text, line, &replaced))
}

/// The `TODO.md` we lay down when there is none.
///
/// It explains its own format: the file ends up in a vault, opened by somebody —
/// or by an agent — who has not read our documentation.
pub fn seed_todo(worktree: &Path) -> String {
    format!(
        "---\nclaudhub: todo\nworktree: {}\n---\n\n# To do\n\n\
         A task is a Markdown checkbox — `- [ ] …` — and nothing else is \
         interpreted: the text around it belongs to whoever writes it. Claudhub \
         shows these boxes in its notes panel, and this worktree's agent keeps \
         the list up to date; the file is given to it through `$CLAUDHUB_TODO`.\n\n",
        scalar(&worktree.display().to_string())
    )
}

/// The index file's name, in a worktree's folder.
pub const INDEX: &str = "Review.md";

/// The name the index carried before the interface was written in English.
///
/// Read but never written: a vault holds files somebody may have linked to, and
/// silently dropping the ticks of an existing review to rename a file would cost
/// more than carrying one constant. `sync_notes` erases it once the new one has
/// been written, the list it aligns the folder on no longer containing it.
pub const LEGACY_INDEX: &str = "Relecture.md";

/// A worktree's task list, in the same folder.
///
/// It is **not** written by Claudhub as it goes, unlike the notes and the index:
/// it belongs to whoever keeps it — the agent ticking off what it has just done,
/// or yourself in Obsidian. Claudhub reads it, shows it, and touches only the
/// box you click. That is what lets an agent write whatever it likes in it —
/// sub-lists, links, text between tasks — without the next note write erasing it.
pub const TODO: &str = "TODO.md";

/// A worktree's free note: what you write *beside* the code, and which is about
/// no particular line.
///
/// It has **no frontmatter**: it is ordinary Markdown, the kind you would keep
/// in your vault anyway, and a vault note does not have to carry our keys to be
/// readable. The name is enough to find it, and its lack of a mark also puts it
/// out of the purge's reach.
///
/// Empty, it **does not exist**: an empty file and an absent file cannot be told
/// apart in a vault, and leaving a shell per opened worktree is exactly what
/// `notes_on_disk` avoids elsewhere.
pub const NOTES: &str = "NOTES.md";

/// Appends a task to a `TODO.md`, after the last one it carries.
///
/// After the last task and not at the end of the file: an agent writes under its
/// list — what it has understood, what is left to decide — and a task added
/// after that prose would no longer read as part of the list.
pub fn append_task(text: &str, label: &str) -> String {
    let label = label.trim();
    let entry = format!("- [ ] {label}");
    let tasks = parse_todo(text).tasks;
    let mut lines: Vec<String> = text.lines().map(str::to_string).collect();
    match tasks.last() {
        Some(last) => lines.insert(last.line + 1, entry),
        None => {
            // Une ligne vide avant la première tâche : elle suit de la prose,
            // et Markdown ne commence pas une liste collée à un paragraphe.
            if lines.last().is_some_and(|line| !line.trim().is_empty()) {
                lines.push(String::new());
            }
            lines.push(entry);
        }
    }
    let mut out = lines.join("\n");
    out.push('\n');
    out
}

/// Réécrit le libellé d'une tâche, en laissant sa case et son indentation.
///
/// `None` si la ligne n'est plus une case à cocher, comme pour `toggle_task` :
/// le fichier a changé sous nos pieds, et réécrire au jugé emporterait la
/// mauvaise ligne.
pub fn set_task_label(text: &str, line: usize, label: &str) -> Option<String> {
    let raw = text.lines().nth(line)?;
    let at = box_at(raw)?;
    Some(replace_line(
        text,
        line,
        &format!("{}{} {}", &raw[..at], &raw[at..at + 3], label.trim()),
    ))
}

/// Retire la tâche d'une ligne.
pub fn remove_task(text: &str, line: usize) -> Option<String> {
    let raw = text.lines().nth(line)?;
    box_at(raw)?;
    let kept: Vec<&str> = text
        .lines()
        .enumerate()
        .filter(|(i, _)| *i != line)
        .map(|(_, l)| l)
        .collect();
    let mut out = kept.join("\n");
    if !out.is_empty() || text.ends_with('\n') {
        out.push('\n');
    }
    Some(out)
}

/// La position de la case à cocher d'une ligne, s'il y en a une.
fn box_at(raw: &str) -> Option<usize> {
    let trimmed = raw.trim_start();
    if !(trimmed.starts_with("- ") || trimmed.starts_with("* ") || trimmed.starts_with("+ ")) {
        return None;
    }
    ["[ ]", "[x]", "[X]"]
        .iter()
        .filter_map(|mark| raw.find(mark))
        .min()
}

/// Remplace une ligne, en gardant le reste du fichier au caractère près.
fn replace_line(text: &str, line: usize, replacement: &str) -> String {
    let mut out = String::with_capacity(text.len() + replacement.len());
    for (i, current) in text.lines().enumerate() {
        if i > 0 {
            out.push('\n');
        }
        out.push_str(if i == line { replacement } else { current });
    }
    if text.ends_with('\n') {
        out.push('\n');
    }
    out
}

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

/// The review index: the ticked files, as Markdown checkboxes.
///
/// Checkboxes and not a list: Obsidian makes them clickable, and unticking there
/// hands the file back to be reviewed here. Only the **ticked** files appear —
/// the other list has no edge, a branch review routinely touching several
/// hundred files.
///
/// A section's title is the range's key, not its translated label: changing the
/// interface language must not make what has already been written unreadable.
pub fn render_index(worktree: &Path, reviewed: &[Reviewed]) -> String {
    let mut out = String::from("---\nclaudhub: review\n");
    out.push_str(&format!(
        "worktree: {}\n",
        scalar(&worktree.display().to_string())
    ));
    out.push_str("---\n\n# Review\n\n");
    out.push_str(
        "The files reviewed, ticked from Claudhub. Unticking a box here hands them back to be reviewed.\n",
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
            body: "The null return travels all the way to the controller.".into(),
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

    /// What we did not write does not belong to us.
    #[test]
    fn a_foreign_note_is_not_ours() {
        assert!(parse_note("---\ntags: [idea]\n---\n\nA note of my own.").is_none());
        assert!(parse_note("No frontmatter at all.").is_none());
    }

    /// A Markdown excerpt contains backticks, and a fence of three would close
    /// the block in the middle.
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
            "The null return travels all the way to the controller.",
            "Fixed in the vault.\n\n- and on two lines",
        );
        let parsed = parse_note(&edited).unwrap();
        assert_eq!(parsed.body, "Fixed in the vault.\n\n- and on two lines");
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

    /// With no section, a ticked line names no range: taking it for changes in
    /// progress would show a tick where nobody put one.
    #[test]
    fn a_line_without_a_section_is_ignored() {
        assert!(parse_index("- [x] a.rs +1 −0\n").is_empty());
    }

    /// What surrounds the boxes belongs to whoever writes it, and the rendering
    /// does not go through our structures: ticking flips one character, all the
    /// rest of the file is copied as it is.
    #[test]
    fn checking_a_box_leaves_the_rest_of_the_file_alone() {
        let text = "# To do\n\nAn agent's paragraph.\n\n- [ ] read the diff\n  - [x] subtask\n- [ ] write the test\n";
        let todo = parse_todo(text);
        assert_eq!(todo.tasks.len(), 3);
        assert_eq!(todo.done(), 1);
        assert_eq!(todo.tasks[0].label, "read the diff");
        assert_eq!(todo.tasks[1].depth, 1);

        let after = toggle_task(text, todo.tasks[0].line, true).expect("the box exists");
        assert_eq!(
            after,
            text.replace("- [ ] read the diff", "- [x] read the diff")
        );
        assert_eq!(parse_todo(&after).done(), 2);
    }

    /// A line that is no longer a box means the file has changed under our feet:
    /// better to write nothing than to flip the wrong one.
    #[test]
    fn a_line_that_is_no_longer_a_box_refuses_the_toggle() {
        assert!(toggle_task("- [ ] a task\nsome text\n", 1, true).is_none());
        assert!(toggle_task("- [ ] a task\n", 9, true).is_none());
    }

    /// A task joins the list, not what the agent wrote after it.
    #[test]
    fn a_task_lands_after_the_last_one() {
        let text = "# To do\n\n- [x] read\n- [ ] write\n\nWhat is left to decide.\n";
        let after = append_task(text, "  re-read  ");
        assert_eq!(
            after,
            "# To do\n\n- [x] read\n- [ ] write\n- [ ] re-read\n\nWhat is left to decide.\n"
        );
        assert_eq!(parse_todo(&after).tasks.len(), 3);
    }

    /// With no task, the list begins — but not glued to the paragraph before it,
    /// which Markdown would not read as a list.
    #[test]
    fn the_first_task_opens_the_list() {
        let after = append_task(&seed_todo(Path::new("/tmp/wt")), "read the diff");
        assert_eq!(parse_todo(&after).tasks.len(), 1);
        assert!(after.ends_with("\n- [ ] read the diff\n"));
    }

    /// Editing a label leaves the box, the indentation and the rest of the file
    /// where they are.
    #[test]
    fn a_label_is_rewritten_in_place() {
        let text = "- [x] read\n  - [ ] subtask\nSome text.\n";
        let after = set_task_label(text, 1, "  re-read the diff  ").expect("a box");
        assert_eq!(after, "- [x] read\n  - [ ] re-read the diff\nSome text.\n");

        let after = remove_task(text, 0).expect("a box");
        assert_eq!(after, "  - [ ] subtask\nSome text.\n");

        assert!(set_task_label(text, 2, "x").is_none());
        assert!(remove_task(text, 2).is_none());
    }

    /// The file we lay down has to read back: it is the same contract as the
    /// notes', and there is nobody to check it for us.
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
