//! Le protocole entre le thread d'interface et les workers.
//!
//! Rien ici ne fait de travail : ce sont des données. Le thread UI envoie des
//! `Cmd`, les workers répondent par des `Evt`. Aucune commande git n'est
//! lancée depuis un `render` ou un gestionnaire de clic — la plus rapide
//! (`git status` sur un petit dépôt) coûte déjà plusieurs millisecondes, soit
//! une frame perdue à chaque frappe.

use std::path::PathBuf;

use crate::git::{
    Branch, Commit, DiffFile, DiffRange, FileDiff, GraphRow, LogRange, Status, Summary, Worktree,
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
    /// Résume l'état de plusieurs checkouts d'un coup, pour la barre
    /// latérale. Une file à part : ce balayage ne doit jamais passer devant le
    /// diff qu'on vient de demander.
    LoadSummaries {
        worktrees: Vec<WorktreeId>,
    },
    /// Cherche les agents de codage qui tournent dans ces checkouts.
    ScanAgents {
        worktrees: Vec<WorktreeId>,
        /// Les programmes de **tous** les profils d'agent, dont seul le nom
        /// sert. La liste entière et non le seul profil courant : un agent
        /// lancé depuis un terminal à côté compte autant, et n'en chercher
        /// qu'un n'en verrait qu'un sur deux.
        programs: Vec<String>,
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
    /// Supprime des fichiers que git ne suit pas. Séparé de `Discard` : ce
    /// n'est pas un retour en arrière mais une suppression, et git n'en garde
    /// aucune trace.
    Delete {
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

    /// Intègre `from` dans la branche du checkout.
    ///
    /// `no_ff` force un commit de fusion : c'est ce qui garde une trace de la
    /// branche d'agent une fois son travail intégré.
    Merge {
        worktree: WorktreeId,
        from: String,
        no_ff: bool,
    },
    /// Intègre une branche d'agent dans la base, **depuis le dépôt
    /// principal**.
    ///
    /// Le worker vérifie d'abord que le principal est propre et positionné sur
    /// la base ; sinon il refuse et le dit. C'est le seul endroit où cette
    /// vérification peut se faire : la vue ne connaît l'état d'un checkout que
    /// s'il a été ouvert, et le dépôt principal ne l'est pas toujours.
    Integrate {
        main: PathBuf,
        branch: String,
        base: String,
        no_ff: bool,
    },
    /// Rejoue la branche du checkout sur `onto`.
    Rebase {
        worktree: WorktreeId,
        onto: String,
    },
    /// Abandonne l'opération en cours et rend le checkout à son état d'avant.
    AbortPending {
        worktree: WorktreeId,
    },
    /// Reprend l'opération en cours, une fois les conflits résolus.
    ResumePending {
        worktree: WorktreeId,
    },
    /// Résout un conflit en gardant une des deux versions, puis l'indexe.
    ResolveConflict {
        worktree: WorktreeId,
        path: PathBuf,
        /// La nôtre, au sens de l'utilisateur : la version de la branche où
        /// l'on se trouve. La traduction en `--ours`/`--theirs` dépend de
        /// l'opération en cours et se fait dans la couche git.
        ours: bool,
    },

    // — Sentry ————————————————————————————————————————————————————
    /// Les issues d'un projet. **File réseau** : une API distante met parfois
    /// plusieurs secondes et ne doit pas occuper un worker de lecture.
    ///
    /// Le jeton n'y figure pas : le worker le lit lui-même, dans
    /// `SENTRY_TOKEN` puis dans les réglages. Un secret n'a rien à faire dans
    /// une commande que l'on journalise.
    LoadIssues {
        org: String,
        project: String,
        query: String,
    },
    /// L'événement le plus récent d'une issue : sa pile et son message.
    LoadIssueEvent {
        issue: String,
    },

    // — Fichiers du projet ————————————————————————————————————————
    /// Liste les fichiers du worktree, suivis et nouveaux non ignorés.
    ListFiles {
        worktree: WorktreeId,
        ignored: bool,
    },
    /// Lit un fichier pour l'éditer.
    ReadFile {
        worktree: WorktreeId,
        path: PathBuf,
    },
    /// Écrit un fichier, sauf si son empreinte a changé depuis la lecture.
    WriteFile {
        worktree: WorktreeId,
        path: PathBuf,
        content: String,
        /// Empreinte du contenu lu. `None` écrase sans regarder — réservé à
        /// une sauvegarde qu'on a explicitement confirmée.
        expect: Option<u64>,
    },
    /// Renomme, supprime ou crée un fichier ou un dossier.
    FileOp {
        worktree: WorktreeId,
        op: crate::files::Op,
    },
    /// Lit le dossier de notes d'un worktree — un fichier Markdown par note,
    /// plus l'index de relecture.
    ///
    /// Le worker ne rend que du texte : c'est `ui::vault` qui sait ce qu'un
    /// fichier contient, et l'analyse se teste sans toucher au disque.
    ReadNotes {
        worktree: WorktreeId,
        dir: PathBuf,
    },
    /// Aligne le dossier de notes sur ce que Claudhub a en mémoire.
    ///
    /// La liste est **exhaustive** : le worker écrit ce qui a changé et efface
    /// ce qui porte notre marque sans y figurer plus — c'est ainsi qu'une note
    /// supprimée disparaît, et qu'un fichier renommé dans le coffre ne fait
    /// pas un doublon. Ce qu'un autre a écrit n'est jamais touché.
    WriteNotes {
        worktree: WorktreeId,
        dir: PathBuf,
        files: Vec<(String, String)>,
    },
    /// Écrit un fichier du coffre, sauf s'il a changé depuis qu'on l'a lu.
    ///
    /// Une commande à part de `WriteNotes`, et conditionnelle, parce que ces
    /// fichiers-là ne sont **pas à nous** : l'agent du worktree coche dans
    /// `TODO.md` pendant qu'on le regarde. L'empreinte est celle de ce qu'on
    /// avait sous les yeux ; un écart veut dire qu'il a écrit entre-temps, et
    /// écrire au jugé effacerait son travail. C'est le garde de `files::write`,
    /// pour la même raison.
    ///
    /// **Un texte vide efface le fichier.** Dans un coffre, un fichier vide ne
    /// se distingue pas d'un fichier absent, et en laisser un par worktree
    /// ouvert est précisément ce que `notes_on_disk` évite ailleurs.
    WriteVaultFile {
        worktree: WorktreeId,
        /// Le chemin complet du fichier : il vit dans le coffre, hors du
        /// worktree, et n'a pas de chemin relatif à lui donner.
        path: PathBuf,
        text: String,
        expect: Option<u64>,
    },
    /// Lance l'éditeur externe sur un fichier, à une ligne donnée.
    OpenExternal {
        worktree: WorktreeId,
        path: PathBuf,
        line: usize,
    },

    // — `wt` : ce que le `wt.toml` du projet ajoute ————————————————
    /// Lit le `wt.toml` d'un dépôt. Une lecture de fichier, mais elle a sa
    /// commande : la vue n'a le droit de toucher au disque nulle part.
    WtLoad {
        main: PathBuf,
    },
    /// Les questions du projet qui s'appliquent, compte tenu des réponses déjà
    /// données. Un `[[prompt]]` avec `source` lance un shell : jamais depuis le
    /// thread d'interface.
    WtQuestions {
        main: PathBuf,
        slug: String,
        answers: std::collections::BTreeMap<String, String>,
    },
    /// Crée un worktree avec tout ce que le projet demande : branche selon son
    /// modèle, dossiers, copies, ports, puis `post_new`.
    WtCreate {
        main: PathBuf,
        slug: String,
        from: Option<String>,
        answers: std::collections::BTreeMap<String, String>,
    },
    WtRemove {
        main: PathBuf,
        slug: String,
    },
    WtUp {
        main: PathBuf,
        slug: String,
    },
    WtDown {
        main: PathBuf,
        slug: String,
    },
    /// Prépare une tâche du projet : les commandes sont rendues ici et lancées
    /// par un onglet de terminal, parce qu'elles sont souvent interactives.
    WtTask {
        main: PathBuf,
        worktree: WorktreeId,
        slug: String,
        task: String,
    },
    /// Relève l'état et les adresses des worktrees d'un projet.
    ///
    /// Ce sont des commandes shell, une par worktree et par relevé : file de
    /// fond uniquement, et période longue.
    WtScan {
        targets: Vec<(PathBuf, WorktreeId)>,
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
        /// Lancer `claudhub` depuis un worktree doit ouvrir *ce* worktree, pas le
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
    Summaries {
        summaries: Vec<(WorktreeId, Summary)>,
    },
    /// Ce que le `wt.toml` d'un dépôt déclare. `None` quand il n'y en a pas —
    /// le cas courant, et pas une erreur : les gestes de `wt` disparaissent
    /// simplement du menu.
    WtProject {
        main: PathBuf,
        project: Option<crate::wt::Snapshot>,
    },
    WtQuestions {
        main: PathBuf,
        slug: String,
        /// Les réponses avec lesquelles la question a été posée : le dialogue
        /// les repasse d'un tour à l'autre, et une réponse en retard ne doit
        /// pas écraser un tour plus avancé.
        answers: std::collections::BTreeMap<String, String>,
        questions: Vec<crate::wt::Question>,
    },
    /// Une tâche prête à partir dans un terminal.
    WtTask {
        worktree: WorktreeId,
        task: String,
        launch: crate::wt::Launch,
    },
    WtStates {
        states: Vec<(WorktreeId, WtWorktree)>,
    },
    Issues {
        issues: Vec<crate::sentry::Issue>,
    },
    IssueEvent {
        issue: String,
        event: crate::sentry::Event,
    },
    ProjectFiles {
        worktree: WorktreeId,
        files: Vec<PathBuf>,
    },
    FileContent {
        worktree: WorktreeId,
        path: PathBuf,
        content: crate::files::Content,
    },
    Agents {
        agents: crate::agent::Agents,
    },
    /// Le coffre d'un worktree vient d'être écrit.
    ///
    /// La vue tient déjà ce qu'elle a écrit ; ce que cet événement porte est
    /// autre chose : le **dossier peut venir de naître** avec ce fichier, et
    /// tant qu'il n'existait pas il n'y avait rien à surveiller. C'est le seul
    /// moment où l'on sait qu'il est là.
    VaultWritten {
        worktree: WorktreeId,
    },
    /// Le contenu du dossier de notes : nom de fichier et texte.
    NotesRead {
        worktree: WorktreeId,
        files: Vec<(String, String)>,
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

/// Ce que `wt` sait d'un worktree, et que git ignore.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct WtWorktree {
    /// `None` quand le projet ne déclare pas de `[status] up` : il n'y a alors
    /// rien à démarrer, et afficher « arrêté » serait une information fausse.
    pub up: Option<bool>,
    pub endpoints: Vec<crate::wt::Endpoint>,
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
    Delete,
    Commit,
    Fetch,
    Pull,
    Push,
    Checkout,
    Branch,
    Worktree,
    Diff,
    History,
    Merge,
    Integrate,
    Rebase,
    Abort,
    Resume,
    WtUp,
    WtDown,
    Resolve,
    Read,
    Write,
    FileOp,
    OpenExternal,
    Sentry,
    Notes,
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
            Self::Delete => "action-delete-ok",
            Self::Commit => "action-commit-ok",
            Self::Fetch => "action-fetch-ok",
            Self::Pull => "action-pull-ok",
            Self::Push => "action-push-ok",
            Self::Checkout => "action-checkout-ok",
            Self::Branch => "action-branch-ok",
            Self::Worktree => "action-worktree-ok",
            Self::Diff => "action-diff-ok",
            Self::History => "action-history-ok",
            Self::Merge => "action-merge-ok",
            Self::Integrate => "action-integrate-ok",
            Self::Rebase => "action-rebase-ok",
            Self::Abort => "action-abort-ok",
            Self::Resume => "action-resume-ok",
            Self::WtUp => "action-wt-up-ok",
            Self::WtDown => "action-wt-down-ok",
            Self::Read => "action-read-ok",
            Self::Write => "action-write-ok",
            Self::FileOp => "action-file-op-ok",
            Self::OpenExternal => "action-open-external-ok",
            Self::Sentry => "action-sentry-ok",
            Self::Resolve => "action-resolve-ok",
            Self::Notes => "action-notes-ok",
        }
    }
}
