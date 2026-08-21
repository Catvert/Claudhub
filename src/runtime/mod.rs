//! The git workers.
//!
//! A small group of OS threads consumes the same command channel and answers
//! with events. Threads rather than an async executor because
//! `std::process::Command` blocks anyway: a git command is a `fork`, a wait, and
//! nothing to interleave between the two.
//!
//! Several threads rather than one because a `git fetch` on a slow remote would
//! otherwise freeze the status refresh and the diff display. Git protects the
//! index itself with an `index.lock`; two concurrent writes fail cleanly instead
//! of corrupting each other.

pub mod executor;
pub mod protocol;
pub mod watch;

use std::path::{Path, PathBuf};

use anyhow::Result;

pub mod remote;
pub mod wire;

pub use protocol::{Action, Cmd, Evt, Secret, WorktreeId};

use crate::git::{branch, diff, history, repo, status, DiffRange, LogRange};

/// Workers dedicated to reads: status, diffs, branches, and the local writes,
/// all of which are measured in milliseconds.
const READERS: usize = 3;

/// Workers dedicated to databases. See `is_db`.
const DB_WORKERS: usize = 2;

/// The operations that talk to the network have their own queue.
///
/// A `fetch` on a slow remote, an authentication that waits, a connection that
/// times out: those commands are measured in seconds, sometimes in tens of
/// seconds. Sharing the queue with the reads meant an unlucky `pull` took one
/// worker in three, and three in a row froze the whole interface — no status, no
/// diff, nothing, with nobody able to connect that to the button that triggered
/// it.
fn is_network(cmd: &Cmd) -> bool {
    matches!(
        cmd,
        Cmd::Fetch { .. }
            | Cmd::Pull { .. }
            | Cmd::AutoFetch { .. }
            // An agent writing a message takes ten to thirty seconds: exactly
            // the profile of the commands that made the network move out of the
            // read queue.
            | Cmd::SuggestMessage { .. }
            | Cmd::Push { .. }
            | Cmd::LoadIssues { .. }
            | Cmd::LoadIssueEvent { .. }
    )
}

/// The databases have their own queue.
///
/// Neither the reads' — an unlucky `SELECT` there would take one worker in three
/// and the diff would wait behind it — nor the network's: a thirty-second query
/// would delay a `fetch`, and a slow `fetch` would delay reading a schema. They
/// are two worlds with no reason to cross.
///
/// Two workers, because unfolding a schema asks for several at once — a
/// database's tables, then all its columns — and they wait on a socket, not on a
/// core.
fn is_db(cmd: &Cmd) -> bool {
    matches!(
        cmd,
        Cmd::DbDatabases { .. }
            | Cmd::DbTables { .. }
            | Cmd::DbColumns { .. }
            | Cmd::DbAllColumns { .. }
            | Cmd::DbQuery { .. }
            | Cmd::DbExport { .. }
    )
}

/// The background sweep: each worktree's summary and the search for agents.
///
/// It has its own queue for the same reason as the network: it covers every open
/// worktree, it comes back every few seconds, and it must never get in front of
/// the diff just asked for.
fn is_background(cmd: &Cmd) -> bool {
    matches!(
        cmd,
        Cmd::LoadSummaries { .. } | Cmd::ScanAgents { .. } | Cmd::WtScan { .. }
    )
}

/// The `wt` operations that run the project's hooks.
///
/// They get the network queue, and for the same reason: a `post_new` installing
/// dependencies, an `up` starting containers, that takes minutes. Putting them
/// with the reads would amount to freezing the review for the length of a
/// `composer install`.
fn is_long(cmd: &Cmd) -> bool {
    matches!(
        cmd,
        Cmd::WtCreate { .. } | Cmd::WtRemove { .. } | Cmd::WtUp { .. } | Cmd::WtDown { .. }
    )
}

/// What is needed to talk to the workers, wherever they run.
///
/// `Local` is the normal mode: this process's queues. `Remote` is the wire to a
/// `claudhub-server` — a single channel, and the server sorts them back into its
/// queues on arrival. The same `send` for both: the view's seventy send sites
/// have no business knowing where the workers live.
pub struct Handle {
    inner: HandleInner,
}

