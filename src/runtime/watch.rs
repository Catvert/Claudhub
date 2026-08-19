//! Surveillance des fichiers d'un worktree.
//!
//! C'est ce qui rend la revue vivante quand le travail se fait ailleurs : un
//! agent qui écrit dans le terminal intégré, un `git commit` tapé à la main,
//! un éditeur externe. Sans cela il faudrait appuyer sur « actualiser » après
//! chaque action, ce qui, dans un outil dont le sujet est justement de
//! regarder ce qu'un autre processus fabrique, est le mauvais défaut.
//!
//! Deux sources d'événements comptent : l'arborescence de travail, et
//! `.git/HEAD` / `.git/index` — sans ces deux-là, un commit tapé au clavier
//! laisserait le panneau afficher des fichiers qui ne sont plus modifiés.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::time::Duration;

use notify::RecursiveMode;
use notify_debouncer_full::{new_debouncer, DebouncedEvent, Debouncer, RecommendedCache};

/// Fenêtre de regroupement. Une compilation touche des milliers de fichiers ;
/// rafraîchir à chaque écriture reviendrait à lancer `git status` en boucle.
/// Un quart de seconde reste imperceptible et ramène une rafale à un seul
/// rafraîchissement.
const DEBOUNCE: Duration = Duration::from_millis(250);

pub struct Watcher {
    debouncer: Debouncer<notify::RecommendedWatcher, RecommendedCache>,
    watched: HashSet<PathBuf>,
}

impl Watcher {
    /// Démarre la surveillance et rend le récepteur des chemins modifiés.
    ///
    /// Ce que le récepteur livre est un chemin *quelconque* sous un worktree
    /// surveillé ; c'est à l'appelant de le rattacher au worktree qu'il
    /// connaît, lui seul sachant lesquels sont ouverts.
    pub fn new() -> anyhow::Result<(Self, async_channel::Receiver<PathBuf>)> {
        // Canal async : c'est une tâche gpui qui le draine, et elle ne peut pas
        // se permettre d'attendre sur un `recv` bloquant.
        let (tx, rx) = async_channel::unbounded::<PathBuf>();
        let (raw_tx, raw_rx) = mpsc::channel();
        let debouncer = new_debouncer(DEBOUNCE, None, raw_tx)?;

        // Un thread de traduction : il ne garde d'un lot que les chemins qui
        // changent quelque chose, dédoublonnés.
        std::thread::Builder::new()
            .name("perch-watch".into())
            .spawn(move || {
                while let Ok(result) = raw_rx.recv() {
                    let Ok(events) = result else { continue };
                    let mut seen: Vec<PathBuf> = Vec::new();
                    for event in &events {
                        for path in interesting_paths(event) {
                            if !seen.contains(&path) {
                                seen.push(path);
                            }
                        }
                    }
                    for path in seen {
                        if tx.send_blocking(path).is_err() {
                            return; // la fenêtre est partie
                        }
                    }
                }
            })?;

        Ok((
            Self {
                debouncer,
                watched: HashSet::new(),
            },
            rx,
        ))
    }

    /// Surveille un worktree. Appeler deux fois est sans effet.
    pub fn watch(&mut self, worktree: &Path) {
        if self.watched.contains(worktree) {
            return;
        }
        if let Err(e) = self.debouncer.watch(worktree, RecursiveMode::Recursive) {
            log::warn!("surveillance de {} impossible : {e}", worktree.display());
            return;
        }
        self.watched.insert(worktree.to_path_buf());
    }

    pub fn unwatch(&mut self, worktree: &Path) {
        if self.watched.remove(worktree) {
            let _ = self.debouncer.unwatch(worktree);
        }
    }
}

/// Les chemins d'un événement qui méritent un rafraîchissement.
fn interesting_paths(event: &DebouncedEvent) -> Vec<PathBuf> {
    event
        .paths
        .iter()
        .filter(|p| is_interesting(p))
        .cloned()
        .collect()
}

/// Tout ce qui est sous `.git/` est écarté sauf les quelques références dont
/// la modification change ce que `git status` répond : sans ce filtre, chaque
/// commande git déclencherait une dizaine de rafraîchissements pour ses
/// fichiers de verrou, ses journaux et ses objets fraîchement écrits.
fn is_interesting(path: &Path) -> bool {
    let text = path.to_string_lossy();
    let Some(pos) = text.find("/.git/") else {
        // Hors de `.git/` : c'est du travail, ça compte — y compris un
        // `Cargo.lock`, qui est un fichier suivi comme un autre.
        return !text.ends_with("/.git");
    };
    let inside = &text[pos + "/.git/".len()..];
    // Les verrous sont créés puis détruits par toute commande git : ils
    // annoncent une écriture qui n'a pas encore eu lieu.
    if inside.ends_with(".lock") {
        return false;
    }
    inside == "HEAD"
        || inside == "index"
        || inside == "ORIG_HEAD"
        || inside == "MERGE_HEAD"
        || inside.starts_with("refs/")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn work_tree_changes_are_interesting() {
        assert!(is_interesting(Path::new("/repo/src/main.rs")));
        assert!(is_interesting(Path::new("/repo/assets/i18n/fr.json")));
        // Un fichier suivi qui s'appelle `.lock` reste du travail.
        assert!(is_interesting(Path::new("/repo/Cargo.lock")));
    }

    #[test]
    fn git_internals_are_filtered_except_the_refs_that_matter() {
        assert!(is_interesting(Path::new("/repo/.git/HEAD")));
        assert!(is_interesting(Path::new("/repo/.git/index")));
        assert!(is_interesting(Path::new("/repo/.git/refs/heads/main")));

        // Bruit de toute commande git.
        assert!(!is_interesting(Path::new("/repo/.git/index.lock")));
        assert!(!is_interesting(Path::new("/repo/.git/logs/HEAD")));
        assert!(!is_interesting(Path::new("/repo/.git/objects/ab/cdef")));
        assert!(!is_interesting(Path::new("/repo/.git/COMMIT_EDITMSG")));
    }

    /// Le seul test qui prouve que la chaîne complète marche : un vrai
    /// observateur du système de fichiers, une vraie écriture, et le chemin
    /// qui ressort à l'autre bout. Les deux tests précédents ne valident que
    /// le filtre.
    #[test]
    fn a_real_write_reaches_the_receiver() {
        let dir = std::env::temp_dir().join(format!("perch-watch-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("répertoire temporaire");

        let (mut watcher, changes) = Watcher::new().expect("observateur disponible");
        watcher.watch(&dir);

        let file = dir.join("écrit pendant la surveillance.txt");
        std::fs::write(&file, b"contenu").expect("écriture");

        // Le débounce est de 250 ms ; la marge couvre une machine chargée sans
        // rendre l'échec silencieux — au-delà, c'est que rien n'est arrivé.
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        let mut received = None;
        while std::time::Instant::now() < deadline {
            if let Ok(path) = changes.try_recv() {
                received = Some(path);
                break;
            }
            std::thread::sleep(Duration::from_millis(50));
        }

        let _ = std::fs::remove_dir_all(&dir);
        assert_eq!(
            received.as_deref().and_then(Path::file_name),
            Some(file.file_name().unwrap()),
            "l'écriture n'a pas été signalée"
        );
    }
}
