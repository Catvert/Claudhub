//! Lire, retoucher et ranger les fichiers d'un worktree.
//!
//! Tout ce qui touche au disque hors de git : la lecture d'un fichier pour
//! l'éditer, son écriture, les renommages et les suppressions, et le lancement
//! de l'éditeur externe. Comme la couche git, rien ici ne doit être appelé
//! depuis le thread d'interface.
//!
//! **L'écriture est conditionnelle.** Un agent écrit dans les mêmes fichiers
//! pendant qu'on les lit : `expect` porte l'empreinte de ce qu'on avait sous
//! les yeux, et l'écriture est refusée si le fichier a changé depuis. C'est la
//! seule façon de ne pas effacer une heure de travail d'un agent avec une
//! correction de faute de frappe.

use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};

/// Au-delà, l'éditeur intégré n'est pas une bonne idée : `InputState` documente
/// cinquante mille lignes, et une fenêtre figée est un pire service qu'un
/// refus.
pub const MAX_LINES: usize = 50_000;

/// Ce qu'on a lu, et de quoi vérifier qu'on écrit bien par-dessus.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Content {
    pub text: String,
    /// Empreinte du texte lu.
    ///
    /// Elle ne sert qu'à comparer deux lectures dans la même session : c'est
    /// exactement la garantie qu'on veut — « ce fichier a-t-il changé depuis
    /// que je l'ai ouvert ? » — et elle n'a pas à survivre au redémarrage.
    pub hash: u64,
}

/// Empreinte d'un texte, pour la détection d'écriture concurrente.
///
/// FNV-1a écrit à la main, et non `DefaultHasher` : l'empreinte est produite
/// par le worker et comparée par la vue, qui seront deux **binaires** quand le
/// worker tournera dans le serveur WSL — or `DefaultHasher` ne promet rien
/// d'un processus à l'autre. FNV-1a est défini par ses deux constantes, et un
/// test fige une valeur connue pour qu'aucun changement ne passe inaperçu.
pub fn digest(text: &str) -> u64 {
    const OFFSET: u64 = 0xcbf2_9ce4_8422_2325;
    const PRIME: u64 = 0x0000_0100_0000_01b3;
    let mut hash = OFFSET;
    for byte in text.bytes() {
        hash ^= u64::from(byte);
        hash = hash.wrapping_mul(PRIME);
    }
    hash
}

/// True if this content is not text.
///
/// The null byte is git's own criterion, and on the first block: an executable
/// has one in its first bytes, a text never does.
pub fn looks_binary(bytes: &[u8]) -> bool {
    bytes.iter().take(8000).any(|byte| *byte == 0)
}

/// Reads a file from the worktree for editing.
pub fn read(worktree: &Path, path: &Path) -> Result<Content> {
    let full = worktree.join(path);
    let bytes = std::fs::read(&full).with_context(|| format!("cannot read {}", full.display()))?;
    if looks_binary(&bytes) {
        bail!("{} is a binary file", path.display());
    }
    let text = String::from_utf8(bytes)
        .map_err(|_| anyhow::anyhow!("{} is not UTF-8 text", path.display()))?;
    // Counted before returning: it is the only chance to refuse before the
    // window has started colouring five hundred thousand lines.
    if text.lines().count() > MAX_LINES {
        bail!(
            "{} is over {MAX_LINES} lines: open it in your editor",
            path.display()
        );
    }
    let hash = digest(&text);
    Ok(Content { text, hash })
}

/// Writes a file, unless somebody else has changed it since.
///
/// `expect` is the digest of what we had read. A vanished file counts as a
/// change: recreating it through a save would bring back what a `git restore`
/// had just removed.
pub fn write(worktree: &Path, path: &Path, text: &str, expect: Option<u64>) -> Result<()> {
    write_at(&worktree.join(path), text, expect)
}

/// The same conditional write, on an absolute path.
///
/// What is written outside the worktree has no relative path to give it: a
/// vault's `TODO.md` lives elsewhere, and the digest guard serves it just as
/// well — it is the agent writing in it while we watch.
pub fn write_at(full: &Path, text: &str, expect: Option<u64>) -> Result<()> {
    if let Some(expected) = expect {
        let current = std::fs::read_to_string(full).map(|text| digest(&text)).ok();
        if current != Some(expected) {
            bail!(
                "{} has changed since it was opened: reload it before saving",
                full.display()
            );
        }
    }
    if let Some(parent) = full.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(full, text).with_context(|| format!("cannot write {}", full.display()))
}