enum HandleInner {
    Local {
        reads: async_channel::Sender<Cmd>,
        network: async_channel::Sender<Cmd>,
        background: async_channel::Sender<Cmd>,
        databases: async_channel::Sender<Cmd>,
        /// The fifth channel: watch orders go through no queue — setting up a
        /// watch is already deferred into the watcher thread, and making it wait
        /// behind a diff would make no sense. `None` when watching could not
        /// start: the orders are then dropped, and the review only refreshes by
        /// hand.
        watcher: Option<watch::Watcher>,
    },
    Remote(async_channel::Sender<Cmd>),
    /// No worker at all: the commands are dropped.
    ///
    /// The state before the connection, on Windows, where the workers live in a
    /// distribution that has to be woken first. Dropping is the right behaviour:
    /// queueing them would deliver them all at once on opening, and falling back
    /// to local workers would make `git.exe` work on Windows paths, silently and
    /// wide of the mark.
    ///
    /// **Corollary, and it has been paid for**: what is only asked once, at
    /// startup, does not come back by itself. `ClaudhubApp::backend_ready` is
    /// where that is caught up, and it has to re-send *everything* the window
    /// asked for too early — the list of remembered repositories was missing
    /// there, and a Windows window reopened empty on every launch.
    Pending,
}

impl Handle {
    /// A remote transport's handle: everything goes down the same channel,
    /// towards the thread that writes the frames.
    pub(crate) fn remote(wire: async_channel::Sender<Cmd>) -> Self {
        Self {
            inner: HandleInner::Remote(wire),
        }
    }

    /// A handle with no workers, while waiting for a server to answer.
    pub fn pending() -> Self {
        Self {
            inner: HandleInner::Pending,
        }
    }

    /// Sends a command into the queue that suits it. Failure (closed channel)
    /// only happens at shutdown — or, remotely, at the server's death, which the
    /// view learns through `Evt::ServerLost`: nothing to say here.
    pub fn send(&self, cmd: Cmd) {
        match &self.inner {
            HandleInner::Local {
                reads,
                network,
                background,
                databases,
                watcher,
            } => {
                let Some(cmd) = route_watch(watcher.as_ref(), cmd) else {
                    return;
                };
                let queue = if is_network(&cmd) || is_long(&cmd) {
                    network
                } else if is_background(&cmd) {
                    background
                } else if is_db(&cmd) {
                    databases
                } else {
                    reads
                };
                send_to(queue, cmd);
            }
            HandleInner::Remote(wire) => send_to(wire, cmd),
            HandleInner::Pending => log::debug!("command issued before the server, dropped"),
        }
    }
}

fn send_to(queue: &async_channel::Sender<Cmd>, cmd: Cmd) {
    if let Err(err) = queue.try_send(cmd) {
        log::debug!("command dropped: {err}");
    }
}

/// Hands the watch orders to the watcher and returns the others.
fn route_watch(watcher: Option<&watch::Watcher>, cmd: Cmd) -> Option<Cmd> {
    match cmd {
        Cmd::Watch { worktree } => watcher?.watch(&worktree),
        Cmd::Unwatch { worktree } => watcher?.unwatch(&worktree),
        Cmd::WatchDir { dir } => watcher?.watch_dir(&dir),
        Cmd::UnwatchDir { dir } => watcher?.unwatch_dir(&dir),
        other => return Some(other),
    }
    None
}

/// Starts the workers and returns what is needed to talk and listen to them.
pub fn spawn() -> (Handle, async_channel::Receiver<Evt>) {
    let (read_tx, read_rx) = async_channel::unbounded::<Cmd>();
    let (net_tx, net_rx) = async_channel::unbounded::<Cmd>();
    let (bg_tx, bg_rx) = async_channel::unbounded::<Cmd>();
    let (db_tx, db_rx) = async_channel::unbounded::<Cmd>();
    let (evt_tx, evt_rx) = async_channel::unbounded::<Evt>();

    for n in 0..READERS {
        worker(format!("claudhub-git-{n}"), read_rx.clone(), evt_tx.clone());
    }
    // Only one for the network: two simultaneous `fetch`es on the same
    // repository would fight over the reference lock without speeding anything up.
    worker("claudhub-git-net".into(), net_rx, evt_tx.clone());
    worker("claudhub-scan".into(), bg_rx, evt_tx.clone());
    for n in 0..DB_WORKERS {
        worker(format!("claudhub-db-{n}"), db_rx.clone(), evt_tx.clone());
    }

    // File watching lives here and not in the view: its batches become
    // `Evt::FilesChanged` on the same channel as everything else — a single
    // stream to push over a wire, local or remote.
    let watcher = match watch::Watcher::new() {
        Ok((watcher, changes)) => {
            let forward = evt_tx.clone();
            let spawned = std::thread::Builder::new()
                .name("claudhub-watch-evt".into())
                .spawn(move || {
                    while let Ok(paths) = changes.recv_blocking() {
                        if forward.send_blocking(Evt::FilesChanged { paths }).is_err() {
                            return;
                        }
                    }
                });
            if let Err(e) = spawned {
                log::warn!("relais de surveillance indisponible : {e:#}");
            }
            Some(watcher)
        }
        Err(e) => {
            log::warn!("surveillance des fichiers indisponible : {e:#}");
            None
        }
    };
    drop(evt_tx);

    (
        Handle {
            inner: HandleInner::Local {
                reads: read_tx,
                network: net_tx,
                background: bg_tx,
                databases: db_tx,
                watcher,
            },
        },
        evt_rx,
    )
}

