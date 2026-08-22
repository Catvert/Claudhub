//! The protocol between the interface thread and the workers.
//!
//! Nothing here does any work: these are data. The UI thread sends `Cmd`s, the
//! workers answer with `Evt`s. No git command is launched from a `render` or a
//! click handler — the fastest of them (`git status` on a small repository)
//! already costs several milliseconds, that is, a frame lost on every
//! keystroke.

use std::path::PathBuf;

use crate::git::{
    Branch, Commit, DiffFile, DiffRange, FileDiff, GraphRow, LogRange, Status, Summary, Worktree,
};

/// Identifies the checkout concerned. It is the path that serves as the key
/// everywhere: stable, unique, and also the working directory passed to git.
pub type WorktreeId = PathBuf;

/// A secret that travels inside a command without ending up in a trace.
///
/// The `Debug` is written by hand and masks the value, like `db::Connection`'s:
/// a `Cmd` is logged, a token is not. That is what lets the Sentry token travel
/// in the command rather than being read back by the worker — a remote server's
/// worker does not have our settings to hand.
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
    /// Opens a repository (or a repository's worktree) and enumerates its worktrees.
    OpenRepo(PathBuf),
    /// Opens the path **if** it is a repository, and says nothing otherwise.
    ///
    /// It is `claudhub`'s launch directory: launched from its project, one
    /// expects to find it open there; launched from elsewhere, an error message
    /// would be noise. The check lives here and not in the view — `is_repo`
    /// costs a git subprocess, which a gpui entity constructor is not allowed to
    /// pay.
    OpenIfRepo(PathBuf),
    /// Re-reads the worktrees, state and branches of an already open repository.
    RefreshRepo {
        main: PathBuf,
    },
    /// A checkout's `git status`.
    RefreshStatus {
        worktree: WorktreeId,
    },
    /// Lists a review range's files.
    LoadDiffFiles {
        worktree: WorktreeId,
        range: DiffRange,
    },
    /// A file's diff; `untracked` switches to `--no-index`, git not knowing the
    /// file yet.
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
    /// Summarises the state of several checkouts at once, for the sidebar. A
    /// queue of its own: this sweep must never get in front of the diff just
    /// asked for.
    LoadSummaries {
        worktrees: Vec<WorktreeId>,
    },
    /// Looks for the coding agents running in these checkouts.
    ScanAgents {
        worktrees: Vec<WorktreeId>,
        /// The programs of **all** the agent profiles, of which only the name is
        /// used. The whole list and not just the current profile: an agent
        /// launched from a terminal alongside counts as much, and looking for
        /// only one would see only half of them.
        programs: Vec<String>,
    },
    /// Loads a checkout's history, with its graph layout.
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
    /// Destructive: the changes are lost. Confirmation is the caller's
    /// responsibility, not the worker's.
    Discard {
        worktree: WorktreeId,
        paths: Vec<PathBuf>,
    },
    /// Deletes files git does not track. Separate from `Discard`: it is not a
    /// step back but a deletion, and git keeps no trace of it.
    Delete {
        worktree: WorktreeId,
        paths: Vec<PathBuf>,
    },
    /// Stages (or unstages, with `reverse`) a single hunk.
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
    /// Asks the configured agent for a commit message, from what is staged.
    ///
    /// The command travels inside the message: the worker sometimes runs in
    /// another process — the WSL server — whose settings file is not ours. The
    /// view, for its part, always has the setting to hand.
    SuggestMessage {
        worktree: WorktreeId,
        command: String,
    },
    Fetch {
        worktree: WorktreeId,
    },
    /// A repository's periodic fetch.
    ///
    /// A command separate from `Fetch`, and not only for the queue: this one
    /// **says nothing** when it succeeds. A message every ten minutes in the
    /// status bar announcing that nothing happened would wear out precisely the
    /// place where one looks at what has just happened.
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

    /// Integrates `from` into the checkout's branch.
    ///
    /// `no_ff` forces a merge commit: that is what keeps a trace of the agent's
    /// branch once its work is integrated.
    Merge {
        worktree: WorktreeId,
        from: String,
        no_ff: bool,
    },
    /// Integrates an agent branch into the base, **from the main repository**.
    ///
    /// The worker first checks that the main one is clean and sitting on the
    /// base; otherwise it refuses and says so. It is the only place this check
    /// can be made: the view only knows a checkout's state if it has been
    /// opened, and the main repository is not always open.
    Integrate {
        main: PathBuf,
        branch: String,
        base: String,
        no_ff: bool,
    },
    /// Replays the checkout's branch onto `onto`.
    Rebase {
        worktree: WorktreeId,
        onto: String,
    },
    /// Aborts the operation in progress and returns the checkout to its earlier state.
    AbortPending {
        worktree: WorktreeId,
    },
    /// Resumes the operation in progress, once the conflicts are resolved.
    ResumePending {
        worktree: WorktreeId,
    },
    /// Resolves a conflict by keeping one of the two versions, then stages it.
    ResolveConflict {
        worktree: WorktreeId,
        path: PathBuf,
        /// Ours, in the user's sense: the version of the branch one is on. The
        /// translation into `--ours`/`--theirs` depends on the operation in
        /// progress and happens in the git layer.
        ours: bool,
    },

    // — Sentry ————————————————————————————————————————————————————
    /// A project's issues. **Network queue**: a remote API sometimes takes
    /// several seconds and must not occupy a read worker.
    ///
    /// The token travels as a [`Secret`], whose `Debug` masks the value: a
    /// command is logged, a secret is not. On the worker side, `SENTRY_TOKEN`
    /// keeps priority — the server's environment is authoritative.
    LoadIssues {
        org: String,
        project: String,
        query: String,
        token: Secret,
    },
    /// An issue's most recent event: its stack and its message.
    LoadIssueEvent {
        issue: String,
        token: Secret,
    },

    // — Databases —————————————————————————————————————————————————
    /// A connection's databases.
    ///
    /// **The connection travels whole, password included**, where the Sentry
    /// token is read back by the worker. The reason for that exception is that
    /// the reason for the rule does not apply: `db::Connection` has a `Debug`
    /// written by hand that masks the password, so nothing writes it into a
    /// trace. And the price would be real: there are several connections, one
    /// would have to be named by an id and read back from the settings file,
    /// whose write is deferred by half a second — a connection just typed in
    /// would be queried with what it held before.
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
    /// The columns of every table of a database, in one go.
    ///
    /// It is what makes the filter and the completions possible without
    /// unfolding the tree table by table: one connection, one query, three
    /// hundred tables.
    DbAllColumns {
        connection: crate::db::Connection,
        database: String,
    },
    /// Runs a query and returns one page of results.
    ///
    /// `database` is the console's current database — the one `USE` would
    /// choose. `None` for SQLite, which has only one.
    DbQuery {
        connection: crate::db::Connection,
        database: Option<String>,
        sql: String,
        offset: usize,
        limit: usize,
        /// What is needed to recognise **this** request's answer.
        ///
        /// Comparing the query is not enough: changing page, sorting and loading
        /// more all replay the same text, and the console has to be able to drop
        /// the answer of a gesture another has replaced. A counter that never
        /// goes back says it unambiguously.
        request: u64,
    },
    /// Replays a query and writes its **whole** result into a CSV file.
    ///
    /// The path is chosen by a native picker before the send: a worker asks no
    /// questions.
    DbExport {
        connection: crate::db::Connection,
        database: Option<String>,
        sql: String,
        path: std::path::PathBuf,
    },

    // — Project files —————————————————————————————————————————————
    /// Lists the worktree's files, tracked and new-but-not-ignored.
    ListFiles {
        worktree: WorktreeId,
        ignored: bool,
    },
    /// Reads a file for editing.
    ReadFile {
        worktree: WorktreeId,
        path: PathBuf,
    },
    /// Writes a file, unless its digest has changed since the read.
    WriteFile {
        worktree: WorktreeId,
        path: PathBuf,
        content: String,
        /// Digest of the content read. `None` overwrites without looking —
        /// reserved for a save that has been explicitly confirmed.
        expect: Option<u64>,
    },
    /// Renames, deletes or creates a file or a folder.
    FileOp {
        worktree: WorktreeId,
        op: crate::files::Op,
    },
    /// Reads a worktree's notes folder — one Markdown file per note, plus the
    /// review index.
    ///
    /// The worker only returns text: it is `ui::vault` that knows what a file
    /// contains, and the parsing can be tested without touching the disk.
    ReadNotes {
        worktree: WorktreeId,
        dir: PathBuf,
    },
    /// Aligns the notes folder on what Claudhub holds in memory.
    ///
    /// The list is **exhaustive**: the worker writes what has changed and erases
    /// what carries our mark without appearing in it any more — that is how a
    /// deleted note disappears, and how a file renamed in the vault does not
    /// make a duplicate. What somebody else wrote is never touched.
    WriteNotes {
        worktree: WorktreeId,
        dir: PathBuf,
        files: Vec<(String, String)>,
    },
    /// Writes a vault file, unless it has changed since we read it.
    ///
    /// A command separate from `WriteNotes`, and conditional, because those
    /// files are **not ours**: the worktree's agent ticks things off in
    /// `TODO.md` while we watch. The digest is that of what we had in front of
    /// us; a mismatch means it wrote in the meantime, and writing blind would
    /// erase its work. It is `files::write`'s guard, for the same reason.
    ///
    /// **Empty text erases the file.** In a vault, an empty file cannot be told
    /// from an absent one, and leaving one per opened worktree is precisely what
    /// `notes_on_disk` avoids elsewhere.
    WriteVaultFile {
        worktree: WorktreeId,
        /// The file's full path: it lives in the vault, outside the worktree,
        /// and has no relative path to be given.
        path: PathBuf,
        text: String,
        expect: Option<u64>,
    },
    /// Launches the external editor on a file, at a given line.
    ///
    /// The command template travels here for the same reason as
    /// `SuggestMessage`'s: the settings are the view's, not the worker's.
    OpenExternal {
        worktree: WorktreeId,
        path: PathBuf,
        line: usize,
        editor: String,
    },

    // — File watching —————————————————————————————————————————————
    // Watching lives with the workers and not in the view: it is the worktree's
    // disk it looks at, and that disk is the server's when the workers run in
    // WSL. These four orders go through no queue — `Handle::send` hands them
    // straight to the watcher thread — and what comes back is an
    // [`Evt::FilesChanged`].
    /// Watches a worktree: the folders git knows, without recursion, plus its
    /// `HEAD` and its `index`.
    Watch {
        worktree: WorktreeId,
    },
    Unwatch {
        worktree: WorktreeId,
    },
    /// Watches a folder as it is, without recursion or git: a worktree's notes
    /// vault. No effect while the folder does not exist — the order is to be
    /// sent again after creating it.
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
        /// The checkout the opening was asked from, when it is one. Launching
        /// `claudhub` from a worktree must open *that* worktree, not the main
        /// repository that happens to head the list.
        opened_at: Option<WorktreeId>,
    },
    /// A repository we could not open: the folder has vanished, or it is not one.
    ///
    /// An event of its own rather than a `Failed`: it is not an operation that
    /// failed but a repository that is missing, and what to do with it depends
    /// on how it got there. Remembered, it has to stay **visible** so it can be
    /// removed; named just now in a folder picker, it only has to be said.
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
        /// The repository's integration branch, as git declares it
        /// (`origin/HEAD`, then `init.defaultBranch`, then the usual names that
        /// really exist). `None` on a repository that has none: the branch
        /// review then has nothing to compare itself against.
        default_base: Option<String>,
    },
    Summaries {
        summaries: Vec<(WorktreeId, Summary)>,
    },
    /// The repository has just been fetched without being asked. What it carries
    /// is not a result but an occasion: the ahead and behind counts may have
    /// changed, and they are read from the status.
    Fetched {
        main: PathBuf,
    },
    /// The message the agent proposes for what is staged.
    ///
    /// It carries its worktree because it arrives several seconds after the
    /// request: one may have changed worktree in the meantime, and putting that
    /// message into another's field would be the worst of services.
    CommitMessage {
        worktree: WorktreeId,
        message: String,
    },
    /// What a repository's `wt.toml` declares. `None` when there is none — the
    /// common case, and not an error: `wt`'s gestures simply disappear from the
    /// menu.
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
        /// The subset of `files` that `.gitignore` excludes, sorted. Empty when
        /// they were not asked for.
        ignored: Vec<PathBuf>,
    },
    FileContent {
        worktree: WorktreeId,
        path: PathBuf,
        content: crate::files::Content,
    },
    Agents {
        agents: crate::agent::Agents,
    },
    /// A worktree's vault has just been written.
    ///
    /// The view already holds what it wrote; what this event carries is
    /// something else: the **folder may have just been born** with this file,
    /// and while it did not exist there was nothing to watch. It is the only
    /// moment we know it is there.
    VaultWritten {
        worktree: WorktreeId,
    },
    /// Watched paths have changed — one batch per debounce window of the watcher
    /// (250 ms), which makes it a single event on the wire instead of one per
    /// file of a build.
    ///
    /// The paths are arbitrary: it is the view that attaches them to the open
    /// worktrees, being the only one to know which are open.
    FilesChanged {
        paths: Vec<PathBuf>,
    },

    // — Transport ——————————————————————————————————————————————————
    // Never produced by a worker: it is the remote transport's client
    // (`runtime::remote`) that synthesises them, at the handshake and at the
    // server's death. They go through the event channel because it is the only
    // stream the view drains — a second channel would be a second pump.
    /// The server answered the handshake: what it knows and the view cannot
    /// guess about its own machine.
    ServerHello {
        build: String,
        /// Its launch directory — that is what counts as the "current folder"
        /// when the workers run elsewhere.
        cwd: PathBuf,
        running_under_wsl: bool,
        /// Its `/etc/shells`, for the settings form.
        shells: Vec<String>,
    },
    /// The server has gone — end of stream or unreadable frame. The review stays
    /// on screen but nothing moves any more: the view says so and offers to
    /// relaunch.
    ServerLost {
        message: String,
    },
    /// The notes folder's content: file name and text.
    NotesRead {
        worktree: WorktreeId,
        files: Vec<(String, String)>,
    },
    /// A connection's databases, or what prevented reading them.
    ///
    /// **A `Result` in the event rather than an `Evt::Failed`.** A read failure
    /// belongs to the place in the tree that asked for it — that is where it is
    /// read, under the node just unfolded — and not to the status bar, which
    /// would have replaced it with the next message. It is also what makes the
    /// "in error" row distinct from the "not loaded yet" one.
    DbDatabases {
        /// The connection aimed at, by its key: the settings may have changed
        /// between the request and the answer, and an index would no longer name
        /// the same one.
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
    /// A query's result, and how long it took.
    ///
    /// The duration is measured **in the worker**: from the view it would
    /// include the wait in the queue and the next turn of the event pump, which
    /// would make a one-millisecond query look like a twenty-millisecond one.
    DbRows {
        /// The request these rows answer. See `Cmd::DbQuery`.
        request: u64,
        rows: DbResult<crate::db::Rows>,
        elapsed_ms: u64,
    },
    /// A CSV export succeeded, or did not.
    DbExported {
        path: std::path::PathBuf,
        /// The number of rows written.
        rows: DbResult<u64>,
    },
    History {
        worktree: WorktreeId,
        range: LogRange,
        commits: Vec<Commit>,
        /// One entry per commit, in the same order: the view shows them side by
        /// side and being off by one would make each line point at the wrong one.
        graph: Vec<GraphRow>,
    },
    /// A write operation succeeded. `output` is git's output, which the view
    /// shows as it is: it is what says what was pushed, advanced or created, and
    /// rewording it would only add approximations.
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