/// What we do to a file from the explorer.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum Op {
    Rename { from: PathBuf, to: PathBuf },
    Delete { path: PathBuf },
    NewFile { path: PathBuf },
    NewDir { path: PathBuf },
}

impl Op {
    /// Le chemin sur lequel l'opération porte, pour le message de résultat.
    pub fn target(&self) -> &Path {
        match self {
            Self::Rename { to, .. } => to,
            Self::Delete { path } | Self::NewFile { path } | Self::NewDir { path } => path,
        }
    }
}

/// Runs an explorer operation.
///
/// Paths are **brought back inside the worktree**: a `../` typed in a rename
/// dialog would otherwise leave the repository, and a deletion there would do
/// damage git does not recover.
pub fn apply(worktree: &Path, op: &Op) -> Result<()> {
    match op {
        Op::Rename { from, to } => {
            let (from, to) = (inside(worktree, from)?, inside(worktree, to)?);
            if to.exists() {
                bail!("{} already exists", to.display());
            }
            if let Some(parent) = to.parent() {
                std::fs::create_dir_all(parent)?;
            }
            std::fs::rename(&from, &to).with_context(|| format!("cannot rename {}", from.display()))
        }
        Op::Delete { path } => {
            let full = inside(worktree, path)?;
            let result = if full.is_dir() {
                std::fs::remove_dir_all(&full)
            } else {
                std::fs::remove_file(&full)
            };
            result.with_context(|| format!("cannot delete {}", full.display()))
        }
        Op::NewFile { path } => {
            let full = inside(worktree, path)?;
            if full.exists() {
                bail!("{} already exists", path.display());
            }
            if let Some(parent) = full.parent() {
                std::fs::create_dir_all(parent)?;
            }
            std::fs::write(&full, "").with_context(|| format!("cannot create {}", full.display()))
        }
        Op::NewDir { path } => {
            let full = inside(worktree, path)?;
            std::fs::create_dir_all(&full)
                .with_context(|| format!("cannot create {}", full.display()))
        }
    }
}

/// Resolves a relative path inside the worktree, refusing to leave it.
fn inside(worktree: &Path, path: &Path) -> Result<PathBuf> {
    if path.is_absolute() {
        bail!("{}: an absolute path is not accepted here", path.display());
    }
    if path
        .components()
        .any(|c| matches!(c, std::path::Component::ParentDir))
    {
        bail!("{}: cannot leave the worktree", path.display());
    }
    Ok(worktree.join(path))
}

/// Splits an external editor's command, substituting the file and the line.
///
/// `{path}` and `{line}` are replaced everywhere they appear, including in the
/// middle of an argument — `code -g {path}:{line}` and `zed {path}:{line}` are
/// written that way. Without `{path}`, the path is appended at the end: that is
/// what a bare `vim` expects.
pub fn editor_command(template: &str, path: &Path, line: usize) -> Option<(String, Vec<String>)> {
    let template = template.trim();
    if template.is_empty() {
        return None;
    }
    let path = path.display().to_string();
    let mut parts: Vec<String> = crate::cmdline::split_command(template)
        .into_iter()
        .map(|part| {
            part.replace("{path}", &path)
                .replace("{line}", &line.to_string())
        })
        .collect();
    if !template.contains("{path}") {
        parts.push(path);
    }
    let mut parts = parts.into_iter();
    let program = parts.next()?;
    Some((program, parts.collect()))
}

/// Launches the external editor and returns immediately.
///
/// Without waiting: a graphical editor only returns when it is closed, and a
/// `vim` launched into the void never would. The process is detached, its
/// outputs thrown away — it is a separate program, not a command whose result
/// we read.
pub fn open_external(worktree: &Path, template: &str, path: &Path, line: usize) -> Result<String> {
    let Some((program, args)) = editor_command(template, &worktree.join(path), line) else {
        bail!("no external editor configured");
    };
    std::process::Command::new(&program)
        .args(&args)
        .current_dir(worktree)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .with_context(|| format!("{program} could not be launched"))?;
    Ok(program)
}