fn worker(
    name: String,
    commands: async_channel::Receiver<Cmd>,
    events: async_channel::Sender<Evt>,
) {
    std::thread::Builder::new()
        .name(name)
        .spawn(move || {
            while let Ok(cmd) = commands.recv_blocking() {
                for evt in handle(cmd) {
                    if events.send_blocking(evt).is_err() {
                        return; // the window is gone
                    }
                }
            }
        })
        .expect("the system refuses to create a thread");
}

/// Runs a command and returns the events to publish, and says so in the journal.
///
/// Every command, at `debug`: with several workers on four queues, the order
/// things really happen in is not the order they were asked for, and a journal
/// that only carried the results would not say what was waiting behind what.
/// Writes are louder — `info`, with their duration — and say what they did one
/// floor down, in `done` and `fail`.
fn handle(cmd: Cmd) -> Vec<Evt> {
    let name = cmd.name();
    let started = std::time::Instant::now();
    let evts = dispatch(cmd);
    let elapsed = crate::logging::ms(started.elapsed());
    // A command that produced a `Done` or a `Failed` is a **write**: that is
    // what those two events mean, and it saves classifying sixty variants a
    // second time. It is said at `info`, with how long it took — "why did that
    // push take forty seconds" is a question the journal has to answer without
    // being reconfigured first.
    if evts
        .iter()
        .any(|evt| matches!(evt, Evt::Done { .. } | Evt::Failed { .. }))
    {
        log::info!("{name} — {elapsed}");
    } else {
        log::debug!("{name} — {elapsed} ({} event(s))", evts.len());
    }
    evts
}

