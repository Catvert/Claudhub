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
use std::path::{Component, Path, PathBuf};
use std::sync::mpsc;
use std::sync::OnceLock;
use std::time::Duration;

use notify::RecursiveMode;
use notify_debouncer_full::{new_debouncer, DebouncedEvent};

/// Ce que le thread de surveillance sait faire.
enum Order {
    Watch(PathBuf),
    Unwatch(PathBuf),
}

/// Fenêtre de regroupement. Une compilation touche des milliers de fichiers ;
/// rafraîchir à chaque écriture reviendrait à lancer `git status` en boucle.
/// Un quart de seconde reste imperceptible et ramène une rafale à un seul
/// rafraîchissement.
const DEBOUNCE: Duration = Duration::from_millis(250);

/// La façade côté interface : elle ne fait qu'envoyer des ordres.
///
/// Poser une surveillance récursive coûte un appel système par dossier — près
/// d'une demi-seconde sur une arborescence de quarante mille répertoires, ce
/// qui est une demi-seconde de fenêtre figée si on le fait dans le thread qui
/// dessine. L'ordre part donc dans un thread dédié et le sélecteur de worktree
/// rend la main tout de suite.
pub struct Watcher {
    orders: mpsc::Sender<Order>,
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
        let mut debouncer = new_debouncer(DEBOUNCE, None, raw_tx)?;
        let (order_tx, order_rx) = mpsc::channel::<Order>();

        // Un thread pour poser et retirer les surveillances, opérations longues
        // sur une grosse arborescence.
        std::thread::Builder::new()
            .name("perch-watch-orders".into())
            .spawn(move || {
                let mut watched: HashSet<PathBuf> = HashSet::new();
                while let Ok(order) = order_rx.recv() {
                    match order {
                        Order::Watch(path) => {
                            if !watched.insert(path.clone()) {
                                continue;
                            }
                            if on_windows_filesystem(&path) {
                                log::warn!(
                                    "{} est sur un disque Windows : inotify n'y \
                                     remonte rien, la revue ne se rafraîchira pas \
                                     toute seule",
                                    path.display()
                                );
                            }
                            for (dir, mode) in watchable_directories(&path) {
                                if let Err(e) = debouncer.watch(&dir, mode) {
                                    log::warn!(
                                        "surveillance de {} impossible : {e}",
                                        dir.display()
                                    );
                                }
                            }
                        }
                        Order::Unwatch(path) => {
                            if watched.remove(&path) {
                                for (dir, _) in watchable_directories(&path) {
                                    let _ = debouncer.unwatch(&dir);
                                }
                            }
                        }
                    }
                }
            })?;

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

        Ok((Self { orders: order_tx }, rx))
    }

    /// Surveille un worktree. Appeler deux fois est sans effet, et l'appel rend
    /// la main immédiatement : le travail se fait ailleurs.
    pub fn watch(&mut self, worktree: &Path) {
        let _ = self.orders.send(Order::Watch(worktree.to_path_buf()));
    }

    pub fn unwatch(&mut self, worktree: &Path) {
        let _ = self.orders.send(Order::Unwatch(worktree.to_path_buf()));
    }
}

/// Les chemins d'un événement qui méritent un rafraîchissement.
fn interesting_paths(event: &DebouncedEvent) -> Vec<PathBuf> {
    if !changes_content(&event.kind) {
        return Vec::new();
    }
    event
        .paths
        .iter()
        .filter(|p| is_interesting(p))
        .cloned()
        .collect()
}

