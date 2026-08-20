//! Ce que Claudhub retient d'un worktree entre deux lancements.
//!
//! À part des réglages, et pour la même raison qu'eux : ce n'est pas une
//! préférence qu'on écrit à la main mais l'état d'un travail en cours — la
//! base à laquelle on compare un worktree d'agent, les dossiers qu'on a
//! repliés, le prochain numéro de note. Un `settings.json` où l'on trouverait
//! quelques centaines de lignes par dépôt ne serait plus modifiable.
//!
//! Ce fichier est écrit **depuis le thread d'interface**, ce qui déroge à
//! « `src/ui/` ne fait jamais d'entrée-sortie ». C'est le précédent de
//! `settings.rs` et la même raison : quelques kilo-octets écrits une fois par
//! demi-seconde ne valent pas un aller-retour par le protocole. La règle vise
//! les commandes git, dont la plus rapide coûte déjà une frame, pas la
//! préférence qu'on range.
//!
//! Comme les réglages, tout y est optionnel (`#[serde(default)]`) : un champ
//! ajouté ne casse pas un fichier déjà écrit, et un fichier illisible n'empêche
//! jamais de démarrer.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::Duration;

use gpui::{App, BorrowAppContext};
use serde::{Deserialize, Serialize};

use crate::ui::notes::Note;

/// Même délai que les réglages : assez court pour qu'une fermeture brutale ne
/// perde rien de visible, assez long pour qu'un glissement de souris n'écrive
/// pas un fichier par image.
const SAVE_DELAY: Duration = Duration::from_millis(500);

/// Ce qui survit au redémarrage pour un checkout donné.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct WorktreeState {
    /// Dépôt principal dont ce checkout dépend.
    ///
    /// Il ne sert qu'à la purge, et il lui est indispensable : sans lui, une
    /// entrée absente de la liste des worktrees d'un dépôt qu'on vient
    /// d'ouvrir ne se distingue pas d'une entrée appartenant à un dépôt qu'on
    /// n'a pas encore ouvert — et l'oublier reviendrait à effacer les notes
    /// d'un worktree bien vivant.
    pub repo: PathBuf,
    /// Base de comparaison de la revue de branche. Le choix est propre au
    /// worktree, et le réapprendre à chaque lancement était le manque que ce
    /// magasin comble en premier.
    pub base: Option<String>,
    /// Dossiers repliés dans la liste de revue.
    ///
    /// Un `Vec` et non un `HashSet` : c'est la forme que JSON sait écrire, et
    /// la vue en refait un ensemble à la lecture.
    pub collapsed: Vec<PathBuf>,
    /// Notes de relecture d'un fichier d'état antérieur, et rien d'autre.
    ///
    /// Elles vivent désormais dans un dossier de fichiers Markdown : ce champ
    /// n'existe que pour les y verser une fois, à l'arrivée du dossier, après
    /// quoi il est vidé. Rien ne l'écrit plus.
    pub notes: Vec<Note>,
    /// Prochain identifiant de note. Il ne se déduit pas de `notes` : une note
    /// supprimée libérerait son numéro, et une note envoyée à l'agent y serait
    /// désignée par un numéro qui vaudrait pour une autre.
    pub next_note: u64,
}

/// Ce qui survit au redémarrage pour un dépôt, worktrees confondus.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct RepoState {
    /// Projet Sentry, qui dépend du dépôt et non du worktree ni du compte.
    pub sentry_project: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Store {
    /// Clé : le chemin du checkout, comme partout ailleurs dans Claudhub.
    pub worktrees: HashMap<PathBuf, WorktreeState>,
    /// Clé : le chemin du dépôt principal.
    pub repos: HashMap<PathBuf, RepoState>,
}

impl Store {
    pub fn load() -> Self {
        let Some(path) = state_path() else {
            return Self::default();
        };
        match std::fs::read_to_string(&path) {
            Ok(text) => serde_json::from_str(&text).unwrap_or_else(|e| {
                // Écraser un fichier qu'on n'a pas su lire ferait perdre les
                // notes de tous les worktrees pour une seule clé mal formée.
                log::warn!("état illisible ({}) : {e}", path.display());
                Self::default()
            }),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Self::default(),
            Err(e) => {
                log::warn!("lecture de l'état : {e}");
                Self::default()
            }
        }
    }

    pub fn save(&self) {
        let Some(path) = state_path() else { return };
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        match serde_json::to_string_pretty(self) {
            Ok(json) => {
                if let Err(e) = crate::ui::settings::write_private(&path, &json) {
                    log::warn!("écriture de l'état : {e}");
                }
            }
            Err(e) => log::warn!("sérialisation de l'état : {e}"),
        }
    }

    pub fn worktree(&self, path: &Path) -> Option<&WorktreeState> {
        self.worktrees.get(path)
    }