// — The notes folder ————————————————————————————————————————————————

/// What `sync_notes` is allowed to erase, by the frontmatter's mark.
///
/// A vault folder contains its owner's notes, and a deleted review note must
/// not take the week's journal with it. The value counts as much as the key:
/// `claudhub: todo` carries our mark but **does not belong to us** — it is the
/// agent, or its reader, that keeps it, and writing a note must not take the
/// running task list away.
const OURS: [&str; 2] = ["\nclaudhub: note", "\nclaudhub: review"];

/// True for a file Claudhub writes whole, and may therefore erase.
fn is_ours(text: &str) -> bool {
    text.starts_with("---") && OURS.iter().any(|mark| text.contains(mark))
}

/// Writes a vault file, or erases it if its text is empty.
///
/// Empty means absent: an empty file in a vault is a shell nobody opens twice,
/// and the free note of a worktree that is emptied should disappear rather than
/// sit in the way of a search.
///
/// The digest guards the erasure as it guards the write: the file may have
/// changed since we read it.
pub fn write_vault_file(path: &Path, text: &str, expect: Option<u64>) -> Result<()> {
    if !text.trim().is_empty() {
        return write_at(path, text, expect);
    }
    match std::fs::read_to_string(path) {
        Ok(current) => {
            if expect.is_some_and(|expected| digest(&current) != expected) {
                bail!(
                    "{} has changed since it was opened: reload it before saving",
                    path.display()
                );
            }
            std::fs::remove_file(path).with_context(|| format!("cannot delete {}", path.display()))
        }
        // Nothing to erase: that is the state we were asking for.
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e).with_context(|| format!("reading {}", path.display())),
    }
}

/// The Markdown files of a notes folder, name and content.
///
/// A missing folder is not an error: it is the state of a worktree that has not
/// been annotated yet.
pub fn read_notes(dir: &Path) -> Result<Vec<(String, String)>> {
    let entries = match std::fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(e).with_context(|| format!("reading {}", dir.display())),
    };
    let mut out = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("md") {
            continue;
        }
        let Ok(text) = std::fs::read_to_string(&path) else {
            continue;
        };
        let Some(name) = path.file_name().map(|n| n.to_string_lossy().into_owned()) else {
            continue;
        };
        out.push((name, text));
    }
    out.sort();
    Ok(out)
}

