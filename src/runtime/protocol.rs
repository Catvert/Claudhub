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

/// Which view a request of the outside world belongs to.
///
/// A closed list, like the requests themselves: postcard is positional, so what
/// crosses the wire is named here once. Two entries, and adding a third is a
/// change to Claudhub rather than to anything the wire has to guess.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub enum Caller {
    /// The Sentry views: the issue list, and the one opened in the centre.
    Sentry,
    /// The CI view: the runs of the branch, read through `gh`.
    Ci,
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
    /// The unstaged remainder of a partially staged file: index → working
    /// tree, plain `git diff`. What the next commit would leave behind — the
    /// review shows it beside the file's diff so those hunks can still be
    /// added.
    LoadUnstagedDiff {
        worktree: WorktreeId,
        path: PathBuf,
        context: usize,
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
    /// The repository's tags, read from `refs/tags`. Milliseconds: a read like
    /// any other.
    LoadTags {
        main: PathBuf,
    },
    /// The tag names `origin` carries. **The network queue**, because that is a
    /// round trip — and it is asked for, never done on a repaint.
    LoadRemoteTags {
        worktree: WorktreeId,
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
    /// One commit's full message, author and date — the block the diff shows
    /// above the commit's files. Not part of the history list: two thousand
    /// bodies would be read for the one that gets opened.
    LoadCommitDetail {
        worktree: WorktreeId,
        id: String,
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
    /// Rolls the whole worktree back to `HEAD` — index, tracked files and
    /// untracked ones alike. One command and not a `Discard` plus a `Delete`
    /// built from the file list: a file staged as added is in neither, and
    /// three read workers share the queue, so nothing orders two commands.
    /// Destructive: the confirmation is the caller's responsibility.
    RollbackAll {
        worktree: WorktreeId,
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
        /// Push the branch in the same breath, the way PhpStorm's
        /// "Commit and Push" does.
        ///
        /// **A flag and not a second command**, the precedent being
        /// `CreateTag`: the commit is local and the push is a round trip, so
        /// two commands would go into two queues — and nothing orders those.
        /// The push could then leave before the commit existed, and git would
        /// send the branch as it was. `queue_of` reads the flag, which is all
        /// it takes for the pair to travel in the network queue.
        push: bool,
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
    /// Asks GitHub for the latest published Claudhub release, once per
    /// launch. Network queue — an HTTP round trip — and, like `AutoFetch`,
    /// **silent on failure**: offline is a normal day.
    ReleaseCheck,
    Pull {
        worktree: WorktreeId,
    },
    Push {
        worktree: WorktreeId,
        force_with_lease: bool,
    },
    /// Resolves a divergence the way the user chose in the dialog a rejected
    /// push or a refused pull opened: fetch and merge (or rebase onto) the
    /// upstream, then push again when a push is what was being attempted.
    ///
    /// **One command and not a `Pull` then a `Push`**, the precedent being
    /// `Commit { push }`: two commands would go into two queues, and nothing
    /// orders those.
    Reconcile {
        worktree: WorktreeId,
        rebase: bool,
        push: bool,
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
    /// Renames a branch, checked out or not.
    ///
    /// It goes to the main repository and not to a worktree: a branch belongs to
    /// the repository, and the one being renamed is often held by a checkout the
    /// window has never opened.
    RenameBranch {
        main: PathBuf,
        from: String,
        to: String,
    },
    /// Removes a branch from `origin`, leaving the local one alone.
    ///
    /// Two commands and not a flag on `DeleteBranch`, for the reason that split
    /// the tags: they are two regrets with two different costs, and a branch
    /// other people have pulled does not come back.
    DeleteRemoteBranch {
        main: PathBuf,
        name: String,
    },
    /// Publishes one branch, which need not be the one HEAD is on.
    ///
    /// `Push` sends the checkout's own branch and needs no name; this one names
    /// what it sends, which is what makes it usable from a list.
    PushBranch {
        main: PathBuf,
        branch: String,
        force_with_lease: bool,
    },
    /// Fast-forwards a branch onto its upstream, without leaving the branch one
    /// is on.
    UpdateBranch {
        main: PathBuf,
        branch: String,
    },
    /// Creates a tag. Annotated when a message is given — that is git's own
    /// distinction, and the message is what makes it.
    CreateTag {
        worktree: WorktreeId,
        name: String,
        message: Option<String>,
        /// The commit to mark. `None` marks HEAD.
        at: Option<String>,
        /// Push it to `origin` in the same breath.
        ///
        /// **A flag and not a second command**, which is the one place this
        /// module departs from "one gesture, one command": the creation is
        /// local and the push is a round trip, so the two would go into two
        /// different queues — and nothing orders those. The push could then
        /// leave before the tag existed, and git would refuse a tag it had
        /// never heard of. The flag is readable by `queue_of`, which is all it
        /// takes for the whole thing to go into the network queue.
        push: bool,
    },
    /// Removes a tag locally.
    DeleteTag {
        worktree: WorktreeId,
        name: String,
    },
    /// Removes a tag from `origin`, leaving the local one alone. Two commands
    /// and not a flag, because they are two gestures with two different costs:
    /// this one is a push, and a tag other people have pulled does not come
    /// back.
    DeleteRemoteTag {
        worktree: WorktreeId,
        name: String,
    },
    /// Pushes one tag to `origin`, or every tag it does not have.
    PushTag {
        worktree: WorktreeId,
        /// `None` pushes them all.
        name: Option<String>,
    },

    /// The repository's stashes. Keyed by the **main** repository: `refs/stash`
    /// lives in the common `.git`, and the list read from a linked worktree is
    /// the list of the main one, to the entry.
    LoadStashes {
        main: PathBuf,
    },
    /// Puts the checkout's changes aside.
    ///
    /// A checkout's gesture and not a repository's, unlike the read above: what
    /// it takes off the table is *this* working tree.
    StashPush {
        worktree: WorktreeId,
        message: Option<String>,
        /// Take the files git does not know yet. Without it they stay on the
        /// disk, which is the surprise everyone has had once.
        untracked: bool,
        /// Leave what was staged staged.
        keep_index: bool,
    },
    /// Restores a stash into the checkout, keeping it (`pop: false`) or taking
    /// it off the stack.
    ///
    /// `hash` is the commit the panel was showing, and the whole reason the
    /// gesture is safe: `stash@{1}` becomes `stash@{0}` the moment anything
    /// drops the entry above it, and git takes nothing but the name.
    StashRestore {
        worktree: WorktreeId,
        name: String,
        hash: String,
        pop: bool,
        /// Restore what was staged as staged.
        index: bool,
    },
    /// Throws one stash away. `hash` guards the name, as above.
    StashDrop {
        worktree: WorktreeId,
        name: String,
        hash: String,
    },
    /// Creates a branch at the commit the stash was made on and restores it
    /// there — git's own way out of a stash that no longer fits the tree.
    StashBranch {
        worktree: WorktreeId,
        name: String,
        hash: String,
        branch: String,
    },
    /// Empties the stack. Destructive, and confirmed by the caller.
    ///
    /// Sent from a checkout like the other four, though the stack it empties
    /// belongs to the repository: `refs/stash` is shared, and any of its
    /// worktrees is as good a place to run the command from.
    StashClear {
        worktree: WorktreeId,
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
    /// Reads the three versions of a conflicted file out of the index — the
    /// ancestor and the two that grew out of it — for the three-pane merge.
    ///
    /// A command of its own and not a `ReadFile`: what is on disk is the fourth
    /// version, the one with markers through it, and it is the only one that
    /// says nothing about what each side did.
    ReadMerge {
        worktree: WorktreeId,
        path: PathBuf,
    },
    /// Writes a merged file and stages it, which is what marks it resolved.
    ResolveWith {
        worktree: WorktreeId,
        path: PathBuf,
        content: String,
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

    // — Searching the project ——————————————————————————————————————
    /// Searches the whole checkout — `git grep`. See `git::search`.
    ///
    /// **A queue of its own**, and a send id like the SQL console's: a search
    /// is retyped letter by letter, and the answer to a query the next
    /// keystroke has already replaced must be recognisable as stale. A counter
    /// that never goes back says it unambiguously.
    Search {
        worktree: WorktreeId,
        query: crate::git::search::Query,
        request: u64,
    },
    /// Reads a file to **preview** it beside a search result.
    ///
    /// A command of its own and not `ReadFile`: that one opens the editor —
    /// it lands a caret, writes the trail and calls up the editing screen —
    /// and previewing is the opposite gesture, walking the hits without
    /// leaving the list.
    ReadPreview {
        worktree: WorktreeId,
        path: PathBuf,
    },

    // — Project files —————————————————————————————————————————————
    /// Lists the worktree's files, tracked and new-but-not-ignored.
    ListFiles {
        worktree: WorktreeId,
        ignored: bool,
    },
    /// Reads one level of a directory the file listing stopped at.
    ///
    /// Only ever asked of a directory `.gitignore` excludes whole, which is
    /// what makes a `readdir` the right answer rather than a git command: git
    /// does not descend into one, so it has nothing left to say about what is
    /// inside. See `files::read_dir`.
    ReadDir {
        worktree: WorktreeId,
        dir: PathBuf,
    },
    /// Reads a file for editing.
    ReadFile {
        worktree: WorktreeId,
        path: PathBuf,
    },
    /// Reads a file to **look at** it rather than edit it.
    ///
    /// A command of its own and not a flag on `ReadFile`: what comes back is
    /// bytes and not text, and the two answers land in two different halves of
    /// a tab — the editor, or the preview. Which one is asked is decided from
    /// the file name alone (`files::picture_of`), before anything is read.
    ReadImage {
        worktree: WorktreeId,
        path: PathBuf,
    },
    /// Reads what `HEAD` holds for a file, which is what the editor's gutter
    /// compares its buffer against. A command of its own rather than a
    /// `LoadFileDiff`: the diff git computes is against the file **on disk**,
    /// and the buffer the gutter marks is the one under the caret.
    ReadFileBase {
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
        /// An existing branch to check out — local, or `origin/…`, which git
        /// turns into a local one of the short name — or a name imposed on the
        /// new branch. `None`: a new branch named by the project's template.
        branch: Option<String>,
        /// Where a *new* branch starts. `None`: the main repository's HEAD.
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
    /// Reads a worktree's `justfile`: the recipes the run button offers.
    ///
    /// A subprocess (`just --dump`), hence a command and not a file read the
    /// view could do itself. Background queue: nothing on screen waits for it,
    /// and it must never sit in front of a diff.
    JustLoad {
        worktree: WorktreeId,
    },
    /// Lists a worktree's tests — every runner it carries — for the tests
    /// panel.
    ///
    /// A subprocess that boots PHP or node per runner, a second or two on a
    /// real suite: background queue, never in front of a diff.
    TestsLoad {
        worktree: WorktreeId,
    },
    /// Runs what the target names and reads the runner's account.
    ///
    /// Its own queue: a suite is measured in minutes, and neither the
    /// background sweep nor a project's hooks may wait behind it.
    TestsRun {
        worktree: WorktreeId,
        target: crate::suite::Target,
        /// Send id, handed back with the result: a run launched for a
        /// worktree since closed must not paint another's panel.
        id: u64,
    },
    /// Stops every test run up to `id` — the one in flight and its
    /// campaign's queued remainder. **Never queued**: `Handle::send` hands it
    /// straight to the suite module, the watcher's route — queued behind the
    /// run it names, it would arrive after the death it asks for.
    TestsStop {
        id: u64,
    },

    /// Reads the state and the addresses of a project's worktrees.
    ///
    /// These are shell commands, one per worktree and per reading: background
    /// queue only, and a long period.
    WtScan {
        targets: Vec<(PathBuf, WorktreeId)>,
    },
    /// The addresses `[open] source` enumerates for one worktree — one per
    /// tenant, per service… — asked **when the menu opens**, never by the scan:
    /// it is a shell command that may query a database, and the project's own
    /// comment says "resolved on opening, not while listing". Background queue.
    WtLinks {
        main: PathBuf,
        worktree: WorktreeId,
        slug: String,
    },

    // — The language server ————————————————————————————————————
    /// Starts a language server on a worktree, replacing the one running there.
    ///
    /// The declaration travels rather than being read back by the worker: it
    /// comes from the settings, which live on the interface's side, exactly as
    /// the external editor's command and the Sentry token do — and the worker
    /// may well be in another process, whose settings file is not ours.
    LspStart {
        worktree: WorktreeId,
        server: crate::lsp::Server,
    },
    LspStop {
        worktree: WorktreeId,
    },
    LspOpen {
        worktree: WorktreeId,
        path: PathBuf,
        language_id: String,
        text: String,
    },
    LspChange {
        worktree: WorktreeId,
        path: PathBuf,
        text: String,
    },
    LspClose {
        worktree: WorktreeId,
        path: PathBuf,
    },
    LspSave {
        worktree: WorktreeId,
        path: PathBuf,
    },
    /// One LSP request — completion, hover, definition, code actions, semantic
    /// tokens.
    ///
    /// **One variant and not six.** The view already builds the parameters and
    /// reads the result as `lsp_types`; a variant per method would be six pairs
    /// of `Cmd`/`Evt` carrying the same two strings. `params` is JSON because
    /// the core must not depend on `lsp-types`, which belongs to the `ui`
    /// feature, and because postcard cannot carry a `Value` back.
    LspRequest {
        worktree: WorktreeId,
        /// The view's own counter, which never goes back — the same device as
        /// the SQL console's send id, and for the same reason: the answer to a
        /// gesture that has been replaced must be recognisable.
        id: u64,
        method: String,
        params: String,
    },
    /// Abandons a request. A completion is asked on one keystroke and stale on
    /// the next, and a server told nothing goes on computing it.
    LspCancel {
        worktree: WorktreeId,
        id: u64,
    },
    /// What the view did with a `workspace/applyEdit`: a code action's fix
    /// often arrives that way rather than in the action itself, and a server
    /// left without an answer keeps the request open.
    LspApplied {
        worktree: WorktreeId,
        /// The **server's** request id, echoed back.
        id: u64,
        applied: bool,
    },

    // — The world outside the repository —————————————————————————
    /// One request a view makes of a service — see `crate::outside`.
    ///
    /// **One variant and not one per view**: postcard is positional and
    /// `PROTOCOL_VERSION` is announced at the handshake, so what crosses is
    /// versioned once here rather than growing a message per feature.
    ///
    /// The queue is read off the request, not off this variant: an HTTP call is
    /// the network's business, a shell command the background sweep's. See
    /// `runtime::queue_of`.
    Call {
        /// Which view is asking. It is what carries the answer home.
        caller: Caller,
        /// A counter that never goes back: the same device as the SQL console's
        /// send id and the language client's request id.
        call: u64,
        cap: crate::outside::Cap,
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
            Self::LspStart { .. } => "LspStart",
            Self::LspStop { .. } => "LspStop",
            Self::LspOpen { .. } => "LspOpen",
            Self::LspChange { .. } => "LspChange",
            Self::LspClose { .. } => "LspClose",
            Self::LspSave { .. } => "LspSave",
            Self::LspRequest { .. } => "LspRequest",
            Self::LspCancel { .. } => "LspCancel",
            Self::LspApplied { .. } => "LspApplied",
            Self::OpenIfRepo(..) => "OpenIfRepo",
            Self::RefreshRepo { .. } => "RefreshRepo",
            Self::RefreshStatus { .. } => "RefreshStatus",
            Self::LoadDiffFiles { .. } => "LoadDiffFiles",
            Self::LoadUnstagedDiff { .. } => "LoadUnstagedDiff",
            Self::LoadFileDiff { .. } => "LoadFileDiff",
            Self::LoadBranches { .. } => "LoadBranches",
            Self::LoadTags { .. } => "LoadTags",
            Self::LoadRemoteTags { .. } => "LoadRemoteTags",
            Self::LoadSummaries { .. } => "LoadSummaries",
            Self::ScanAgents { .. } => "ScanAgents",
            Self::LoadHistory { .. } => "LoadHistory",
            Self::LoadCommitDetail { .. } => "LoadCommitDetail",
            Self::Stage { .. } => "Stage",
            Self::Unstage { .. } => "Unstage",
            Self::Discard { .. } => "Discard",
            Self::Delete { .. } => "Delete",
            Self::RollbackAll { .. } => "RollbackAll",
            Self::ApplyHunk { .. } => "ApplyHunk",
            Self::Commit { .. } => "Commit",
            Self::SuggestMessage { .. } => "SuggestMessage",
            Self::Fetch { .. } => "Fetch",
            Self::AutoFetch { .. } => "AutoFetch",
            Self::ReleaseCheck => "ReleaseCheck",
            Self::Pull { .. } => "Pull",
            Self::Push { .. } => "Push",
            Self::Reconcile { .. } => "Reconcile",
            Self::Checkout { .. } => "Checkout",
            Self::CreateBranch { .. } => "CreateBranch",
            Self::DeleteBranch { .. } => "DeleteBranch",
            Self::RenameBranch { .. } => "RenameBranch",
            Self::DeleteRemoteBranch { .. } => "DeleteRemoteBranch",
            Self::PushBranch { .. } => "PushBranch",
            Self::UpdateBranch { .. } => "UpdateBranch",
            Self::CreateTag { .. } => "CreateTag",
            Self::DeleteTag { .. } => "DeleteTag",
            Self::DeleteRemoteTag { .. } => "DeleteRemoteTag",
            Self::PushTag { .. } => "PushTag",
            Self::LoadStashes { .. } => "LoadStashes",
            Self::StashPush { .. } => "StashPush",
            Self::StashRestore { .. } => "StashRestore",
            Self::StashDrop { .. } => "StashDrop",
            Self::StashBranch { .. } => "StashBranch",
            Self::StashClear { .. } => "StashClear",
            Self::Merge { .. } => "Merge",
            Self::Integrate { .. } => "Integrate",
            Self::Rebase { .. } => "Rebase",
            Self::AbortPending { .. } => "AbortPending",
            Self::ResumePending { .. } => "ResumePending",
            Self::ReadMerge { .. } => "ReadMerge",
            Self::ResolveWith { .. } => "ResolveWith",
            Self::ResolveConflict { .. } => "ResolveConflict",
            Self::DbDatabases { .. } => "DbDatabases",
            Self::DbTables { .. } => "DbTables",
            Self::DbColumns { .. } => "DbColumns",
            Self::DbAllColumns { .. } => "DbAllColumns",
            Self::DbQuery { .. } => "DbQuery",
            Self::DbExport { .. } => "DbExport",
            Self::Search { .. } => "Search",
            Self::ReadPreview { .. } => "ReadPreview",
            Self::ListFiles { .. } => "ListFiles",
            Self::ReadDir { .. } => "ReadDir",
            Self::ReadFile { .. } => "ReadFile",
            Self::ReadImage { .. } => "ReadImage",
            Self::ReadFileBase { .. } => "ReadFileBase",
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
            Self::JustLoad { .. } => "JustLoad",
            Self::TestsLoad { .. } => "TestsLoad",
            Self::TestsRun { .. } => "TestsRun",
            Self::TestsStop { .. } => "TestsStop",
            Self::WtScan { .. } => "WtScan",
            Self::WtLinks { .. } => "WtLinks",
            Self::Call { .. } => "Call",
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
    /// The unstaged remainder of a partially staged file. It carries the
    /// path for the same reason `CommitDetail` carries its id: the selection
    /// may have moved during the read.
    UnstagedDiff {
        worktree: WorktreeId,
        path: PathBuf,
        diff: FileDiff,
    },
    /// The opened commit's full message, author and date.
    ///
    /// It carries the commit's id because it arrives after a git command: the
    /// selection may have moved on, and this block above another commit's diff
    /// would caption it with the wrong story.
    CommitDetail {
        worktree: WorktreeId,
        id: String,
        author: String,
        date: String,
        /// The raw message, subject line included.
        message: String,
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
    Tags {
        main: PathBuf,
        tags: Vec<crate::git::Tag>,
    },
    /// The tags `origin` has, as a `ls-remote` has just read them.
    ///
    /// Kept apart from `Tags`: what is local is known at every refresh, what is
    /// on the remote only once it has been asked for — and a panel that mixed
    /// the two would say "pushed" about a tag nobody ever pushed.
    RemoteTags {
        main: PathBuf,
        names: Vec<String>,
    },
    /// The repository's stashes, by main repository — the shared `refs/stash`.
    Stashes {
        main: PathBuf,
        stashes: Vec<crate::git::Stash>,
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
    /// The latest published release, when the check reached GitHub. The
    /// interface compares it with its **own** version — in remote mode the
    /// fetcher is the server, and the window is what gets updated.
    ReleaseChecked {
        version: String,
        url: String,
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
    /// What a worktree's `justfile` declares. `None` when there is none, or
    /// when `just` is not installed: the run button disappears, which is the
    /// truth in both cases.
    JustRecipes {
        worktree: WorktreeId,
        recipes: Option<crate::just::Snapshot>,
    },
    /// What a worktree's Pest suite declares — or that there is no Pest, or
    /// that listing failed: the panel shows all three truthfully.
    TestsFound {
        worktree: WorktreeId,
        report: crate::suite::Report,
    },
    /// One line of a running suite, as Pest says it — a run is measured in
    /// minutes, and the panel follows it live, the way `WtProgress` narrates
    /// a hook.
    TestsLine {
        worktree: WorktreeId,
        id: u64,
        line: String,
    },
    /// A run's account — or why there was none. A failing test is a run, not
    /// an error: the error side is the suite that never started.
    TestsRan {
        worktree: WorktreeId,
        id: u64,
        run: std::result::Result<crate::suite::Run, String>,
    },
    WtStates {
        states: Vec<(WorktreeId, WtWorktree)>,
    },
    /// The addresses `[open] source` enumerated for a worktree. The static URL
    /// is not among them: it travels with `WtStates`, and the menu shows it
    /// before this arrives.
    WtLinks {
        worktree: WorktreeId,
        endpoints: Vec<crate::wt::Endpoint>,
    },
    /// One line of a `wt` operation, as it is said — `new`, `up`, `down` and
    /// `rm` run the project's hooks, which are measured in minutes, and the
    /// `Done` or `Failed` that closes the operation arrives only at the end.
    /// `main` and `slug` say which operation: two may be in flight.
    WtProgress {
        main: PathBuf,
        slug: String,
        op: crate::wt::Op,
        line: String,
        warning: bool,
    },
    /// What a service answered — the body, or one sentence of why not. It goes
    /// back to the request that is awaiting it, by `call`.
    Called {
        caller: Caller,
        call: u64,
        result: Result<String, String>,
    },

    /// What a search found, or why it could not run — a bad regular expression
    /// is the common one, and it belongs under the field rather than in a
    /// status bar the next message wipes.
    SearchDone {
        worktree: WorktreeId,
        request: u64,
        result: DbResult<crate::git::search::Results>,
    },
    /// A file read for the search preview.
    Preview {
        worktree: WorktreeId,
        path: PathBuf,
        content: DbResult<crate::files::Content>,
    },

    ProjectFiles {
        worktree: WorktreeId,
        files: Vec<PathBuf>,
        /// The subset of `files` that `.gitignore` excludes, sorted. Empty when
        /// they were not asked for.
        ignored: Vec<PathBuf>,
        /// Those of `ignored` that are directories nobody has looked inside,
        /// sorted. See `git::Files::dirs`.
        dirs: Vec<PathBuf>,
    },
    /// What one level of an excluded directory holds, folders and files apart.
    ///
    /// The folders arrive unexplored in their turn: opening `vendor/` says what
    /// is directly in it, not what a hundred and fifty thousand files under it
    /// are. The error travels in the event rather than in the status bar — a
    /// directory removed since the listing belongs under the chevron that asked.
    DirListed {
        worktree: WorktreeId,
        dir: PathBuf,
        result: Result<(Vec<PathBuf>, Vec<PathBuf>), String>,
    },
    FileContent {
        worktree: WorktreeId,
        path: PathBuf,
        content: crate::files::Content,
    },
    /// An image, as read, for the tab that will paint it.
    ImageContent {
        worktree: WorktreeId,
        path: PathBuf,
        image: crate::files::Image,
    },
    /// The three versions of a conflicted file, or why there are not three.
    ///
    /// The error travels in the event rather than in the status bar: it belongs
    /// under the pane that asked for it — a binary file and a conflict where one
    /// side deleted the file both answer here, and both leave the two buttons of
    /// the conflicts panel as the way out.
    MergeStages {
        worktree: WorktreeId,
        path: PathBuf,
        result: Result<crate::git::Stages, String>,
    },
    /// What `HEAD` holds for a file. `None` for a file git does not track: it
    /// has no base, and every one of its lines is new — which is a different
    /// answer from an empty base, and the gutter says so.
    FileBase {
        worktree: WorktreeId,
        path: PathBuf,
        text: Option<String>,
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

    // — The language server ————————————————————————————————————
    /// The server answered `initialize`: it is up, and this is what it says it
    /// can do.
    ///
    /// The capabilities travel as JSON and are read by the view alone. They are
    /// what decides which providers the editor gets — a hover provider posted
    /// for a server that has none would spend a round trip per pointer rest to
    /// be told nothing — and they carry the semantic-token legend, which only
    /// the server knows.
    LspReady {
        worktree: WorktreeId,
        name: String,
        capabilities: String,
    },
    /// The session is over: stopped, crashed, or never started. `reason` is
    /// `None` when we ended it ourselves.
    ///
    /// A dead server is an event and never a silence — the same rule as
    /// `ServerLost`: the button says so and what was waiting is failed, rather
    /// than a completion that never comes and a spinner that never stops.
    LspStopped {
        worktree: WorktreeId,
        reason: Option<String>,
    },
    /// The answer to one `Cmd::LspRequest`, carrying the view's own id.
    LspAnswer {
        worktree: WorktreeId,
        id: u64,
        /// The `result` member as JSON, or what the server refused with.
        result: Result<String, String>,
    },
    /// A file's diagnostics, pushed by the server whenever it feels like it —
    /// which is the whole point: they arrive for a file an agent has just
    /// written, without anyone asking.
    LspDiagnostics {
        worktree: WorktreeId,
        path: PathBuf,
        /// The `diagnostics` array as JSON.
        diagnostics: String,
    },
    /// The server asks for an edit to be applied (`workspace/applyEdit`).
    ///
    /// A request of the server's and not a notification: it waits for a yes or
    /// a no, and only the view can give one — it holds the buffer, and it is
    /// the only one that knows which file is open. What it cannot apply it
    /// refuses, which the server reads and reports; applying to a file nobody
    /// has open would be writing without the digest that every other write in
    /// Claudhub carries.
    LspApplyEdit {
        worktree: WorktreeId,
        id: u64,
        /// The `WorkspaceEdit` as JSON.
        edit: String,
    },
    /// What the server is busy with, from `$/progress`; `None` when it is done.
    ///
    /// It answers the one question a fast-starting server still raises: why the
    /// completion is thin for the first ten seconds. PHPantom builds its index
    /// in layers and says so here.
    LspBusy {
        worktree: WorktreeId,
        message: Option<String>,
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
        /// The patch restricted to the lines asked for, one per commit and in
        /// the same order — `LogRange::Lines` only, empty everywhere else.
        ///
        /// It comes with the list because it comes from the **same** command:
        /// `git log -L` writes both, and the line range only makes sense in
        /// HEAD's numbering, so it cannot be asked for again commit by commit.
        patches: Vec<FileDiff>,
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
    /// The static `[open] url`, rendered — at most one. What `[open] source`
    /// enumerates is **not** here: it is a shell command, asked by `WtLinks`
    /// when the menu opens.
    pub endpoints: Vec<crate::wt::Endpoint>,
    /// The branch `wt` recorded when it created the worktree. Empty when `wt`
    /// did not create it, or before its first `up`.
    pub branch: String,
    /// The answers remembered from the last `new`/`up` — `db`, `tenants`,
    /// `services`… — raw values: `wt` keeps no labels.
    pub opts: std::collections::BTreeMap<String, String>,
    /// The ports frozen at the first start.
    pub ports: std::collections::BTreeMap<String, u16>,
    /// Each `[status.info]` line, run and trimmed. In the order of the keys as
    /// `wt` holds them — a `BTreeMap` in its config, so alphabetical.
    pub info: Vec<(String, String)>,
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
    /// A commit that pushes: its own action, so that the button that asked for
    /// it is the one that spins, and so that the bar says which of the two
    /// gestures is running.
    CommitPush,
    SuggestMessage,
    Fetch,
    Pull,
    Push,
    Checkout,
    Branch,
    /// Creating or removing a tag, locally.
    Tag,
    /// Pushing a tag, or removing one from the remote: the network's cost, and
    /// its own message.
    PushTag,
    /// Anything done to the stash: putting work aside, restoring it, dropping
    /// it. One action for the six commands, because what the bar has to say is
    /// the same — and the balloon shows git's own account, which is where the
    /// difference actually is.
    Stash,
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
    Notes,
}

impl Action {
    /// The i18n key of the message shown on success.
    /// Every action, for the tests that check each has its messages.
    pub const ALL: [Action; 32] = [
        Action::Refresh,
        Action::Stage,
        Action::Unstage,
        Action::Discard,
        Action::Delete,
        Action::Commit,
        Action::CommitPush,
        Action::SuggestMessage,
        Action::Fetch,
        Action::Pull,
        Action::Push,
        Action::Checkout,
        Action::Branch,
        Action::Tag,
        Action::PushTag,
        Action::Stash,
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
            Self::CommitPush => "running-commit-push",
            Self::SuggestMessage => "running-suggest-message",
            Self::Fetch => "running-fetch",
            Self::Pull => "running-pull",
            Self::Push => "running-push",
            Self::Tag => "running-tag",
            Self::PushTag => "running-push-tag",
            Self::Stash => "running-stash",
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
            Self::CommitPush => "action-commit-push-ok",
            Self::SuggestMessage => "action-suggest-message-ok",
            Self::Fetch => "action-fetch-ok",
            Self::Pull => "action-pull-ok",
            Self::Push => "action-push-ok",
            Self::Checkout => "action-checkout-ok",
            Self::Branch => "action-branch-ok",
            Self::Tag => "action-tag-ok",
            Self::PushTag => "action-push-tag-ok",
            Self::Stash => "action-stash-ok",
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