/// Where the events come from.
///
/// **One `match` and one arm per variant**, each delegating to a named
/// function: the exhaustiveness check is what tells whoever adds a `Cmd` that
/// they have a worker to write, and grouping the arms behind fallible
/// dispatchers — "not mine, here it is back" — would trade that guarantee for
/// shorter functions. What the arms delegate to lives below, so this stays a
/// dispatch table one can read end to end.
///
/// Returning a `Vec` rather than emitting as it goes keeps this function pure
/// and testable: it does not know the channel.
fn dispatch(cmd: Cmd) -> Vec<Evt> {
    match cmd {
        Cmd::OpenRepo(path) => open(path),
        // The launch directory: opened if it is a repository, silence otherwise
        // — an error message for a `claudhub` launched from `~` would be noise.
        Cmd::OpenIfRepo(path) => {
            if repo::is_repo(&path) {
                open(path)
            } else {
                Vec::new()
            }
        }
        // Handed to the watcher by `Handle::send` before any queue: if one
        // arrives here, a transport has sent them down a path that does not
        // route them yet.
        Cmd::Watch { .. } | Cmd::Unwatch { .. } | Cmd::WatchDir { .. } | Cmd::UnwatchDir { .. } => {
            log::debug!("watch order arrived in a worker");
            Vec::new()
        }

        // — Reads ——————————————————————————————————————————————————————
        Cmd::RefreshRepo { main } => match (repo::Repo { main: main.clone() }).worktrees() {
            Ok(worktrees) => vec![Evt::Worktrees { main, worktrees }],
            Err(e) => vec![fail(None, Action::Refresh, e)],
        },
        Cmd::RefreshStatus { worktree } => match status::status(&worktree) {
            Ok(status) => vec![Evt::Status { worktree, status }],
            Err(e) => vec![fail(Some(worktree), Action::Refresh, e)],
        },
        Cmd::LoadDiffFiles { worktree, range } => match diff::files(&worktree, &range) {
            Ok(files) => vec![Evt::DiffFiles {
                worktree,
                range,
                files,
            }],
            Err(e) => vec![fail(Some(worktree), Action::Diff, e)],
        },
        Cmd::LoadFileDiff {
            worktree,
            range,
            path,
            context,
            untracked,
        } => file_diff(worktree, range, path, context, untracked),
        Cmd::LoadHistory {
            worktree,
            range,
            limit,
        } => history(worktree, range, limit),
        Cmd::LoadSummaries { worktrees } => summaries(worktrees),
        Cmd::ScanAgents {
            worktrees,
            programs,
        } => vec![Evt::Agents {
            agents: crate::agent::scan(&worktrees, &programs),
        }],
        Cmd::LoadBranches { main } => match branch::list(&main) {
            Ok(branches) => vec![branches_evt(main, branches)],
            Err(e) => vec![fail(None, Action::Branch, e)],
        },

        // — Writes ——————————————————————————————————————————————————————
        Cmd::Stage { worktree, paths } => write_then_refresh(worktree, Action::Stage, |dir| {
            repo::stage(dir, &paths).map(|_| String::new())
        }),
        Cmd::Unstage { worktree, paths } => write_then_refresh(worktree, Action::Unstage, |dir| {
            repo::unstage(dir, &paths).map(|_| String::new())
        }),
        Cmd::Discard { worktree, paths } => write_then_refresh(worktree, Action::Discard, |dir| {
            repo::discard(dir, &paths).map(|_| String::new())
        }),
        Cmd::Delete { worktree, paths } => write_then_refresh(worktree, Action::Delete, |dir| {
            repo::clean(dir, &paths).map(|_| String::new())
        }),
        Cmd::ApplyHunk {
            worktree,
            patch,
            reverse,
        } => {
            let action = if reverse {
                Action::Unstage
            } else {
                Action::Stage
            };
            write_then_refresh(worktree, action, |dir| {
                repo::apply_patch(dir, &patch, reverse).map(|_| String::new())
            })
        }
        Cmd::Commit {
            worktree,
            message,
            amend,
            all,
        } => write_then_refresh(worktree, Action::Commit, |dir| {
            repo::commit(
                dir,
                repo::CommitOptions {
                    message: &message,
                    amend,
                    all,
                },
            )
        }),
        Cmd::SuggestMessage { worktree, command } => {
            match crate::commit_msg::suggest(&worktree, &command) {
                Ok(message) => vec![Evt::CommitMessage { worktree, message }],
                Err(e) => vec![fail(Some(worktree), Action::SuggestMessage, e)],
            }
        }
        Cmd::AutoFetch { main } => auto_fetch(main),
        Cmd::Fetch { worktree } => {
            write_then_refresh(worktree, Action::Fetch, |dir| repo::fetch(dir, true))
        }
        Cmd::Pull { worktree } => write_then_refresh(worktree, Action::Pull, repo::pull),
        Cmd::Push {
            worktree,
            force_with_lease,
        } => write_then_refresh(worktree, Action::Push, |dir| {
            repo::push(dir, force_with_lease)
        }),
        Cmd::Checkout { worktree, branch } => {
            write_then_refresh(worktree, Action::Checkout, |dir| {
                repo::checkout(dir, &branch).map(|_| String::new())
            })
        }
        Cmd::CreateBranch {
            worktree,
            name,
            from,
        } => write_then_refresh(worktree, Action::Branch, |dir| {
            repo::create_branch(dir, &name, from.as_deref()).map(|_| String::new())
        }),
        Cmd::DeleteBranch { main, name, force } => delete_branch(main, name, force),
        Cmd::Merge {
            worktree,
            from,
            no_ff,
        } => write_then_refresh(worktree, Action::Merge, |dir| {
            repo::merge(dir, &from, no_ff)
        }),
        Cmd::Integrate {
            main,
            branch,
            base,
            no_ff,
        } => write_then_refresh(main, Action::Integrate, |dir| {
            integrate(dir, &branch, &base, no_ff)
        }),
        Cmd::Rebase { worktree, onto } => {
            write_then_refresh(worktree, Action::Rebase, |dir| repo::rebase(dir, &onto))
        }
        Cmd::AbortPending { worktree } => write_then_refresh(worktree, Action::Abort, repo::abort),
        Cmd::ResumePending { worktree } => {
            write_then_refresh(worktree, Action::Resume, repo::resume)
        }
        Cmd::ResolveConflict {
            worktree,
            path,
            ours,
        } => write_then_refresh(worktree, Action::Resolve, |dir| {
            repo::resolve(dir, &path, ours).map(|_| String::new())
        }),

        // — Sentry ——————————————————————————————————————————————————————
        Cmd::LoadIssues {
            org,
            project,
            query,
            token,
        } => with_sentry_token(token, |token| {
            match crate::sentry::issues(&org, &project, &query, token) {
                Ok(issues) => vec![Evt::Issues { issues }],
                Err(e) => vec![fail(None, Action::Sentry, e)],
            }
        }),
        Cmd::LoadIssueEvent { issue, token } => with_sentry_token(token, |token| {
            match crate::sentry::latest_event(&issue, token) {
                Ok(event) => vec![Evt::IssueEvent { issue, event }],
                Err(e) => vec![fail(None, Action::Sentry, e)],
            }
        }),

        // — Databases ———————————————————————————————————————————————————
        Cmd::DbDatabases { connection } => vec![Evt::DbDatabases {
            key: connection.key(),
            databases: db_result(executor::block_on(crate::db::databases(&connection))),
        }],
        Cmd::DbTables {
            connection,
            database,
        } => vec![Evt::DbTables {
            key: connection.key(),
            tables: db_result(executor::block_on(crate::db::tables(
                &connection,
                &database,
            ))),
            database,
        }],
        Cmd::DbColumns {
            connection,
            database,
            table,
        } => vec![Evt::DbColumns {
            key: connection.key(),
            columns: db_result(executor::block_on(crate::db::columns(
                &connection,
                &database,
                &table,
            ))),
            database,
            table,
        }],
        Cmd::DbAllColumns {
            connection,
            database,
        } => vec![Evt::DbAllColumns {
            key: connection.key(),
            columns: db_result(executor::block_on(crate::db::all_columns(
                &connection,
                &database,
            ))),
            database,
        }],
        Cmd::DbQuery {
            connection,
            database,
            sql,
            offset,
            limit,
            request,
        } => db_query(connection, database, sql, offset, limit, request),
        Cmd::DbExport {
            connection,
            database,
            sql,
            path,
        } => {
            let rows = db_result(executor::block_on(crate::db::export_csv(
                &connection,
                database.as_deref(),
                &sql,
                &path,
            )));
            vec![Evt::DbExported { path, rows }]
        }

        // — Project files ———————————————————————————————————————————————
        Cmd::ListFiles { worktree, ignored } => match repo::list_files(&worktree, ignored) {
            Ok(files) => vec![Evt::ProjectFiles { worktree, files }],
            Err(e) => vec![fail(Some(worktree), Action::Read, e)],
        },
        Cmd::ReadFile { worktree, path } => match crate::files::read(&worktree, &path) {
            Ok(content) => vec![Evt::FileContent {
                worktree,
                path,
                content,
            }],
            Err(e) => vec![fail(Some(worktree), Action::Read, e)],
        },
        Cmd::WriteFile {
            worktree,
            path,
            content,
            expect,
        } => write_then_refresh(worktree, Action::Write, |dir| {
            crate::files::write(dir, &path, &content, expect).map(|_| String::new())
        }),
        Cmd::FileOp { worktree, op } => write_then_refresh(worktree, Action::FileOp, |dir| {
            crate::files::apply(dir, &op).map(|_| op.target().display().to_string())
        }),
        Cmd::ReadNotes { worktree, dir } => match crate::files::read_notes(&dir) {
            Ok(files) => vec![Evt::NotesRead { worktree, files }],
            Err(e) => vec![fail(Some(worktree), Action::Notes, e)],
        },
        // The answer does not carry the content — the view already holds what it
        // has just written — but the fact that the folder exists: it may have
        // just been born with this file, and until then there was nothing to
        // watch.
        Cmd::WriteNotes {
            worktree,
            dir,
            files,
        } => vault_written(worktree, crate::files::sync_notes(&dir, &files)),
        Cmd::WriteVaultFile {
            worktree,
            path,
            text,
            expect,
        } => vault_written(
            worktree,
            crate::files::write_vault_file(&path, &text, expect),
        ),
        Cmd::OpenExternal {
            worktree,
            path,
            line,
            editor,
        } => match crate::files::open_external(&worktree, &editor, &path, line) {
            Ok(program) => vec![done(Some(worktree), Action::OpenExternal, program)],
            Err(e) => vec![fail(Some(worktree), Action::OpenExternal, e)],
        },

        // — `wt` and the worktrees ——————————————————————————————————————
        Cmd::WtLoad { main } => {
            let project = crate::wt::snapshot(&main);
            vec![Evt::WtProject { main, project }]
        }
        Cmd::WtQuestions {
            main,
            slug,
            answers,
            phase,
            task,
            round,
        } => wt_questions(main, slug, answers, phase, task, round),
        Cmd::WtCreate {
            main,
            slug,
            from,
            answers,
        } => {
            let created = crate::wt::create(&main, &slug, from.as_deref(), &answers);
            worktrees_changed(main, created.map(|(_, output)| output))
        }
        Cmd::WtRemove { main, slug } => {
            let removed = crate::wt::remove(&main, &slug);
            worktrees_changed(main, removed)
        }
        Cmd::WtUp {
            main,
            slug,
            answers,
        } => match crate::wt::up(&main, &slug, &answers) {
            Ok(output) => vec![done(None, Action::WtUp, output)],
            Err(e) => vec![fail(None, Action::WtUp, e)],
        },
        Cmd::WtDown { main, slug } => match crate::wt::down(&main, &slug) {
            Ok(output) => vec![done(None, Action::WtDown, output)],
            Err(e) => vec![fail(None, Action::WtDown, e)],
        },
        Cmd::WtTask {
            main,
            worktree,
            slug,
            task,
            answers,
        } => match crate::wt::task(&main, &slug, &task, &answers) {
            Ok(launch) => vec![Evt::WtTask {
                worktree,
                task,
                launch,
            }],
            Err(e) => vec![fail(Some(worktree), Action::Worktree, e)],
        },
        Cmd::WtScan { targets } => wt_scan(targets),
        Cmd::AddWorktree {
            main,
            path,
            branch,
            from,
        } => {
            let r = repo::Repo { main: main.clone() };
            let added = r
                .add_worktree(&path, &branch, from.as_deref())
                .map(|()| path.display().to_string());
            worktrees_changed(main, added)
        }
        Cmd::RemoveWorktree { main, path, force } => {
            let r = repo::Repo { main: main.clone() };
            let removed = r
                .remove_worktree(&path, force)
                .map(|()| path.display().to_string());
            worktrees_changed(main, removed)
        }
    }
}

