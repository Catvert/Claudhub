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

use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};

/// Au-delà, l'éditeur intégré n'est pas une bonne idée : `InputState` documente
/// cinquante mille lignes, et une fenêtre figée est un pire service qu'un
/// refus.
pub const MAX_LINES: usize = 50_000;

/// Ce qu'on a lu, et de quoi vérifier qu'on écrit bien par-dessus.
#[derive(Debug, Clone, PartialEq, Eq)]
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
pub fn digest(text: &str) -> u64 {
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    text.hash(&mut hasher);
    hasher.finish()
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
    let full = worktree.join(path);
    if let Some(expected) = expect {
        let current = std::fs::read_to_string(&full)
            .map(|text| digest(&text))
            .ok();
        if current != Some(expected) {
            bail!(
                "{} a changé depuis son ouverture : rechargez-le avant d'enregistrer",
                path.display()
            );
        }
    }
    if let Some(parent) = full.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(&full, text)
        .with_context(|| format!("écriture de {} impossible", full.display()))
}

/// Ce qu'on fait à un fichier depuis l'explorateur.
#[derive(Debug, Clone, PartialEq, Eq)]
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
    let mut parts: Vec<String> = crate::ui::split_command(template)
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

#[cfg(test)]
mod tests {
    use super::*;

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
