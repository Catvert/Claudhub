//! Le protocole entre le thread d'interface et les workers.
//!
//! Rien ici ne fait de travail : ce sont des données. Le thread UI envoie des
//! `Cmd`, les workers répondent par des `Evt`. Aucune commande git n'est
//! lancée depuis un `render` ou un gestionnaire de clic — la plus rapide
//! (`git status` sur un petit dépôt) coûte déjà plusieurs millisecondes, soit
//! une frame perdue à chaque frappe.

use std::path::PathBuf;

use crate::git::{
    Branch, Commit, DiffFile, DiffRange, FileDiff, GraphRow, LogRange, Status, Worktree,
};

/// Identifie le checkout concerné. C'est le chemin qui sert de clé partout :
/// il est stable, unique, et c'est aussi le répertoire de travail passé à git.
pub type WorktreeId = PathBuf;

#[derive(Debug, Clone)]
pub enum Cmd {
    /// Ouvre un dépôt (ou le worktree d'un dépôt) et énumère ses worktrees.
    OpenRepo(PathBuf),
    /// Relit worktrees, état et branches d'un dépôt déjà ouvert.
    RefreshRepo {
        main: PathBuf,
    },
    /// `git status` d'un checkout.
    RefreshStatus {
        worktree: WorktreeId,
    },
    /// Liste les fichiers d'un domaine de revue.
    LoadDiffFiles {
        worktree: WorktreeId,
        range: DiffRange,
    },
    /// Diff d'un fichier ; `untracked` bascule sur `--no-index`, git ne
    /// connaissant pas encore le fichier.
    LoadFileDiff {
        worktree: WorktreeId,
        range: DiffRange,
        path: PathBuf,
        context: usize,
        untracked: bool,
    },
    LoadBranches {
        main: PathBuf,
    },
    /// Charge l'historique d'un checkout, avec la disposition de son graphe.
    LoadHistory {
        worktree: WorktreeId,
        range: LogRange,
        limit: usize,
    },

    Stage {
        worktree: WorktreeId,
        paths: Vec<PathBuf>,
    },
    Unstage {
        worktree: WorktreeId,
        paths: Vec<PathBuf>,
    },
    /// Destructif : les modifications sont perdues. La confirmation est de la
    /// responsabilité de l'appelant, pas du worker.
    Discard {
        worktree: WorktreeId,
        paths: Vec<PathBuf>,
    },
    /// Indexe (ou dés-indexe, avec `reverse`) un seul hunk.
    ApplyHunk {
        worktree: WorktreeId,
        patch: String,
        reverse: bool,
    },

    Commit {
        worktree: WorktreeId,
        message: String,
        amend: bool,
        all: bool,
    },
    Fetch {
        worktree: WorktreeId,
    },
    Pull {
        worktree: WorktreeId,
    },
    Push {
        worktree: WorktreeId,
        force_with_lease: bool,
    },
    Checkout {
        worktree: WorktreeId,
        branch: String,
    },
    CreateBranch {
        worktree: WorktreeId,
        name: String,
        from: Option<String>,
    },
    DeleteBranch {
        main: PathBuf,
        name: String,
        force: bool,
    },

    AddWorktree {
        main: PathBuf,
        path: PathBuf,
        branch: String,
        from: Option<String>,
    },
    RemoveWorktree {
        main: PathBuf,
        path: PathBuf,
        force: bool,
    },
}

#[derive(Debug, Clone)]
pub enum Evt {
    RepoOpened {
        main: PathBuf,
        name: String,
        worktrees: Vec<Worktree>,
        /// Le checkout d'où l'ouverture a été demandée, quand c'en est un.
        /// Lancer `perch` depuis un worktree doit ouvrir *ce* worktree, pas le
        /// dépôt principal qui se trouve en tête de la liste.
        opened_at: Option<WorktreeId>,
    },
    Worktrees {
        main: PathBuf,
        worktrees: Vec<Worktree>,
    },
    Status {
        worktree: WorktreeId,
        status: Status,
    },
    DiffFiles {
        worktree: WorktreeId,
        range: DiffRange,
        files: Vec<DiffFile>,
    },
    FileDiff {
        worktree: WorktreeId,
        path: PathBuf,
        diff: FileDiff,
    },
    Branches {
        main: PathBuf,
        branches: Vec<Branch>,
        /// Branche d'intégration du dépôt, telle que git la déclare
        /// (`origin/HEAD`, puis `init.defaultBranch`, puis les noms usuels qui
        /// existent vraiment). `None` sur un dépôt qui n'en a aucune : la
        /// revue de branche n'a alors rien à quoi se comparer.
        default_base: Option<String>,
    },
    History {
        worktree: WorktreeId,
        range: LogRange,
        commits: Vec<Commit>,
        /// Une entrée par commit, dans le même ordre : la vue les affiche côte
        /// à côte et un décalage ferait pointer chaque trait sur le mauvais.
        graph: Vec<GraphRow>,
    },
    /// Une opération d'écriture a abouti. `output` est la sortie de git, que
    /// la vue affiche telle quelle : c'est elle qui dit ce qui a été poussé,
    /// avancé ou créé, et la reformuler n'apporterait que des approximations.
    Done {
        worktree: Option<WorktreeId>,
        action: Action,
        output: String,
    },
    Failed {
        worktree: Option<WorktreeId>,
        action: Action,
        message: String,
    },
}

/// Ce que l'utilisateur a demandé, pour formuler le message de résultat et
/// savoir quoi rafraîchir ensuite.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    Open,
    Refresh,
    Stage,
    Unstage,
    Discard,
    Commit,
    Fetch,
    Pull,
    Push,
    Checkout,
    Branch,
    Worktree,
    Diff,
    History,
}

impl Action {
    /// Clé i18n du message affiché en cas de succès.
    pub fn success_key(self) -> &'static str {
        match self {
            Self::Open => "action-open-ok",
            Self::Refresh => "action-refresh-ok",
            Self::Stage => "action-stage-ok",
            Self::Unstage => "action-unstage-ok",
            Self::Discard => "action-discard-ok",
            Self::Commit => "action-commit-ok",
            Self::Fetch => "action-fetch-ok",
            Self::Pull => "action-pull-ok",
            Self::Push => "action-push-ok",
            Self::Checkout => "action-checkout-ok",
            Self::Branch => "action-branch-ok",
            Self::Worktree => "action-worktree-ok",
            Self::Diff => "action-diff-ok",
            Self::History => "action-history-ok",
        }
    }
}