/// Opens a repository, or says why it could not be opened.
fn open(path: PathBuf) -> Vec<Evt> {
    match open_repo(&path) {
        Ok(evt) => vec![evt],
        Err(e) => vec![Evt::RepoUnavailable {
            path,
            message: describe_error(e),
        }],
    }
}

/// A file's diff. `untracked` switches to `--no-index`: git does not know the
/// file yet, and `diff` alone would return nothing.
fn file_diff(
    worktree: PathBuf,
    range: DiffRange,
    path: PathBuf,
    context: usize,
    untracked: bool,
) -> Vec<Evt> {
    let result = if untracked {
        diff::untracked_file(&worktree, &path)
    } else {
        diff::file(&worktree, &range, &path, context)
    };
    match result {
        Ok(diff) => vec![Evt::FileDiff {
            worktree,
            path,
            diff,
        }],
        Err(e) => vec![fail(Some(worktree), Action::Diff, e)],
    }
}

/// The history, and the graph layout that goes with it: the view shows them
/// side by side, so they are computed together and travel together.
fn history(worktree: PathBuf, range: LogRange, limit: usize) -> Vec<Evt> {
    match history::commits(&worktree, &range, limit) {
        Ok(commits) => {
            let graph = history::layout(&commits);
            vec![Evt::History {
                worktree,
                range,
                commits,
                graph,
            }]
        }
        Err(e) => vec![fail(Some(worktree), Action::History, e)],
    }
}

