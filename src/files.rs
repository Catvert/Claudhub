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

/// Vrai si ce contenu n'est pas du texte.
///
/// L'octet nul est le critère de git lui-même, et sur le premier bloc : un
/// exécutable en a un dans ses premiers octets, un texte n'en a jamais.
pub fn looks_binary(bytes: &[u8]) -> bool {
    bytes.iter().take(8000).any(|byte| *byte == 0)
}

/// Lit un fichier du worktree pour l'éditer.
pub fn read(worktree: &Path, path: &Path) -> Result<Content> {
    let full = worktree.join(path);
    let bytes = std::fs::read(&full)
        .with_context(|| format!("lecture de {} impossible", full.display()))?;
    if looks_binary(&bytes) {
        bail!("{} est un fichier binaire", path.display());
    }
    let text = String::from_utf8(bytes)
        .map_err(|_| anyhow::anyhow!("{} n'est pas du texte UTF-8", path.display()))?;
    // Compté avant de rendre : c'est la seule occasion de refuser sans que la
    // fenêtre ait déjà commencé à colorer cinq cent mille lignes.
    if text.lines().count() > MAX_LINES {
        bail!(
            "{} dépasse {MAX_LINES} lignes : ouvrez-le dans votre éditeur",
            path.display()
        );
    }
    let hash = digest(&text);
    Ok(Content { text, hash })
}

/// Écrit un fichier, sauf si quelqu'un d'autre l'a modifié depuis.
///
/// `expect` est l'empreinte de ce qu'on avait lu. Un fichier disparu compte
/// comme un changement : le recréer par une sauvegarde ferait revenir ce qu'un
/// `git restore` venait d'enlever.
pub fn write(worktree: &Path, path: &Path, text: &str, expect: Option<u64>) -> Result<()> {
    write_at(&worktree.join(path), text, expect)
}

/// La même écriture conditionnelle, sur un chemin absolu.
///
/// Ce qu'on écrit hors du worktree n'a pas de chemin relatif à lui donner : le
/// `TODO.md` d'un coffre vit ailleurs, et le garde d'empreinte lui sert autant
/// — c'est l'agent qui écrit dedans pendant qu'on le regarde.
pub fn write_at(full: &Path, text: &str, expect: Option<u64>) -> Result<()> {
    if let Some(expected) = expect {
        let current = std::fs::read_to_string(full).map(|text| digest(&text)).ok();
        if current != Some(expected) {
            bail!(
                "{} a changé depuis son ouverture : rechargez-le avant d'enregistrer",
                full.display()
            );
        }
    }
    if let Some(parent) = full.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(full, text).with_context(|| format!("écriture de {} impossible", full.display()))
}

/// Ce qu'on fait à un fichier depuis l'explorateur.
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

/// Exécute une opération de l'explorateur.
///
/// Les chemins sont **ramenés dans le worktree** : un `../` saisi dans un
/// dialogue de renommage sortirait sinon du dépôt, et une suppression y ferait
/// des dégâts que git ne rattrape pas.
pub fn apply(worktree: &Path, op: &Op) -> Result<()> {
    match op {
        Op::Rename { from, to } => {
            let (from, to) = (inside(worktree, from)?, inside(worktree, to)?);
            if to.exists() {
                bail!("{} existe déjà", to.display());
            }
            if let Some(parent) = to.parent() {
                std::fs::create_dir_all(parent)?;
            }
            std::fs::rename(&from, &to)
                .with_context(|| format!("renommage de {} impossible", from.display()))
        }
        Op::Delete { path } => {
            let full = inside(worktree, path)?;
            let result = if full.is_dir() {
                std::fs::remove_dir_all(&full)
            } else {
                std::fs::remove_file(&full)
            };
            result.with_context(|| format!("suppression de {} impossible", full.display()))
        }
        Op::NewFile { path } => {
            let full = inside(worktree, path)?;
            if full.exists() {
                bail!("{} existe déjà", path.display());
            }
            if let Some(parent) = full.parent() {
                std::fs::create_dir_all(parent)?;
            }
            std::fs::write(&full, "")
                .with_context(|| format!("création de {} impossible", full.display()))
        }
        Op::NewDir { path } => {
            let full = inside(worktree, path)?;
            std::fs::create_dir_all(&full)
                .with_context(|| format!("création de {} impossible", full.display()))
        }
    }
}