/// Vrai pour un événement susceptible de changer ce que `git status` répond.
///
/// Le filtre décisif est `Access` : inotify signale chaque **ouverture** de
/// fichier, et c'est nous qui les ouvrons. `git status` lisait le worktree,
/// chaque lecture produisait un événement, chaque événement déclenchait un
/// `git status` — une boucle qui tournait à plein régime, invisible tant que la
/// liste ne se vidait pas entre deux réponses.
///
/// Les métadonnées sont écartées pour la même raison : une date d'accès ou un
/// mode qui change ne change rien à ce que git voit. `Any` et `Other` sont
/// gardés — c'est ainsi que `notify` signale un débordement de sa file, après
/// lequel on a justement tout à relire.
fn changes_content(kind: &notify::EventKind) -> bool {
    use notify::event::{EventKind, ModifyKind};
    match kind {
        EventKind::Access(_) => false,
        EventKind::Modify(ModifyKind::Metadata(_)) => false,
        EventKind::Create(_) | EventKind::Remove(_) | EventKind::Modify(_) => true,
        EventKind::Any | EventKind::Other => true,
    }
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

/// Les dossiers d'un worktree qui valent la peine d'être surveillés.
///
/// Ce sont ceux que git connaît : les dossiers contenant un fichier suivi, ou
/// un fichier nouveau qui n'est pas ignoré. C'est `git ls-files` qui les
/// donne, en une commande et quelques dizaines de millisecondes.
///
/// Surveiller le worktree en bloc coûterait cent fois plus cher pour rien. Sur
/// l'application qui a motivé ce filtre, quarante mille répertoires existent
/// mais sept cent vingt et un contiennent du code : le reste est `vendor/`,
/// `node_modules/` et surtout `storage/`, que Laravel ne déclare pas ignoré
/// dossier par dossier et qu'un serveur de développement réécrit sans arrêt.
/// Chacune de ces écritures produisait un réveil, donc un `git status`, donc
/// un rechargement de la revue — en boucle.
///
/// Vrai si ce chemin est sur un disque Windows monté par WSL.
///
/// C'est le seul cas où la surveillance échoue **sans rien dire** : sur drvfs
/// (`/mnt/c`, `/mnt/d`…), `notify` pose ses surveillances sans erreur et ne
/// livre jamais un événement, parce que le noyau WSL n'a rien à traduire — les
/// écritures ont lieu côté Windows. Toute la promesse « la revue suit sans
/// qu'on lui demande » disparaît alors en silence, et c'est à l'interface de
/// le dire.
///
/// Le repli par sondage serait pire : `git status` y coûte déjà plusieurs fois
/// ce qu'il coûte sur le système de fichiers Linux, et le lancer sur un
/// minuteur ferait payer en permanence ce que le déplacement du dépôt vers
/// `~` supprime d'un coup.
pub fn on_windows_filesystem(path: &Path) -> bool {
    running_under_wsl() && is_windows_mount(path)
}

/// Le noyau de WSL porte « microsoft » dans sa version, sous WSL1 comme sous
/// WSL2 ; c'est la façon dont tout le monde le reconnaît, faute d'autre
/// marqueur stable.
fn running_under_wsl() -> bool {
    static WSL: OnceLock<bool> = OnceLock::new();
    *WSL.get_or_init(|| {
        std::fs::read_to_string("/proc/sys/kernel/osrelease")
            .map(|release| release.to_ascii_lowercase().contains("microsoft"))
            .unwrap_or(false)
    })
}

/// `/mnt/c`, `/mnt/d`… — la façon dont WSL monte les lecteurs Windows.
///
/// La racine d'automontage se configure (`/etc/wsl.conf`), donc la
/// reconnaissance rate les installations qui l'ont déplacée. C'est assumé :
/// ce test ne sert qu'à afficher un avertissement, et un avertissement qui
/// manque vaut mieux qu'un avertissement qui ment.
fn is_windows_mount(path: &Path) -> bool {
    let mut parts = path.components();
    if parts.next() != Some(Component::RootDir) {
        return false;
    }
    if parts.next() != Some(Component::Normal("mnt".as_ref())) {
        return false;
    }
    match parts.next() {
        Some(Component::Normal(drive)) => drive
            .to_str()
            .is_some_and(|d| d.len() == 1 && d.chars().all(|c| c.is_ascii_alphabetic())),
        _ => false,
    }
}

/// Chaque dossier est surveillé **sans récursion** : ses sous-dossiers sont
/// déjà dans la liste s'ils contiennent quelque chose, et un dossier créé plus
/// tard est signalé par son parent, ce qui suffit à déclencher le
/// rafraîchissement qui le découvrira.
///
/// `.git` est ajouté à part : sa racine pour `HEAD` et `index`, et `refs/`
/// récursivement puisqu'il est petit. Le prendre en entier ramènerait les
/// milliers de répertoires d'objets.
fn watchable_directories(worktree: &Path) -> Vec<(PathBuf, RecursiveMode)> {
    let mut dirs: Vec<(PathBuf, RecursiveMode)> = Vec::new();

    match tracked_directories(worktree) {
        Some(tracked) => {
            dirs.extend(
                tracked
                    .into_iter()
                    .map(|dir| (dir, RecursiveMode::NonRecursive)),
            );
            dirs.push((worktree.to_path_buf(), RecursiveMode::NonRecursive));
        }
        // Sans git sous la main, tout surveiller reste juste, seulement lent.
        None => dirs.push((worktree.to_path_buf(), RecursiveMode::Recursive)),
    }

    if let Some(git_dir) = git_dir(worktree) {
        let refs = git_dir.join("refs");
        dirs.push((git_dir, RecursiveMode::NonRecursive));
        if refs.is_dir() {
            dirs.push((refs, RecursiveMode::Recursive));
        }
    }
    dirs
}

/// Les dossiers contenant un fichier que git suit, ou un fichier nouveau qu'il
/// n'ignore pas.
fn tracked_directories(worktree: &Path) -> Option<HashSet<PathBuf>> {
    use std::process::{Command, Stdio};

    let out = Command::new("git")
        .arg("-C")
        .arg(worktree)
        .args([
            "ls-files",
            "-z",
            "--cached",
            "--others",
            "--exclude-standard",
        ])
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }

    let text = String::from_utf8_lossy(&out.stdout);
    let mut dirs = HashSet::new();
    for file in text.split('\0').filter(|s| !s.is_empty()) {
        // Chaque ancêtre, pas seulement le parent : un dossier intermédiaire
        // qui ne contient que des sous-dossiers doit être surveillé lui aussi,
        // sans quoi la création d'un fichier à ce niveau passerait inaperçue.
        let mut current = Path::new(file).parent();
        while let Some(dir) = current.filter(|d| !d.as_os_str().is_empty()) {
            if !dirs.insert(worktree.join(dir)) {
                break; // déjà vu : ses ancêtres le sont aussi
            }
            current = dir.parent();
        }
    }
    Some(dirs)
}