/// A summary per worktree, for the sidebar.
fn summaries(worktrees: Vec<PathBuf>) -> Vec<Evt> {
    let summaries = worktrees
        .into_iter()
        .filter_map(|worktree| {
            // A worktree deleted under our feet is not an error to display: it
            // will disappear from the list at the next `git worktree list`.
            status::summary(&worktree)
                .ok()
                .map(|summary| (worktree, summary))
        })
        .collect();
    vec![Evt::Summaries { summaries }]
}

/// The branch list, with the base git suggests for it.
fn branches_evt(main: PathBuf, branches: Vec<crate::git::Branch>) -> Evt {
    let default_base = branch::default_base(&main);
    Evt::Branches {
        main,
        branches,
        default_base,
    }
}

/// Deletes a branch and re-reads the list: the panel shows it, and nothing else
/// would tell it the branch has gone.
fn delete_branch(main: PathBuf, name: String, force: bool) -> Vec<Evt> {
    match repo::delete_branch(&main, &name, force) {
        Ok(()) => {
            let mut evts = vec![done(None, Action::Branch, String::new())];
            evts.extend(
                branch::list(&main)
                    .ok()
                    .map(|list| branches_evt(main, list)),
            );
            evts
        }
        Err(e) => vec![fail(None, Action::Branch, e)],
    }
}