    /// L'état d'un checkout, créé au besoin.
    ///
    /// Le dépôt est exigé à l'écriture parce que c'est le seul moment où on le
    /// connaît à coup sûr, et que la purge en dépend.
    pub fn worktree_mut(&mut self, path: &Path, repo: &Path) -> &mut WorktreeState {
        let state = self.worktrees.entry(path.to_path_buf()).or_default();
        state.repo = repo.to_path_buf();
        state
    }

    /// Oublie les checkouts d'un dépôt qui n'existent plus.
    ///
    /// Appelée quand git vient d'énumérer les worktrees : c'est le seul moment
    /// où la liste est sûre. Les entrées d'un autre dépôt — et celles écrites
    /// avant que le champ `repo` existe, dont le dépôt est vide — sont
    /// laissées intactes : mieux vaut une entrée morte qu'une note effacée.
    pub fn forget_missing(&mut self, repo: &Path, alive: &[PathBuf]) {
        self.worktrees.retain(|path, state| {
            state.repo.as_os_str().is_empty() || state.repo != repo || alive.contains(path)
        });
    }
}

// --- Global -----------------------------------------------------------------

pub struct StateStore {
    store: Store,
    /// Vrai quand une écriture différée est déjà programmée : les
    /// modifications suivantes s'y agrègent au lieu d'en programmer une autre.
    saving: bool,
}

impl gpui::Global for StateStore {}

impl Store {
    /// Installe l'état chargé. À appeler une fois, au démarrage.
    pub fn init_global(self, cx: &mut App) {
        cx.set_global(StateStore {
            store: self,
            saving: false,
        });
    }

    pub fn global(cx: &App) -> &Store {
        &cx.global::<StateStore>().store
    }

    /// Modifie l'état et programme son écriture.
    ///
    /// Aucun effet de bord visible, contrairement aux réglages : rien ici ne
    /// porte de police ni de thème, et la vue qui appelle sait déjà ce qu'elle
    /// doit réafficher.
    pub fn update_global(cx: &mut App, f: impl FnOnce(&mut Store)) {
        let changed = cx.update_global::<StateStore, _>(|store, _| {
            let before = store.store.clone();
            f(&mut store.store);
            store.store != before
        });
        if changed {
            schedule_save(cx);
        }
    }
}

fn schedule_save(cx: &mut App) {
    let already_scheduled =
        cx.update_global::<StateStore, _>(|store, _| std::mem::replace(&mut store.saving, true));
    if already_scheduled {
        return;
    }
    cx.spawn(async move |cx| {
        cx.background_executor().timer(SAVE_DELAY).await;
        let store = cx.update(|cx| {
            cx.update_global::<StateStore, _>(|store, _| {
                store.saving = false;
                store.store.clone()
            })
        });
        if let Ok(store) = store {
            store.save();
        }
    })
    .detach();
}

/// Où l'état est rangé, à côté des réglages et de la disposition.
fn state_path() -> Option<PathBuf> {
    crate::ui::settings::config_dir().map(|dir| dir.join("state.json"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_keys_take_their_defaults() {
        // Un fichier écrit par une version qui ignorait les notes doit
        // continuer de se charger — c'est le cas de tous ceux déjà sur le
        // disque des utilisateurs dès qu'on ajoute un champ.
        let store: Store =
            serde_json::from_str(r#"{"worktrees":{"/w":{"base":"origin/main"}}}"#).unwrap();
        let state = store.worktree(Path::new("/w")).unwrap();
        assert_eq!(state.base.as_deref(), Some("origin/main"));
        assert!(state.notes.is_empty());
        assert!(state.collapsed.is_empty());
        assert!(store.repos.is_empty());
    }

    #[test]
    fn writing_a_worktree_records_its_repository() {
        let mut store = Store::default();
        store
            .worktree_mut(Path::new("/r/wt/a"), Path::new("/r"))
            .base = Some("dev".into());
        assert_eq!(
            store.worktree(Path::new("/r/wt/a")).unwrap().repo,
            PathBuf::from("/r")
        );
    }

    #[test]
    fn purging_only_touches_the_repository_being_listed() {
        let mut store = Store::default();
        store.worktree_mut(Path::new("/r/a"), Path::new("/r")).base = Some("dev".into());
        store.worktree_mut(Path::new("/r/b"), Path::new("/r")).base = Some("dev".into());
        // Un autre dépôt, qui n'est pas concerné par cette énumération.
        store
            .worktree_mut(Path::new("/other/a"), Path::new("/other"))
            .base = Some("main".into());
        // Et une entrée écrite avant que le dépôt soit retenu : on ne sait pas
        // à qui elle appartient, donc on n'y touche pas.
        store.worktrees.insert(
            PathBuf::from("/legacy"),
            WorktreeState {
                base: Some("main".into()),
                ..Default::default()
            },
        );

        store.forget_missing(Path::new("/r"), &[PathBuf::from("/r/a")]);

        assert!(store.worktree(Path::new("/r/a")).is_some());
        assert!(store.worktree(Path::new("/r/b")).is_none());
        assert!(store.worktree(Path::new("/other/a")).is_some());
        assert!(store.worktree(Path::new("/legacy")).is_some());
    }
}