/// Le répertoire git d'un checkout.
///
/// Dans le dépôt principal c'est `.git/`. Dans un worktree lié, `.git` est un
/// *fichier* qui pointe vers `<principal>/.git/worktrees/<nom>` : c'est là que
/// vivent le `HEAD` et l'`index` de ce checkout, et les surveiller au mauvais
/// endroit revient à ne rien surveiller du tout.
fn git_dir(worktree: &Path) -> Option<PathBuf> {
    let entry = worktree.join(".git");
    if entry.is_dir() {
        return Some(entry);
    }
    let text = std::fs::read_to_string(&entry).ok()?;
    let target = text.strip_prefix("gitdir:")?.trim();
    let path = PathBuf::from(target);
    let path = if path.is_absolute() {
        path
    } else {
        worktree.join(path)
    };
    path.is_dir().then_some(path)
}

#[cfg(test)]
mod tests {
    use notify::event::{AccessKind, CreateKind, EventKind, MetadataKind, ModifyKind};

    /// Le défaut qui faisait tourner Perch en boucle : inotify signale chaque
    /// ouverture de fichier, `git status` en ouvre des milliers, et chaque
    /// événement relançait un `git status`.
    #[test]
    fn opening_a_file_is_not_a_change() {
        assert!(!changes_content(&EventKind::Access(AccessKind::Open(
            notify::event::AccessMode::Any
        ))));
        assert!(!changes_content(&EventKind::Access(AccessKind::Read)));
        // Une date d'accès n'apprend rien non plus à git.
        assert!(!changes_content(&EventKind::Modify(ModifyKind::Metadata(
            MetadataKind::AccessTime
        ))));

        assert!(changes_content(&EventKind::Create(CreateKind::File)));
        assert!(changes_content(&EventKind::Modify(ModifyKind::Data(
            notify::event::DataChange::Content
        ))));
        assert!(changes_content(&EventKind::Remove(
            notify::event::RemoveKind::File
        )));
        // Le signal de débordement de la file : on a tout à relire.
        assert!(changes_content(&EventKind::Any));
    }

    use super::*;