/// The periodic fetch. Silence when it succeeds, and a trace when it does not.
///
/// A repository with no remote, an offline machine, a missing authentication:
/// none of that happened at the moment the user was looking, and telling them
/// would amount to interrupting them over a command they did not run.
fn auto_fetch(main: PathBuf) -> Vec<Evt> {
    match repo::fetch(&main, true) {
        Ok(_) => vec![Evt::Fetched { main }],
        Err(e) => {
            log::debug!("automatic fetch of {}: {e}", main.display());
            Vec::new()
        }
    }
}

/// Runs `f` with the Sentry token, or reports that there is none.
///
/// `SENTRY_TOKEN` wins over what the command carries: the worker sometimes runs
/// in another process, and that process's environment is the authority.
fn with_sentry_token(token: protocol::Secret, f: impl FnOnce(&str) -> Vec<Evt>) -> Vec<Evt> {
    match crate::sentry::token(&token.0) {
        Some(token) => f(&token),
        None => vec![fail(
            None,
            Action::Sentry,
            anyhow::anyhow!("no Sentry token: SENTRY_TOKEN or the settings"),
        )],
    }
}

/// One page of a query's result, and how long it took.
fn db_query(
    connection: crate::db::Connection,
    database: Option<String>,
    sql: String,
    offset: usize,
    limit: usize,
    request: u64,
) -> Vec<Evt> {
    // Measured here: from the view, the duration would include the wait in the
    // queue and the next turn of the event pump.
    let started = std::time::Instant::now();
    let rows = db_result(executor::block_on(crate::db::query(
        &connection,
        database.as_deref(),
        &sql,
        offset,
        limit,
    )));
    vec![Evt::DbRows {
        request,
        rows,
        elapsed_ms: started.elapsed().as_millis() as u64,
    }]
}

/// A vault write. The answer carries the worktree and nothing else — see
/// [`Evt::VaultWritten`].
fn vault_written(worktree: PathBuf, result: Result<()>) -> Vec<Evt> {
    match result {
        Ok(()) => vec![Evt::VaultWritten { worktree }],
        Err(e) => vec![fail(None, Action::Notes, e)],
    }
}

/// What `wt` knows about each worktree of a project.
fn wt_scan(targets: Vec<(PathBuf, PathBuf)>) -> Vec<Evt> {
    let states = targets
        .into_iter()
        .filter_map(|(main, worktree)| {
            let slug = crate::wt::slug_of(&main, &worktree)?;
            Some((
                worktree,
                protocol::WtWorktree {
                    up: crate::wt::is_up(&main, &slug),
                    endpoints: crate::wt::endpoints(&main, &slug),
                },
            ))
        })
        .collect();
    vec![Evt::WtStates { states }]
}

/// An operation that added or removed a worktree: the list has to be re-read,
/// and it is the only thing that says the operation really landed.
/// One round of a project's questions.
///
/// **The seeding happens here and only on the first round.** A `wt up` starts
/// from what the worktree remembers — that is what stops it asking for its
/// tenants a second time — and where `wt` files that is not the view's
/// business. The answers therefore go back with the questions, and the view
/// adopts them.
fn wt_questions(
    main: PathBuf,
    slug: String,
    answers: std::collections::BTreeMap<String, String>,
    phase: crate::wt::Phase,
    task: Option<String>,
    round: u64,
) -> Vec<Evt> {
    let answers = if round == 0 && phase == crate::wt::Phase::Up {
        let mut seeded = crate::wt::saved_answers(&main, &slug);
        seeded.extend(answers);
        seeded
    } else {
        answers
    };
    match crate::wt::questions(&main, &slug, &answers, phase, task.as_deref()) {
        Ok(questions) => vec![Evt::WtQuestions {
            main,
            slug,
            answers,
            questions,
            phase,
            task,
            round,
        }],
        Err(e) => vec![fail(None, Action::Worktree, e)],
    }
}

fn worktrees_changed(main: PathBuf, result: Result<String>) -> Vec<Evt> {
    match result {
        Ok(output) => worktree_changed(&repo::Repo { main }, output),
        Err(e) => vec![fail(None, Action::Worktree, e)],
    }
}

