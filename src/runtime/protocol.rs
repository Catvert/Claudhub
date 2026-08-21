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

/// Un secret qui voyage dans une commande sans finir dans une trace.
///
/// Le `Debug` est écrit à la main et masque la valeur, comme celui de
/// `db::Connection` : `Cmd` se journalise, un jeton non. C'est ce qui permet
/// au jeton Sentry de voyager dans la commande plutôt que d'être relu par le
/// worker — le worker d'un serveur distant n'a pas nos réglages sous la main.
#[derive(Clone, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
pub struct Secret(pub String);

impl std::fmt::Debug for Secret {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(if self.0.is_empty() {
            "Secret(vide)"
        } else {
            "Secret(…)"
        })
    }
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub enum Cmd {
    /// Ouvre un dépôt (ou le worktree d'un dépôt) et énumère ses worktrees.
    OpenRepo(PathBuf),
    /// Ouvre le chemin **si** c'est un dépôt, et ne dit rien sinon.
    ///
    /// C'est le répertoire de lancement de `claudhub` : lancé depuis son
    /// projet, on s'attend à l'y trouver ouvert ; lancé d'ailleurs, un message
    /// d'erreur serait du bruit. La vérification vit ici et non dans la vue —
    /// `is_repo` coûte un sous-processus git, ce qu'un constructeur d'entité
    /// gpui n'a pas le droit de payer.
    OpenIfRepo(PathBuf),
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
    /// Demande un message de commit à l'agent configuré, à partir de ce qui
    /// est indexé.
    ///
    /// La commande voyage dans le message : le worker tourne parfois dans un
    /// autre processus — le serveur WSL —, dont le fichier de réglages n'est
    /// pas le nôtre. La vue, elle, a toujours le réglage sous la main.
    SuggestMessage {
        worktree: WorktreeId,
        command: String,
    },
    Fetch {
        worktree: WorktreeId,
    },
    /// Le fetch périodique d'un dépôt.
    ///
    /// Une commande à part de `Fetch`, et pas seulement pour la file : celui-ci
    /// **ne dit rien** quand il aboutit. Un message toutes les dix minutes dans
    /// la barre d'état pour annoncer qu'il ne s'est rien passé userait
    /// justement l'endroit où l'on regarde ce qui vient de se passer.
    AutoFetch {
        main: PathBuf,
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
    /// Le jeton voyage en [`Secret`], dont le `Debug` masque la valeur : une
    /// commande se journalise, un secret non. Côté worker, `SENTRY_TOKEN`
    /// garde la priorité — c'est l'environnement du serveur qui fait foi.
    LoadIssues {
        org: String,
        project: String,
        query: String,
        token: Secret,
    },
    /// L'événement le plus récent d'une issue : sa pile et son message.
    LoadIssueEvent {
        issue: String,
        token: Secret,
    },

    // — Bases de données —————————————————————————————————————————
    /// Les bases d'une connexion.
    ///
    /// **La connexion voyage entière, mot de passe compris**, là où le jeton
    /// Sentry est relu par le worker. La raison de cette dérogation est que
    /// la raison de la règle ne s'applique pas : `db::Connection` a un `Debug`
    /// écrit à la main qui masque le mot de passe, donc rien ne l'écrit dans
    /// une trace. Et le prix serait réel : il y a plusieurs connexions, il
    /// faudrait en désigner une par un identifiant et la relire depuis le
    /// fichier de réglages, dont l'écriture est différée d'une demi-seconde —
    /// une connexion qu'on vient de saisir serait interrogée avec ce qu'elle
    /// contenait avant.
    DbDatabases {
        connection: crate::db::Connection,
    },
    DbTables {
        connection: crate::db::Connection,
        database: String,
    },
    DbColumns {
        connection: crate::db::Connection,
        database: String,
        table: String,
    },
    /// Les colonnes de toutes les tables d'une base, d'un coup.
    ///
    /// C'est ce qui rend le filtre et les complétions possibles sans déplier
    /// l'arbre table par table : une connexion, une requête, trois cents
    /// tables.
    DbAllColumns {
        connection: crate::db::Connection,
        database: String,
    },
    /// Exécute une requête et rend une page de résultats.
    ///
    /// `database` est la base courante de la console — celle que `USE`
    /// choisirait. `None` pour SQLite, qui n'en a qu'une.
    DbQuery {
        connection: crate::db::Connection,
        database: Option<String>,
        sql: String,
        offset: usize,
        limit: usize,
        /// De quoi reconnaître la réponse de **cet** envoi.
        ///
        /// Comparer la requête ne suffit pas : changer de page, trier et
        /// charger la suite rejouent tous le même texte, et la console doit
        /// pouvoir écarter la réponse d'un geste qu'un autre a remplacé. Un
        /// compteur qui ne recule jamais le dit sans ambiguïté.
        request: u64,
    },
    /// Rejoue une requête et en écrit le résultat **entier** dans un fichier
    /// CSV.
    ///
    /// Le chemin est choisi par un sélecteur natif avant l'envoi : un worker
    /// ne pose pas de question.
    DbExport {
        connection: crate::db::Connection,
        database: Option<String>,
        sql: String,
        path: std::path::PathBuf,
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
    ///
    /// Le modèle de commande voyage ici pour la même raison que celle de
    /// `SuggestMessage` : les réglages sont ceux de la vue, pas du worker.
    OpenExternal {
        worktree: WorktreeId,
        path: PathBuf,
        line: usize,
        editor: String,
    },

    // — Surveillance de fichiers ——————————————————————————————————
    // La surveillance vit avec les workers et non dans la vue : c'est le
    // disque du worktree qu'elle regarde, et ce disque est celui du serveur
    // quand les workers tournent dans WSL. Ces quatre ordres ne passent par
    // aucune file — `Handle::send` les remet directement au thread du
    // surveillant — et ce qui en revient est un [`Evt::FilesChanged`].
    /// Surveille un worktree : les dossiers que git connaît, sans récursion,
    /// plus son `HEAD` et son `index`.
    Watch {
        worktree: WorktreeId,
    },
    Unwatch {
        worktree: WorktreeId,
    },
    /// Surveille un dossier tel quel, sans récursion ni git : le coffre de
    /// notes d'un worktree. Sans effet tant que le dossier n'existe pas —
    /// l'ordre est à renvoyer après l'avoir créé.
    WatchDir {
        dir: PathBuf,
    },
    UnwatchDir {
        dir: PathBuf,
    },

    // — `wt`: what the project's `wt.toml` adds ————————————————————
    /// Reads a repository's `wt.toml`. A file read, but it gets its own command:
    /// the view is not allowed to touch the disk anywhere.
    WtLoad {
        main: PathBuf,
    },
    /// The project's questions that apply, given the answers already provided. A
    /// `[[prompt]]` with `source` launches a shell: never from the interface
    /// thread.
    WtQuestions {
        main: PathBuf,
        slug: String,
        answers: std::collections::BTreeMap<String, String>,
        phase: crate::wt::Phase,
        /// Set when the questions wanted are a task's own — `ask = "task"`
        /// prompts are never asked by a phase.
        task: Option<String>,
        /// Round zero is the first: that is when the worker seeds the answers
        /// from what the worktree remembers.
        round: u64,
    },
    /// Creates a worktree with everything the project asks for: branch following
    /// its template, folders, copies, ports, then `post_new`.
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
        /// The answers to the `ask = "up"` questions, as `--set`s. Empty repeats
        /// the previous start, which is what a project asking nothing does.
        answers: std::collections::BTreeMap<String, String>,
    },
    WtDown {
        main: PathBuf,
        slug: String,
    },
    /// Prepares a project task: the commands are rendered here and launched by a
    /// terminal tab, because they are often interactive.
    WtTask {
        main: PathBuf,
        worktree: WorktreeId,
        slug: String,
        task: String,
        /// The answers to the prompts the task declares. The worker turns them
        /// into its arguments — the order is the task's, and only it knows it.
        answers: std::collections::BTreeMap<String, String>,
    },
    /// Reads the state and the addresses of a project's worktrees.
    ///
    /// These are shell commands, one per worktree and per reading: background
    /// queue only, and a long period.
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

impl Cmd {
    /// The variant's name, for the journal.
    ///
    /// A match and not `Debug`: the name is wanted **without** the payload — a
    /// `WriteFile` carries a whole file, a `Commit` its message — and formatting
    /// the whole thing to keep its first word would be paid on every command.
    /// The exhaustiveness check is what keeps this list honest: a new variant
    /// does not compile until it has been named here.
    pub fn name(&self) -> &'static str {
        match self {
            Self::OpenRepo(..) => "OpenRepo",
            Self::OpenIfRepo(..) => "OpenIfRepo",
            Self::RefreshRepo { .. } => "RefreshRepo",
            Self::RefreshStatus { .. } => "RefreshStatus",
            Self::LoadDiffFiles { .. } => "LoadDiffFiles",
            Self::LoadFileDiff { .. } => "LoadFileDiff",
            Self::LoadBranches { .. } => "LoadBranches",
            Self::LoadSummaries { .. } => "LoadSummaries",
            Self::ScanAgents { .. } => "ScanAgents",
            Self::LoadHistory { .. } => "LoadHistory",
            Self::Stage { .. } => "Stage",
            Self::Unstage { .. } => "Unstage",
            Self::Discard { .. } => "Discard",
            Self::Delete { .. } => "Delete",
            Self::ApplyHunk { .. } => "ApplyHunk",
            Self::Commit { .. } => "Commit",
            Self::SuggestMessage { .. } => "SuggestMessage",
            Self::Fetch { .. } => "Fetch",
            Self::AutoFetch { .. } => "AutoFetch",
            Self::Pull { .. } => "Pull",
            Self::Push { .. } => "Push",
            Self::Checkout { .. } => "Checkout",
            Self::CreateBranch { .. } => "CreateBranch",
            Self::DeleteBranch { .. } => "DeleteBranch",
            Self::Merge { .. } => "Merge",
            Self::Integrate { .. } => "Integrate",
            Self::Rebase { .. } => "Rebase",
            Self::AbortPending { .. } => "AbortPending",
            Self::ResumePending { .. } => "ResumePending",
            Self::ResolveConflict { .. } => "ResolveConflict",
            Self::LoadIssues { .. } => "LoadIssues",
            Self::LoadIssueEvent { .. } => "LoadIssueEvent",
            Self::DbDatabases { .. } => "DbDatabases",
            Self::DbTables { .. } => "DbTables",
            Self::DbColumns { .. } => "DbColumns",
            Self::DbAllColumns { .. } => "DbAllColumns",
            Self::DbQuery { .. } => "DbQuery",
            Self::DbExport { .. } => "DbExport",
            Self::ListFiles { .. } => "ListFiles",
            Self::ReadFile { .. } => "ReadFile",
            Self::WriteFile { .. } => "WriteFile",
            Self::FileOp { .. } => "FileOp",
            Self::ReadNotes { .. } => "ReadNotes",
            Self::WriteNotes { .. } => "WriteNotes",
            Self::WriteVaultFile { .. } => "WriteVaultFile",
            Self::OpenExternal { .. } => "OpenExternal",
            Self::Watch { .. } => "Watch",
            Self::Unwatch { .. } => "Unwatch",
            Self::WatchDir { .. } => "WatchDir",
            Self::UnwatchDir { .. } => "UnwatchDir",
            Self::WtLoad { .. } => "WtLoad",
            Self::WtQuestions { .. } => "WtQuestions",
            Self::WtCreate { .. } => "WtCreate",
            Self::WtRemove { .. } => "WtRemove",
            Self::WtUp { .. } => "WtUp",
            Self::WtDown { .. } => "WtDown",
            Self::WtTask { .. } => "WtTask",
            Self::WtScan { .. } => "WtScan",
            Self::AddWorktree { .. } => "AddWorktree",
            Self::RemoveWorktree { .. } => "RemoveWorktree",
        }
    }
}

/// What a database read returns: the result, or the error message already
/// flattened.
///
/// A `String` and not an `anyhow::Error`: `Evt` is `Clone`, which an `anyhow`
/// error is not, and the view only shows one sentence anyway.
pub type DbResult<T> = std::result::Result<T, String>;

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
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
    /// Un dépôt qu'on n'a pas pu ouvrir : le dossier a disparu, ou ce n'en
    /// est pas un.
    ///
    /// Un événement à lui plutôt qu'un `Failed` : ce n'est pas une opération
    /// qui a échoué mais un dépôt qui manque, et ce qu'il faut en faire dépend
    /// de la façon dont il est arrivé là. Mémorisé, il doit rester **visible**
    /// pour qu'on puisse le retirer ; désigné à l'instant dans un sélecteur de
    /// dossier, il n'a qu'à se dire.
    RepoUnavailable {
        path: PathBuf,
        message: String,
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
    /// Le dépôt vient d'être relevé sans qu'on l'ait demandé. Ce qu'il porte
    /// n'est pas un résultat mais une occasion : l'avance et le retard sur
    /// l'amont ont pu changer, et ils se lisent dans le statut.
    Fetched {
        main: PathBuf,
    },
    /// Le message que l'agent propose pour ce qui est indexé.
    ///
    /// Il porte son worktree parce qu'il arrive plusieurs secondes après la
    /// demande : on a pu changer de worktree entre-temps, et poser ce
    /// message-là dans le champ d'un autre serait le pire des services.
    CommitMessage {
        worktree: WorktreeId,
        message: String,
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
        /// The answers the round was computed with. They come **back** because
        /// the worker may have seeded them: a `wt up` starts from what the
        /// worktree remembers, which is what stops it asking twice, and the view
        /// has no business knowing where `wt` files that.
        answers: std::collections::BTreeMap<String, String>,
        questions: Vec<crate::wt::Question>,
        phase: crate::wt::Phase,
        /// The task the questions belong to, when they are a task's own.
        task: Option<String>,
        /// The round this answers. A counter and not a comparison of the
        /// answers: the worker seeds them, so what comes back is no longer what
        /// went out, and a late round would replace the questions with the wrong
        /// ones.
        round: u64,
    },
    /// A task ready to go into a terminal.
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
    /// Des chemins surveillés ont changé — un lot par fenêtre de
    /// regroupement du surveillant (250 ms), ce qui en fait un seul
    /// événement sur le fil au lieu d'un par fichier d'une compilation.
    ///
    /// Les chemins sont quelconques : c'est la vue qui les rattache aux
    /// worktrees ouverts, elle seule sait lesquels le sont.
    FilesChanged {
        paths: Vec<PathBuf>,
    },

    // — Transport ——————————————————————————————————————————————————
    // Jamais produits par un worker : c'est le client du transport distant
    // (`runtime::remote`) qui les synthétise, à la poignée de main et à la
    // mort du serveur. Ils passent par le canal d'événements parce que c'est
    // le seul flux que la vue draine — un second canal serait une seconde
    // pompe.
    /// Le serveur a répondu à la poignée de main : ce qu'il sait et que la
    /// vue ne peut pas deviner de sa machine à elle.
    ServerHello {
        build: String,
        /// Son répertoire de lancement — c'est lui qui vaut comme « dossier
        /// courant » quand les workers tournent ailleurs.
        cwd: PathBuf,
        running_under_wsl: bool,
        /// Ses `/etc/shells`, pour le formulaire des réglages.
        shells: Vec<String>,
    },
    /// Le serveur est parti — fin de flux ou trame illisible. La revue reste
    /// affichée mais plus rien ne bouge : la vue le dit et propose de
    /// relancer.
    ServerLost {
        message: String,
    },
    /// Le contenu du dossier de notes : nom de fichier et texte.
    NotesRead {
        worktree: WorktreeId,
        files: Vec<(String, String)>,
    },
    /// Les bases d'une connexion, ou ce qui a empêché de les lire.
    ///
    /// **Un `Result` dans l'événement plutôt qu'un `Evt::Failed`.** Un échec
    /// de lecture appartient à l'endroit de l'arbre qui l'a demandé — c'est
    /// là qu'il se lit, sous le nœud qu'on vient de déplier — et non à la
    /// barre d'état, qui l'aurait remplacé par le message suivant. C'est
    /// aussi ce qui rend la ligne « en erreur » distincte de la ligne « pas
    /// encore chargée ».
    DbDatabases {
        /// La connexion visée, par sa clé : les réglages ont pu changer entre
        /// la demande et la réponse, et un indice ne désignerait plus la même.
        key: String,
        databases: DbResult<Vec<crate::db::Database>>,
    },
    DbTables {
        key: String,
        database: String,
        tables: DbResult<Vec<crate::db::Table>>,
    },
    DbColumns {
        key: String,
        database: String,
        table: String,
        columns: DbResult<Vec<crate::db::Column>>,
    },
    DbAllColumns {
        key: String,
        database: String,
        columns: DbResult<std::collections::BTreeMap<String, Vec<crate::db::Column>>>,
    },
    /// Le résultat d'une requête, et le temps qu'elle a mis.
    ///
    /// La durée est mesurée **dans le worker** : depuis la vue, elle
    /// comprendrait l'attente dans la file et le prochain tour de la pompe
    /// d'événements, ce qui ferait passer une requête d'une milliseconde pour
    /// une requête de vingt.
    DbRows {
        /// L'envoi auquel ces lignes répondent. Voir `Cmd::DbQuery`.
        request: u64,
        rows: DbResult<crate::db::Rows>,
        elapsed_ms: u64,
    },
    /// Un export CSV a abouti, ou non.
    DbExported {
        path: std::path::PathBuf,
        /// Le nombre de lignes écrites.
        rows: DbResult<u64>,
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

/// What `wt` knows about a worktree, and git does not.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct WtWorktree {
    /// `None` when the project declares no `[status] up`: there is then nothing
    /// to start, and showing "stopped" would be false information.
    pub up: Option<bool>,
    pub endpoints: Vec<crate::wt::Endpoint>,
}

/// What the user asked for, in order to phrase the result message and know what
/// to refresh next.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub enum Action {
    Refresh,
    Stage,
    Unstage,
    Discard,
    Delete,
    Commit,
    SuggestMessage,
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
    /// The i18n key of the message shown on success.
    /// Every action, for the tests that check each has its messages.
    pub const ALL: [Action; 29] = [
        Action::Refresh,
        Action::Stage,
        Action::Unstage,
        Action::Discard,
        Action::Delete,
        Action::Commit,
        Action::SuggestMessage,
        Action::Fetch,
        Action::Pull,
        Action::Push,
        Action::Checkout,
        Action::Branch,
        Action::Worktree,
        Action::Diff,
        Action::History,
        Action::Merge,
        Action::Integrate,
        Action::Rebase,
        Action::Abort,
        Action::Resume,
        Action::WtUp,
        Action::WtDown,
        Action::Resolve,
        Action::Read,
        Action::Write,
        Action::FileOp,
        Action::OpenExternal,
        Action::Sentry,
        Action::Notes,
    ];

    /// What the status bar says **while** the operation runs.
    ///
    /// A gerund and not the button's tooltip: the bar says what is happening,
    /// not what a click would do. The wildcard is deliberate and not an
    /// oversight — only the operations that last long enough to be worth a line
    /// are named, and a `Stage` finishing in ten milliseconds would show a
    /// message nobody has time to read.
    pub fn running_key(self) -> &'static str {
        match self {
            Self::Commit => "running-commit",
            Self::SuggestMessage => "running-suggest-message",
            Self::Fetch => "running-fetch",
            Self::Pull => "running-pull",
            Self::Push => "running-push",
            Self::Checkout => "running-checkout",
            Self::Merge => "running-merge",
            Self::Integrate => "running-integrate",
            Self::Rebase => "running-rebase",
            Self::Abort => "running-abort",
            Self::Resume => "running-resume",
            Self::WtUp => "running-wt-up",
            Self::WtDown => "running-wt-down",
            Self::Worktree => "running-worktree",
            _ => "running-generic",
        }
    }

    pub fn success_key(self) -> &'static str {
        match self {
            Self::Refresh => "action-refresh-ok",
            Self::Stage => "action-stage-ok",
            Self::Unstage => "action-unstage-ok",
            Self::Discard => "action-discard-ok",
            Self::Delete => "action-delete-ok",
            Self::Commit => "action-commit-ok",
            Self::SuggestMessage => "action-suggest-message-ok",
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

#[cfg(test)]
mod tests {
    use super::*;

    /// The journal names every command, and the name comes from a match: a
    /// variant added without one does not compile, where a name read off `Debug`
    /// would have silently filed it under whatever its payload starts with.
    #[test]
    fn a_command_carries_its_name() {
        assert_eq!(Cmd::OpenRepo(PathBuf::from("/p")).name(), "OpenRepo");
        assert_eq!(
            Cmd::Push {
                worktree: PathBuf::from("/p"),
                force_with_lease: false,
            }
            .name(),
            "Push"
        );
    }

    /// Two actions sharing a message would say "Pushing…" for a pull.
    #[test]
    fn no_two_actions_share_a_success_message() {
        let mut keys: Vec<&str> = Action::ALL.iter().map(|a| a.success_key()).collect();
        keys.sort_unstable();
        let before = keys.len();
        keys.dedup();
        assert_eq!(before, keys.len(), "two actions share a success message");
    }
}