    /// Un dépôt sur `/mnt/c` ne remonte aucun événement ; c'est ce test qui
    /// tient la reconnaissance, la partie « suis-je sous WSL » n'étant pas
    /// vérifiable ailleurs que sous WSL.
    #[test]
    fn windows_drives_are_recognised_by_their_mount_point() {
        assert!(is_windows_mount(Path::new("/mnt/c/Users/ami/projet")));
        assert!(is_windows_mount(Path::new("/mnt/d")));
        assert!(!is_windows_mount(Path::new("/home/ami/projet")));
        // Un point de montage à nous qui commence pareil n'en est pas un.
        assert!(!is_windows_mount(Path::new("/mnt/data/projet")));
        assert!(!is_windows_mount(Path::new("/mnt")));
    }

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

    #[test]
    fn a_linked_worktree_points_at_its_own_git_directory() {
        let root = std::env::temp_dir().join(format!("perch-gitdir-{}", std::process::id()));
        let main = root.join("depot/.git/worktrees/feature");
        let linked = root.join("feature");
        std::fs::create_dir_all(&main).unwrap();
        std::fs::create_dir_all(&linked).unwrap();
        // Ce que git écrit dans un worktree lié.
        std::fs::write(linked.join(".git"), format!("gitdir: {}\n", main.display())).unwrap();

        assert_eq!(git_dir(&linked).as_deref(), Some(main.as_path()));

        // Et le dépôt principal, dont le `.git` est un vrai répertoire.
        let plain = root.join("depot");
        assert_eq!(git_dir(&plain), Some(plain.join(".git")));

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn only_the_directories_git_knows_are_watched() {
        // Un vrai petit dépôt : du code suivi, et un dossier ignoré qui pèse.
        let root = std::env::temp_dir().join(format!("perch-tracked-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("src/interne")).unwrap();
        std::fs::create_dir_all(root.join("vendor/paquet/profond")).unwrap();
        std::fs::write(root.join("src/interne/code.rs"), "fn main() {}").unwrap();
        std::fs::write(root.join("vendor/paquet/profond/gros.php"), "<?php").unwrap();
        std::fs::write(root.join(".gitignore"), "vendor/\n").unwrap();
        for args in [
            vec!["init", "-q"],
            vec!["config", "user.email", "t@example.com"],
            vec!["config", "user.name", "T"],
            vec!["add", "."],
        ] {
            std::process::Command::new("git")
                .arg("-C")
                .arg(&root)
                .args(&args)
                .output()
                .unwrap();
        }

        let dirs = watchable_directories(&root);
        let watched: Vec<&Path> = dirs.iter().map(|(p, _)| p.as_path()).collect();

        assert!(
            watched.contains(&root.join("src/interne").as_path()),
            "un dossier de code doit être surveillé : {watched:?}"
        );
        assert!(
            watched.contains(&root.join("src").as_path()),
            "les dossiers intermédiaires aussi"
        );
        assert!(
            !watched.iter().any(|p| p.starts_with(root.join("vendor"))),
            "rien d'ignoré ne doit être surveillé : {watched:?}"
        );
        // Aucune surveillance récursive de l'arborescence de travail : c'est
        // elle qui ramènerait les dossiers écartés.
        assert!(
            dirs.iter()
                .filter(|(p, _)| !p.starts_with(root.join(".git")))
                .all(|(_, m)| matches!(m, RecursiveMode::NonRecursive)),
            "aucune surveillance récursive hors de .git"
        );

        let _ = std::fs::remove_dir_all(&root);
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

        // `watch` rend la main avant que la surveillance soit posée — c'est
        // tout l'intérêt, le thread d'interface ne doit pas attendre les
        // milliers d'appels système que cela demande sur un vrai projet. Le
        // test réécrit donc le fichier à chaque tour : dès que la surveillance
        // est en place, l'écriture suivante est vue.
        let deadline = std::time::Instant::now() + Duration::from_secs(10);
        let mut received = None;
        while std::time::Instant::now() < deadline {
            std::fs::write(&file, b"contenu").expect("écriture");
            std::thread::sleep(Duration::from_millis(100));
            if let Ok(path) = changes.try_recv() {
                received = Some(path);
                break;
            }
        }

        let _ = std::fs::remove_dir_all(&dir);
        assert_eq!(
            received.as_deref().and_then(Path::file_name),
            Some(file.file_name().unwrap()),
            "l'écriture n'a pas été signalée"
        );
    }
}