/// Aligns the folder on this list, and on it alone.
///
/// Three rules, and each is paid for if forgotten:
///
/// - **We do not rewrite what has not changed.** A vault is often synced, and
///   touching a file's date on every click would make the sync work for
///   nothing.
/// - **We only erase what we write whole** (`is_ours`). The folder may contain
///   its owner's notes, and a `TODO.md` the agent keeps up to date.
/// - **What is no longer in the list disappears**, including under another
///   name: that is how a deleted note goes away, and how a file renamed in the
///   vault does not leave a duplicate behind.
pub fn sync_notes(dir: &Path, files: &[(String, String)]) -> Result<()> {
    std::fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
    for (name, content) in files {
        let path = dir.join(name);
        if std::fs::read_to_string(&path).is_ok_and(|old| old == *content) {
            continue;
        }
        std::fs::write(&path, content).with_context(|| format!("writing {}", path.display()))?;
    }
    for (name, text) in read_notes(dir)? {
        if files.iter().any(|(kept, _)| *kept == name) {
            continue;
        }
        if is_ours(&text) {
            let _ = std::fs::remove_file(dir.join(name));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// L'empreinte est comparée entre deux processus — la vue et le serveur —
    /// et doit donc être identique d'un binaire à l'autre. Ces valeurs sont
    /// celles de FNV-1a 64 bits ; si ce test casse, c'est que l'algorithme a
    /// changé, et toute empreinte retenue par une session en cours ment.
    #[test]
    fn the_digest_is_stable_across_binaries() {
        assert_eq!(digest(""), 0xcbf2_9ce4_8422_2325);
        assert_eq!(digest("a"), 0xaf63_dc4c_8601_ec8c);
        assert_eq!(digest("Claudhub\n"), digest("Claudhub\n"));
        assert_ne!(digest("Claudhub\n"), digest("Claudhub"));
    }

    /// La chaîne complète du dossier de notes : ce qu'on écrit se relit, ce
    /// qui n'est plus dans la liste s'en va, et ce que nous n'avons pas écrit
    /// reste. Le seul test de ce module qui touche au disque, comme celui de
    /// la surveillance : c'est la seule façon de prouver l'effacement.
    #[test]
    fn the_notes_folder_keeps_what_is_not_ours() {
        let dir = std::env::temp_dir().join(format!("claudhub-notes-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);

        let ours = (
            "0001 a.rs.md".to_string(),
            "---\nclaudhub: note\n---\n\nx\n".to_string(),
        );
        sync_notes(&dir, std::slice::from_ref(&ours)).expect("write");
        std::fs::write(dir.join("Journal.md"), "---\ntags: [me]\n---\n\nMine.\n").unwrap();
        // Our mark, but not our file: the task list belongs to the agent that
        // ticks it, and a deleted note does not take it away.
        std::fs::write(dir.join("TODO.md"), "---\nclaudhub: todo\n---\n\n- [ ] x\n").unwrap();

        let read = read_notes(&dir).expect("read");
        assert_eq!(read.len(), 3);

        // The note goes away, the journal and the list stay.
        sync_notes(&dir, &[]).expect("erase");
        let read = read_notes(&dir).expect("read");
        let names: Vec<&str> = read.iter().map(|(name, _)| name.as_str()).collect();
        assert_eq!(names, ["Journal.md", "TODO.md"]);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn an_editor_command_places_the_file_and_the_line() {
        let path = Path::new("/p/src/main.rs");
        assert_eq!(
            editor_command("code -g {path}:{line}", path, 42),
            Some(("code".into(), vec!["-g".into(), "/p/src/main.rs:42".into()]))
        );
        assert_eq!(
            editor_command("phpstorm --line {line} {path}", path, 7),
            Some((
                "phpstorm".into(),
                vec!["--line".into(), "7".into(), "/p/src/main.rs".into()]
            ))
        );
        // Sans `{path}`, le chemin va à la fin : c'est ce qu'attend un éditeur
        // qui ne sait rien des lignes.
        assert_eq!(
            editor_command("gedit", path, 1),
            Some(("gedit".into(), vec!["/p/src/main.rs".into()]))
        );
        assert_eq!(editor_command("   ", path, 1), None);
    }

    #[test]
    fn an_editor_path_may_contain_a_space() {
        // The flaw `split_command` fixes, here too.
        assert_eq!(
            editor_command(r#""/opt/my editor/bin/ed" {path}"#, Path::new("/a b.rs"), 1),
            Some(("/opt/my editor/bin/ed".into(), vec!["/a b.rs".into()]))
        );
    }

    #[test]
    fn a_path_cannot_leave_the_worktree() {
        let worktree = Path::new("/p/repo");
        assert!(inside(worktree, Path::new("src/main.rs")).is_ok());
        assert!(inside(worktree, Path::new("../elsewhere")).is_err());
        assert!(inside(worktree, Path::new("/etc/passwd")).is_err());
    }

    #[test]
    fn a_null_byte_gives_away_a_binary() {
        assert!(looks_binary(b"\x7fELF\0\0"));
        assert!(!looks_binary(b"fn main() {}\n"));
        // Au-dela du premier bloc, on ne regarde pas : c'est le critere de git.
        let mut long = vec![b'a'; 9000];
        long.push(0);
        assert!(!looks_binary(&long));
    }

    #[test]
    fn writing_refuses_when_the_file_changed_underneath() {
        let dir = std::env::temp_dir().join(format!("claudhub-files-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = Path::new("note.txt");
        std::fs::write(dir.join(path), "one").unwrap();

        let read_back = read(&dir, path).unwrap();
        assert_eq!(read_back.text, "one");
        // An agent writes while we edit.
        std::fs::write(dir.join(path), "two").unwrap();
        assert!(write(&dir, path, "three", Some(read_back.hash)).is_err());
        // And the file has not moved: that is the whole point.
        assert_eq!(std::fs::read_to_string(dir.join(path)).unwrap(), "two");
        // With no expectation, the write goes through.
        assert!(write(&dir, path, "three", None).is_ok());

        let _ = std::fs::remove_dir_all(&dir);
    }
}