/// Résout un chemin relatif dans le worktree, en refusant d'en sortir.
fn inside(worktree: &Path, path: &Path) -> Result<PathBuf> {
    if path.is_absolute() {
        bail!(
            "{} : un chemin absolu n'est pas accepté ici",
            path.display()
        );
    }
    if path
        .components()
        .any(|c| matches!(c, std::path::Component::ParentDir))
    {
        bail!("{} : impossible de sortir du worktree", path.display());
    }
    Ok(worktree.join(path))
}

/// Découpe la commande d'un éditeur externe, en y substituant le fichier et la
/// ligne.
///
/// `{path}` et `{line}` sont remplacés partout où ils apparaissent, y compris
/// au milieu d'un argument — `code -g {path}:{line}` et
/// `zed {path}:{line}` s'écrivent ainsi. Sans `{path}`, le chemin est ajouté à
/// la fin : c'est ce qu'attend un `vim` nu.
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

/// Lance l'éditeur externe et rend la main tout de suite.
///
/// Sans attendre : un éditeur graphique ne rend la main qu'à sa fermeture, et
/// un `vim` lancé dans le vide n'en rendrait jamais. Le processus est détaché,
/// ses sorties jetées — c'est un programme à part, pas une commande dont on
/// lit le résultat.
pub fn open_external(worktree: &Path, template: &str, path: &Path, line: usize) -> Result<String> {
    let Some((program, args)) = editor_command(template, &worktree.join(path), line) else {
        bail!("aucun éditeur externe configuré");
    };
    std::process::Command::new(&program)
        .args(&args)
        .current_dir(worktree)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .with_context(|| format!("{program} n'a pas pu être lancé"))?;
    Ok(program)
}

// — Le dossier de notes ————————————————————————————————————————————

/// Ce que `sync_notes` a le droit d'effacer, par la marque du frontmatter.
///
/// Un dossier de coffre contient les notes de son propriétaire, et une note de
/// relecture supprimée ne doit pas emporter le journal de la semaine. La
/// valeur compte autant que la clé : `claudhub: todo` porte notre marque mais
/// **ne nous appartient pas** — c'est l'agent, ou son lecteur, qui le tient, et
/// une écriture de note ne doit pas emporter la liste de tâches en cours.
const OURS: [&str; 2] = ["\nclaudhub: note", "\nclaudhub: review"];

/// Vrai pour un fichier que Claudhub écrit en entier, donc qu'il peut effacer.
fn is_ours(text: &str) -> bool {
    text.starts_with("---") && OURS.iter().any(|mark| text.contains(mark))
}

/// Écrit un fichier du coffre, ou l'efface si son texte est vide.
///
/// Vide veut dire absent : un fichier vide dans un coffre est une coquille que
/// personne n'ouvrira deux fois, et la note libre d'un worktree qu'on vide doit
/// disparaître plutôt que de rester en travers d'une recherche.
///
/// L'empreinte garde l'effacement comme elle garde l'écriture : le fichier peut
/// avoir changé depuis qu'on l'a lu.
pub fn write_vault_file(path: &Path, text: &str, expect: Option<u64>) -> Result<()> {
    if !text.trim().is_empty() {
        return write_at(path, text, expect);
    }
    match std::fs::read_to_string(path) {
        Ok(current) => {
            if expect.is_some_and(|expected| digest(&current) != expected) {
                bail!(
                    "{} a changé depuis son ouverture : rechargez-le avant d'enregistrer",
                    path.display()
                );
            }
            std::fs::remove_file(path)
                .with_context(|| format!("suppression de {} impossible", path.display()))
        }
        // Rien à effacer : c'est l'état qu'on demandait.
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e).with_context(|| format!("lecture de {}", path.display())),
    }
}