/// Integrates a branch into the base, from the main repository.
///
/// The two preliminary checks are not caution on principle: merging into a
/// dirty checkout mixes the changes in progress with the integrated work, and
/// merging while sitting on another branch writes into that one — two kinds of
/// damage discovered after the fact, and a message avoids both.
fn integrate(main: &Path, branch: &str, base: &str, no_ff: bool) -> Result<String> {
    if repo::is_dirty(main) {
        anyhow::bail!(
            "the main repository has changes in progress: commit or stash them before integrating"
        );
    }
    let current = branch::current(main);
    if current.as_deref() != Some(base) {
        anyhow::bail!(
            "the main repository is on \"{}\" and not on \"{base}\"",
            current.as_deref().unwrap_or("detached HEAD")
        );
    }
    repo::merge(main, branch, no_ff)
}

fn open_repo(path: &Path) -> Result<Evt> {
    let r = repo::Repo::discover(path)?;
    let worktrees = r.worktrees()?;
    // The requested path may be a subfolder of the checkout; we keep the deepest
    // worktree containing it, otherwise a worktree nested in another would be
    // attributed to the wrong one.
    let requested = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
    let opened_at = worktrees
        .iter()
        .filter(|w| requested.starts_with(&w.path))
        .max_by_key(|w| w.path.as_os_str().len())
        .map(|w| w.path.clone());
    Ok(Evt::RepoOpened {
        name: r.name(),
        worktrees,
        opened_at,
        main: r.main,
    })
}

/// Every write is followed by a re-read of the status: that is what keeps the
/// review panel accurate without the view having to know which command touches
/// what. The cost is one more `git status` per action triggered by hand.
fn write_then_refresh(
    worktree: PathBuf,
    action: Action,
    op: impl FnOnce(&Path) -> Result<String>,
) -> Vec<Evt> {
    match op(&worktree) {
        Ok(output) => {
            let mut evts = vec![done(Some(worktree.clone()), action, output)];
            if let Ok(status) = status::status(&worktree) {
                evts.push(Evt::Status { worktree, status });
            }
            evts
        }
        Err(e) => vec![fail(Some(worktree), action, e)],
    }
}

fn worktree_changed(r: &repo::Repo, output: String) -> Vec<Evt> {
    let mut evts = vec![done(None, Action::Worktree, output)];
    if let Ok(worktrees) = r.worktrees() {
        evts.push(Evt::Worktrees {
            main: r.main.clone(),
            worktrees,
        });
    }
    evts
}

fn done(worktree: Option<PathBuf>, action: Action, output: String) -> Evt {
    // A write is an event of the session — a commit, a push, a worktree
    // created — and `info` is the level the journal shows without being asked.
    // Reads say nothing here: there are hundreds a minute and none of them is
    // an event.
    log::info!("{action:?}{} — done{}", at(&worktree), first_line(&output));
    Evt::Done {
        worktree,
        action,
        output,
    }
}

/// ` on <worktree>`, or nothing when the operation belongs to a repository
/// rather than to one of its checkouts.
fn at(worktree: &Option<PathBuf>) -> String {
    match worktree {
        Some(path) => format!(" on {}", path.display()),
        None => String::new(),
    }
}

/// git's first line, when it said something.
///
/// The first only: `git push` writes a paragraph, and a journal line that wraps
/// four times hides the ones around it. The whole of it reaches the user
/// anyway — it is what `Evt::Done` carries to the status bar.
fn first_line(output: &str) -> String {
    match output.lines().find(|line| !line.trim().is_empty()) {
        Some(line) => format!(": {}", line.trim()),
        None => String::new(),
    }
}

/// Flattens `anyhow`'s chain of causes into one sentence: the view shows a
/// message, not a trace.
///
/// The chain of causes, end to end: git's says *what* failed, ours says *what we
/// were trying to do*.
fn describe_error(err: anyhow::Error) -> String {
    err.chain()
        .map(|c| c.to_string())
        .collect::<Vec<_>>()
        .join(" : ")
}

/// Puts a database result into the shape the event carries.
///
/// The error is flattened here rather than in the view: it is the only place
/// that still has the chain of causes, and that chain says both what the engine
/// refused and what we were trying to do.
fn db_result<T>(result: Result<T>) -> protocol::DbResult<T> {
    result.map_err(|e| {
        let message = describe_error(e);
        log::warn!("database: {message}");
        message
    })
}

fn fail(worktree: Option<PathBuf>, action: Action, err: anyhow::Error) -> Evt {
    let message = describe_error(err);
    // The only `warn` of this layer, and that is what makes it worth something:
    // git's own failures are filed at `debug`, because half of them are the
    // normal answer to an optional read. Here the operation is known, so it is
    // known that somebody was waiting for it.
    log::warn!("{action:?}{} — {message}", at(&worktree));
    Evt::Failed {
        worktree,
        action,
        message,
    }
}