/// Les fichiers Markdown d'un dossier de notes, nom et contenu.
///
/// Un dossier absent n'est pas une erreur : c'est l'état d'un worktree qu'on
/// n'a pas encore annoté.
pub fn read_notes(dir: &Path) -> Result<Vec<(String, String)>> {
    let entries = match std::fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(e).with_context(|| format!("lecture de {}", dir.display())),
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

/// Aligne le dossier sur cette liste, et sur elle seule.
///
/// Trois règles, et chacune se paie si on l'oublie :
///
/// - **On ne réécrit pas ce qui n'a pas changé.** Un coffre est souvent
///   synchronisé, et toucher la date d'un fichier à chaque clic ferait
///   travailler la synchronisation pour rien.
/// - **On n'efface que ce que nous écrivons en entier** (`is_ours`). Le
///   dossier peut contenir les notes de son propriétaire, et un `TODO.md` que
///   l'agent tient à jour.
/// - **Ce qui n'est plus dans la liste disparaît**, y compris sous un autre
///   nom : c'est ainsi qu'une note supprimée s'en va, et qu'un fichier renommé
///   dans le coffre ne laisse pas un doublon derrière lui.
pub fn sync_notes(dir: &Path, files: &[(String, String)]) -> Result<()> {
    std::fs::create_dir_all(dir).with_context(|| format!("création de {}", dir.display()))?;
    for (name, content) in files {
        let path = dir.join(name);
        if std::fs::read_to_string(&path).is_ok_and(|old| old == *content) {
            continue;
        }
        std::fs::write(&path, content)
            .with_context(|| format!("écriture de {}", path.display()))?;
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
        sync_notes(&dir, std::slice::from_ref(&ours)).expect("écriture");
        std::fs::write(dir.join("Journal.md"), "---\ntags: [moi]\n---\n\nÀ moi.\n").unwrap();
        // Notre marque, mais pas notre fichier : la liste de tâches appartient
        // à l'agent qui la coche, et une note supprimée ne l'emporte pas.
        std::fs::write(dir.join("TODO.md"), "---\nclaudhub: todo\n---\n\n- [ ] x\n").unwrap();

        let read = read_notes(&dir).expect("lecture");
        assert_eq!(read.len(), 3);

        // La note s'en va, le journal et la liste restent.
        sync_notes(&dir, &[]).expect("effacement");
        let read = read_notes(&dir).expect("lecture");
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
        // Le défaut que `split_command` corrige, ici aussi.
        assert_eq!(
            editor_command(
                r#""/opt/mon éditeur/bin/ed" {path}"#,
                Path::new("/a b.rs"),
                1
            ),
            Some(("/opt/mon éditeur/bin/ed".into(), vec!["/a b.rs".into()]))
        );
    }

    #[test]
    fn a_path_cannot_leave_the_worktree() {
        let worktree = Path::new("/p/repo");
        assert!(inside(worktree, Path::new("src/main.rs")).is_ok());
        assert!(inside(worktree, Path::new("../ailleurs")).is_err());
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
        std::fs::write(dir.join(path), "un").unwrap();

        let read_back = read(&dir, path).unwrap();
        assert_eq!(read_back.text, "un");
        // Un agent écrit pendant qu'on édite.
        std::fs::write(dir.join(path), "deux").unwrap();
        assert!(write(&dir, path, "trois", Some(read_back.hash)).is_err());
        // Et le fichier n'a pas bougé : c'est tout l'intérêt.
        assert_eq!(std::fs::read_to_string(dir.join(path)).unwrap(), "deux");
        // Sans attente, l'écriture passe.
        assert!(write(&dir, path, "trois", None).is_ok());

        let _ = std::fs::remove_dir_all(&dir);
    }
}
