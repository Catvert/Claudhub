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

use crate::git::{branch, diff, history, repo, stash, status, tags, DiffRange, LogRange};

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
            // A tag pushed, a tag removed from the remote, the remote's tag
            // list: three round trips, and they belong where the round trips
            // are.
            | Cmd::PushTag { .. }
            | Cmd::CreateTag { push: true, .. }
            // And a commit that pushes, for the same reason: the pair is one
            // command so that nothing can reorder it, and where it goes is
            // decided by the half that costs seconds.
            | Cmd::Commit { push: true, .. }
            | Cmd::DeleteRemoteTag { .. }
            | Cmd::LoadRemoteTags { .. }
            // A branch published, one removed from the remote, one brought up
            // to its upstream: three round trips, and they belong where the
            // round trips are. A **rename** does not — it moves a local ref and
            // costs a millisecond.
            | Cmd::PushBranch { .. }
            | Cmd::DeleteRemoteBranch { .. }
            | Cmd::UpdateBranch { .. }
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
        Cmd::LoadSummaries { .. }
            | Cmd::ScanAgents { .. }
            | Cmd::WtScan { .. }
            | Cmd::JustLoad { .. }
    )
}

/// A project-wide search.
///
/// **Its own queue, one worker.** Not because `git grep` is slow — measured on
/// a Laravel project of eight thousand tracked files it answers in forty
/// milliseconds, sixty for a regular expression, which is a `git status` — but
/// because its cost is the only one here that is **unbounded**: it grows with
/// the project rather than with the index, it is asked again on every pause in
/// the typing, and on a Windows disk mounted by WSL every read costs several
/// times more. In the read queue a search over a very large checkout would take
/// one worker in three while the diff just asked for waits behind it; in the
/// network's, which has a single worker, it would sit in front of a `push`. It
/// is the databases' reasoning exactly.
///
/// One worker, because a search is **replaced** rather than accumulated: one
/// types, the earlier query is stale, and the send id is what tells the answer
/// of a gesture from the answer of the gesture that replaced it. Two workers
/// would only make two stale searches run at once.
fn is_search(cmd: &Cmd) -> bool {
    matches!(cmd, Cmd::Search { .. })
}

/// The `wt` operations that run the project's hooks.
///
/// **Their own queue**, and not the network's. They were there first, for the
/// right reason — a `post_new` installing dependencies, an `up` starting
/// containers, that takes minutes, and putting them with the reads would freeze
/// the review for the length of a `composer install`. But the network queue has
/// a single worker, so a `wt up` held back everything measured in seconds
/// behind something measured in minutes: the automatic fetch, `push`, `pull`,
/// the commit message an agent writes, the two Sentry calls. That is exactly
/// the symptom that moved the network out of the read queue, one floor down.
///
/// The reason for the network's single worker — two `fetch`es on the same
/// repository fighting over the reference lock — has never covered a project's
/// hooks, which touch nothing of git's.
fn is_long(cmd: &Cmd) -> bool {
    matches!(
        cmd,
        Cmd::WtCreate { .. } | Cmd::WtRemove { .. } | Cmd::WtUp { .. } | Cmd::WtDown { .. }
    )
}

/// Which queue a command belongs to.
///
/// A function of the command alone, so the routing is one readable table and
/// not a chain of conditions buried in `send` — and so a test can check that
/// no command lands in a queue that would make it wait behind something a
/// hundred times slower.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Queue {
    Reads,
    Network,
    Long,
    Background,
    Databases,
    Search,
}

fn queue_of(cmd: &Cmd) -> Queue {
    // A plugin's request says itself which of the two slow queues it belongs
    // to: an HTTP call is Sentry's profile, a shell command is the `wt` status
    // sweep's. Reading it off the capability rather than off the variant is
    // what keeps one table for both — and what lets a capability be added
    // without this function growing a special case.
    if let Cmd::PluginCall { cap, .. } = cmd {
        return if cap.is_network() {
            Queue::Network
        } else {
            Queue::Background
        };
    }
    // Same table, one floor up: a clone and a pull talk to a remote, removing a
    // directory does not.
    if let Cmd::PluginManage { op, .. } = cmd {
        return if op.is_network() {
            Queue::Network
        } else {
            Queue::Reads
        };
    }
    if is_network(cmd) {
        Queue::Network
    } else if is_long(cmd) {
        Queue::Long
    } else if is_background(cmd) {
        Queue::Background
    } else if is_db(cmd) {
        Queue::Databases
    } else if is_search(cmd) {
        Queue::Search
    } else {
        Queue::Reads
    }
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
        long: async_channel::Sender<Cmd>,
        background: async_channel::Sender<Cmd>,
        databases: async_channel::Sender<Cmd>,
        search: async_channel::Sender<Cmd>,
        /// The fifth channel: watch orders go through no queue — setting up a
        /// watch is already deferred into the watcher thread, and making it wait
        /// behind a diff would make no sense. `None` when watching could not
        /// start: the orders are then dropped, and the review only refreshes by
        /// hand.
        watcher: Option<watch::Watcher>,
        /// The seventh lane: the language servers. Outside the queues for the
        /// watcher's reason and one of its own — **order matters**. A
        /// `didChange` must reach the server before the completion that depends
        /// on it, which several workers sharing one channel cannot promise.
        lsp: std::sync::Arc<crate::lsp::Host>,
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
                long,
                background,
                databases,
                search,
                watcher,
                lsp,
            } => {
                let Some(cmd) = route_watch(watcher.as_ref(), cmd) else {
                    return;
                };
                let Some(cmd) = route_lsp(lsp, cmd) else {
                    return;
                };
                let queue = match queue_of(&cmd) {
                    Queue::Network => network,
                    Queue::Long => long,
                    Queue::Background => background,
                    Queue::Databases => databases,
                    Queue::Search => search,
                    Queue::Reads => reads,
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

/// Hands the language-server orders to their host and returns the others.
///
/// The same shape as `route_watch`, and the same reasoning one floor further:
/// these never queue. Unlike the watcher the host always exists — there is
/// nothing to fail at startup, a session being born only when a worktree asks
/// for one.
fn route_lsp(host: &std::sync::Arc<crate::lsp::Host>, cmd: Cmd) -> Option<Cmd> {
    use crate::lsp::Ask;
    match cmd {
        Cmd::LspStart { worktree, server } => host.start(worktree, server),
        Cmd::LspStop { worktree } => host.stop(worktree),
        Cmd::LspOpen {
            worktree,
            path,
            language_id,
            text,
        } => host.ask(
            &worktree,
            Ask::Open {
                path,
                language_id,
                text,
            },
        ),
        Cmd::LspChange {
            worktree,
            path,
            text,
        } => host.ask(&worktree, Ask::Change { path, text }),
        Cmd::LspClose { worktree, path } => host.ask(&worktree, Ask::Close { path }),
        Cmd::LspSave { worktree, path } => host.ask(&worktree, Ask::Save { path }),
        Cmd::LspRequest {
            worktree,
            id,
            method,
            params,
        } => host.ask(&worktree, Ask::Request { id, method, params }),
        Cmd::LspCancel { worktree, id } => host.ask(&worktree, Ask::Cancel { id }),
        Cmd::LspApplied {
            worktree,
            id,
            applied,
        } => host.ask(&worktree, Ask::Applied { id, applied }),
        other => return Some(other),
    }
    None
}

/// Starts the workers and returns what is needed to talk and listen to them.
pub fn spawn() -> (Handle, async_channel::Receiver<Evt>) {
    let (read_tx, read_rx) = async_channel::unbounded::<Cmd>();
    let (net_tx, net_rx) = async_channel::unbounded::<Cmd>();
    let (long_tx, long_rx) = async_channel::unbounded::<Cmd>();
    let (bg_tx, bg_rx) = async_channel::unbounded::<Cmd>();
    let (db_tx, db_rx) = async_channel::unbounded::<Cmd>();
    let (search_tx, search_rx) = async_channel::unbounded::<Cmd>();
    let (evt_tx, evt_rx) = async_channel::unbounded::<Evt>();

    for n in 0..READERS {
        worker(format!("claudhub-git-{n}"), read_rx.clone(), evt_tx.clone());
    }
    // Only one for the network: two simultaneous `fetch`es on the same
    // repository would fight over the reference lock without speeding anything up.
    worker("claudhub-git-net".into(), net_rx, evt_tx.clone());
    // And one for the project's hooks, which are measured in minutes and have
    // no business making a `push` wait.
    worker("claudhub-wt".into(), long_rx, evt_tx.clone());
    worker("claudhub-scan".into(), bg_rx, evt_tx.clone());
    for n in 0..DB_WORKERS {
        worker(format!("claudhub-db-{n}"), db_rx.clone(), evt_tx.clone());
    }
    // One for the searches — see `is_search`.
    worker("claudhub-search".into(), search_rx, evt_tx.clone());

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
                log::warn!("no relay for the file watcher: {e:#}");
            }
            Some(watcher)
        }
        Err(e) => {
            log::warn!("no file watching: {e:#}");
            None
        }
    };
    // The language servers live here too, and for the watcher's reason: what
    // they push — the diagnostics a server sends for a file an agent has just
    // written — becomes events on the same channel as everything else, a single
    // stream to carry over a wire, local or remote.
    let lsp = std::sync::Arc::new(crate::lsp::Host::new(evt_tx.clone()));
    drop(evt_tx);

    (
        Handle {
            inner: HandleInner::Local {
                reads: read_tx,
                network: net_tx,
                long: long_tx,
                background: bg_tx,
                databases: db_tx,
                search: search_tx,
                watcher,
                lsp,
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
        // Same thing one lane over: the language servers are handed to their
        // host by `Handle::send`, and never queue.
        Cmd::LspStart { .. }
        | Cmd::LspStop { .. }
        | Cmd::LspOpen { .. }
        | Cmd::LspChange { .. }
        | Cmd::LspClose { .. }
        | Cmd::LspSave { .. }
        | Cmd::LspRequest { .. }
        | Cmd::LspCancel { .. }
        | Cmd::LspApplied { .. } => {
            log::debug!("language server order arrived in a worker");
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
        Cmd::LoadTags { main } => match tags::list(&main) {
            Ok(tags) => vec![Evt::Tags { main, tags }],
            Err(e) => vec![fail(None, Action::Tag, e)],
        },
        Cmd::LoadRemoteTags { worktree } => match tags::remote(&worktree) {
            Ok(names) => vec![Evt::RemoteTags {
                main: main_of(&worktree),
                names,
            }],
            Err(e) => vec![fail(Some(worktree), Action::PushTag, e)],
        },
        Cmd::LoadStashes { main } => match stash::list(&main) {
            Ok(stashes) => vec![Evt::Stashes { main, stashes }],
            Err(e) => vec![fail(None, Action::Stash, e)],
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
            push,
        } => write_then_refresh(
            worktree,
            if push {
                Action::CommitPush
            } else {
                Action::Commit
            },
            |dir| {
                let committed = repo::commit(
                    dir,
                    repo::CommitOptions {
                        message: &message,
                        amend,
                        all,
                    },
                )?;
                // Committed then pushed, in that order and in one command: the
                // tags' precedent. What the report says is what git wrote about
                // the push — the round trip is the half one waited for, and the
                // commit's own line is already in the history that follows.
                if !push {
                    return Ok(committed);
                }
                let pushed = repo::push(dir, false)?;
                Ok(match pushed.trim().is_empty() {
                    true => committed,
                    false => pushed,
                })
            },
        ),
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
        Cmd::DeleteBranch { main, name, force } => branch_written(main, Action::Branch, |main| {
            repo::delete_branch(main, &name, force).map(|_| String::new())
        }),
        Cmd::RenameBranch { main, from, to } => branch_written(main, Action::Branch, |main| {
            repo::rename_branch(main, &from, &to).map(|_| String::new())
        }),
        Cmd::DeleteRemoteBranch { main, name } => branch_written(main, Action::Branch, |main| {
            repo::delete_remote_branch(main, &name)
        }),
        Cmd::PushBranch {
            main,
            branch,
            force_with_lease,
        } => branch_written(main, Action::Push, |main| {
            repo::push_branch(main, &branch, force_with_lease)
        }),
        Cmd::UpdateBranch { main, branch } => branch_written(main, Action::Pull, |main| {
            repo::update_branch(main, &branch)
        }),
        Cmd::CreateTag {
            worktree,
            name,
            message,
            at,
            push,
        } => tag_written(
            worktree,
            if push { Action::PushTag } else { Action::Tag },
            |dir| {
                tags::create(dir, &name, message.as_deref(), at.as_deref())?;
                // Created then pushed, in that order and in one command: two
                // commands would go into two queues, and nothing orders those.
                if push {
                    tags::push(dir, &name)
                } else {
                    Ok(String::new())
                }
            },
        ),
        Cmd::DeleteTag { worktree, name } => tag_written(worktree, Action::Tag, |dir| {
            tags::delete(dir, &name).map(|_| String::new())
        }),
        Cmd::DeleteRemoteTag { worktree, name } => tag_written(worktree, Action::PushTag, |dir| {
            tags::delete_remote(dir, &name)
        }),
        Cmd::PushTag { worktree, name } => {
            tag_written(worktree, Action::PushTag, |dir| match &name {
                Some(name) => tags::push(dir, name),
                None => tags::push_all(dir),
            })
        }
        Cmd::StashPush {
            worktree,
            message,
            untracked,
            keep_index,
        } => stash_written(worktree, |dir| {
            stash::push(dir, message.as_deref(), untracked, keep_index)
        }),
        Cmd::StashRestore {
            worktree,
            name,
            hash,
            pop,
            index,
        } => stash_written(worktree, |dir| {
            stash::restore(dir, &name, &hash, pop, index)
        }),
        Cmd::StashDrop {
            worktree,
            name,
            hash,
        } => stash_written(worktree, |dir| stash::drop(dir, &name, &hash)),
        Cmd::StashBranch {
            worktree,
            name,
            hash,
            branch,
        } => stash_written(worktree, |dir| stash::branch(dir, &name, &hash, &branch)),
        Cmd::StashClear { worktree } => {
            stash_written(worktree, |dir| stash::clear(dir).map(|_| String::new()))
        }
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
        Cmd::ReadMerge { worktree, path } => {
            let result = repo::stages(&worktree, &path).map_err(|e| format!("{e:#}"));
            vec![Evt::MergeStages {
                worktree,
                path,
                result,
            }]
        }
        Cmd::ResolveWith {
            worktree,
            path,
            content,
        } => write_then_refresh(worktree, Action::Resolve, |dir| {
            repo::resolve_with(dir, &path, &content).map(|_| String::new())
        }),

        // — Plugins —————————————————————————————————————————————————————
        // No `Done`/`Failed` for a capability: a plugin's request is not an
        // operation on the repository, and a failure belongs under the panel
        // that asked for it, not in a status bar the next message overwrites.
        Cmd::PluginCall { plugin, call, cap } => vec![Evt::PluginResult {
            plugin,
            call,
            result: cap.run(),
        }],
        Cmd::PluginManage { dir, op } => {
            let name = op.name().to_string();
            vec![Evt::PluginManaged {
                op: name,
                result: op.run(&dir).map_err(|e| format!("{e:#}")),
                dir,
            }]
        }

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

        // — Searching the project ———————————————————————————————————————
        Cmd::Search {
            worktree,
            query,
            request,
        } => {
            let result = crate::git::search::run(&worktree, &query).map_err(|e| format!("{e:#}"));
            vec![Evt::SearchDone {
                worktree,
                request,
                result,
            }]
        }
        // The failure travels **in the event** and not as an `Evt::Failed`: it
        // belongs under the preview that asked for it — a status bar the next
        // message wipes is where a file too large to show would be announced
        // and lost.
        Cmd::ReadPreview { worktree, path } => {
            let content = crate::files::read(&worktree, &path).map_err(|e| format!("{e:#}"));
            vec![Evt::Preview {
                worktree,
                path,
                content,
            }]
        }

        // — Project files ———————————————————————————————————————————————
        Cmd::ListFiles { worktree, ignored } => match repo::list_files(&worktree, ignored) {
            Ok(listing) => vec![Evt::ProjectFiles {
                worktree,
                files: listing.all,
                ignored: listing.ignored,
                dirs: listing.dirs,
            }],
            Err(e) => vec![fail(Some(worktree), Action::Read, e)],
        },
        Cmd::ReadDir { worktree, dir } => {
            let result = crate::files::read_dir(&worktree, &dir).map_err(|e| e.to_string());
            vec![Evt::DirListed {
                worktree,
                dir,
                result,
            }]
        }
        Cmd::ReadFile { worktree, path } => match crate::files::read(&worktree, &path) {
            Ok(content) => vec![Evt::FileContent {
                worktree,
                path,
                content,
            }],
            Err(e) => vec![fail(Some(worktree), Action::Read, e)],
        },
        Cmd::ReadImage { worktree, path } => match crate::files::read_image(&worktree, &path) {
            Ok(image) => vec![Evt::ImageContent {
                worktree,
                path,
                image,
            }],
            Err(e) => vec![fail(Some(worktree), Action::Read, e)],
        },
        // A file git does not track has no base, and that is an answer rather
        // than a failure: nothing is wrong, every line of it is simply new.
        Cmd::ReadFileBase { worktree, path } => {
            let text = repo::head_blob(&worktree, &path);
            vec![Evt::FileBase {
                worktree,
                path,
                text,
            }]
        }
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
        Cmd::JustLoad { worktree } => {
            let recipes = crate::just::snapshot(&worktree);
            vec![Evt::JustRecipes { worktree, recipes }]
        }
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
    // The history of a few lines is read by `-L`, which hands back the commits
    // and their restricted patches in one go; the graph stays empty, its lanes
    // having nothing to join — the parents are not in the list.
    if let LogRange::Lines { path, start, end } = &range {
        return match history::line_history(&worktree, path, *start, *end, limit) {
            Ok(found) => {
                let (commits, patches): (Vec<_>, Vec<_>) = found.into_iter().unzip();
                vec![Evt::History {
                    worktree,
                    range,
                    graph: vec![Default::default(); commits.len()],
                    commits,
                    patches,
                }]
            }
            Err(e) => vec![fail(Some(worktree), Action::History, e)],
        };
    }
    match history::commits(&worktree, &range, limit) {
        Ok(commits) => {
            let graph = history::layout(&commits);
            vec![Evt::History {
                worktree,
                range,
                commits,
                graph,
                patches: Vec::new(),
            }]
        }
        Err(e) => vec![fail(Some(worktree), Action::History, e)],
    }
}

/// A summary per worktree, for the sidebar.
///
/// **Four at a time, and not one after the other.** Each is a `git status` plus
/// a `--numstat`, so a dozen open worktrees was a dozen round trips in a row on
/// the single background worker — and the sidebar showed nothing until the last
/// one came back. Four because the work is a subprocess waiting on the disk,
/// not arithmetic: more lanes would only make the checkouts compete for it.
///
/// The order is the caller's, whatever the lanes finish in.
fn summaries(worktrees: Vec<PathBuf>) -> Vec<Evt> {
    const LANES: usize = 4;

    let lane = worktrees.len().div_ceil(LANES).max(1);
    let summaries = std::thread::scope(|scope| {
        let threads: Vec<_> = worktrees
            .chunks(lane)
            .map(|chunk| {
                scope.spawn(move || {
                    chunk
                        .iter()
                        .filter_map(|worktree| {
                            // A worktree deleted under our feet is not an error
                            // to display: it will disappear from the list at
                            // the next `git worktree list`.
                            status::summary(worktree)
                                .ok()
                                .map(|summary| (worktree.clone(), summary))
                        })
                        .collect::<Vec<_>>()
                })
            })
            .collect();
        threads
            .into_iter()
            .flat_map(|thread| thread.join().unwrap_or_default())
            .collect()
    });
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
/// A write that touches a repository's refs, followed by a fresh branch list.
///
/// The counterpart of `write_then_refresh` one floor up: what these commands
/// change is not a worktree's status but the list the picker shows, and a list
/// that still carries the branch one has just deleted is the one thing this menu
/// must not do. No worktree is named — a branch belongs to the repository, and
/// the one being written to is often held by a checkout the window never opened.
fn branch_written(
    main: PathBuf,
    action: Action,
    f: impl FnOnce(&Path) -> anyhow::Result<String>,
) -> Vec<Evt> {
    match f(&main) {
        Ok(output) => {
            let mut evts = vec![done(None, action, output)];
            evts.extend(
                branch::list(&main)
                    .ok()
                    .map(|list| branches_evt(main, list)),
            );
            evts
        }
        Err(e) => vec![fail(None, action, e)],
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
    // One session per project, kept for its worktrees: the three questions
    // below each used to reopen it — a `git rev-parse`, the `wt.toml` parsed
    // and the state read, three times per worktree of the scan.
    let mut sessions: std::collections::HashMap<PathBuf, Option<crate::wt::Session>> =
        std::collections::HashMap::new();
    let states = targets
        .into_iter()
        .filter_map(|(main, worktree)| {
            let session = sessions
                .entry(main.clone())
                .or_insert_with(|| crate::wt::Session::open(&main))
                .as_ref()?;
            let slug = session.slug_of(&worktree)?;
            let (up, endpoints) = session.state_of(&slug);
            Some((worktree, protocol::WtWorktree { up, endpoints }))
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

/// Every tag write is followed by a re-read of the tag list, for the reason
/// `write_then_refresh` re-reads the status: the panel must not have to know
/// which command touches what.
///
/// The status is **not** re-read: a tag changes no file, and a `git status` per
/// tag pushed would be a read nobody asked for.
fn tag_written(
    worktree: PathBuf,
    action: Action,
    op: impl FnOnce(&Path) -> Result<String>,
) -> Vec<Evt> {
    match op(&worktree) {
        Ok(output) => {
            let main = main_of(&worktree);
            let mut evts = vec![done(Some(worktree.clone()), action, output)];
            if let Ok(tags) = tags::list(&worktree) {
                evts.push(Evt::Tags { main, tags });
            }
            evts
        }
        Err(e) => vec![fail(Some(worktree), action, e)],
    }
}

/// Every stash write is followed by a re-read of **both** the status and the
/// stack — and of both **whether it succeeded or not**.
///
/// That last part is what sets this apart from `write_then_refresh`. A
/// `git stash pop` that conflicts writes the conflict markers into the files,
/// leaves the stash where it was and exits non-zero: the gesture failed and the
/// working tree changed all the same. Refreshing only on success would leave
/// the review panel showing the tree as it was before, which is the one state
/// it certainly is not in.
fn stash_written(worktree: PathBuf, op: impl FnOnce(&Path) -> Result<String>) -> Vec<Evt> {
    let outcome = op(&worktree);
    let main = main_of(&worktree);
    let mut evts = vec![match outcome {
        Ok(output) => done(Some(worktree.clone()), Action::Stash, output),
        Err(e) => fail(Some(worktree.clone()), Action::Stash, e),
    }];
    if let Ok(stashes) = stash::list(&worktree) {
        evts.push(Evt::Stashes { main, stashes });
    }
    if let Ok(status) = status::status(&worktree) {
        evts.push(Evt::Status { worktree, status });
    }
    evts
}

/// The main repository a checkout belongs to, which is what keys the tag and
/// stash lists.
///
/// Tags live in the shared `.git`: they are the same seen from every worktree,
/// and filing them under the checkout they were read from would make one list
/// per worktree of the same refs. A `rev-parse` costs a fork, and a tag write is
/// not a thing one does sixty times a second; the checkout itself is the fallback
/// when discovery fails, so the event is never lost.
fn main_of(worktree: &Path) -> PathBuf {
    repo::Repo::discover(worktree)
        .map(|r| r.main)
        .unwrap_or_else(|_| worktree.to_path_buf())
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

#[cfg(test)]
mod tests {
    use super::*;

    fn worktree() -> PathBuf {
        PathBuf::from("/p/site")
    }

    #[test]
    fn a_projects_hooks_never_hold_back_a_push() {
        // `wt` used to share the network's single worker. A `wt up` starting
        // containers therefore held everything measured in seconds behind
        // something measured in minutes — which is the very symptom that moved
        // the network out of the read queue in the first place.
        assert_eq!(
            queue_of(&Cmd::WtUp {
                main: worktree(),
                slug: "fix".into(),
                answers: Default::default(),
            }),
            Queue::Long
        );
        assert_eq!(
            queue_of(&Cmd::Push {
                worktree: worktree(),
                force_with_lease: false,
            }),
            Queue::Network
        );
    }

    /// The six branch operations, sorted by what they cost and not by the fact
    /// that they all say "branch".
    ///
    /// Three of them talk to a remote and three do not, and putting a rename
    /// behind a `push` would make a keystroke wait on a network round trip. The
    /// mistake is silent — a misrouted command never fails, it waits.
    #[test]
    fn a_branch_operation_goes_where_its_cost_is() {
        let main = worktree();
        for local in [
            Cmd::CreateBranch {
                worktree: worktree(),
                name: "feat".into(),
                from: None,
            },
            Cmd::DeleteBranch {
                main: main.clone(),
                name: "feat".into(),
                force: false,
            },
            Cmd::RenameBranch {
                main: main.clone(),
                from: "feat".into(),
                to: "feat-2".into(),
            },
        ] {
            assert_eq!(queue_of(&local), Queue::Reads, "{}", local.name());
        }
        for remote in [
            Cmd::PushBranch {
                main: main.clone(),
                branch: "feat".into(),
                force_with_lease: false,
            },
            Cmd::DeleteRemoteBranch {
                main: main.clone(),
                name: "feat".into(),
            },
            Cmd::UpdateBranch {
                main: main.clone(),
                branch: "feat".into(),
            },
        ] {
            assert_eq!(queue_of(&remote), Queue::Network, "{}", remote.name());
        }
    }

    /// A tag creation is local and instant; pushing one is a round trip. The
    /// two go into two different queues, and that is exactly why a creation
    /// that also pushes is **one** command: nothing orders two queues, and the
    /// push would leave before the tag existed.
    #[test]
    fn a_tag_goes_where_its_cost_is() {
        let local = Cmd::CreateTag {
            worktree: worktree(),
            name: "v1.0".into(),
            message: None,
            at: None,
            push: false,
        };
        assert_eq!(queue_of(&local), Queue::Reads);
        let published = Cmd::CreateTag {
            worktree: worktree(),
            name: "v1.0".into(),
            message: None,
            at: None,
            push: true,
        };
        assert_eq!(queue_of(&published), Queue::Network);
        assert_eq!(queue_of(&Cmd::LoadTags { main: worktree() }), Queue::Reads);
        assert_eq!(
            queue_of(&Cmd::LoadRemoteTags {
                worktree: worktree()
            }),
            Queue::Network
        );
        assert_eq!(
            queue_of(&Cmd::DeleteTag {
                worktree: worktree(),
                name: "v1.0".into(),
            }),
            Queue::Reads
        );
        assert_eq!(
            queue_of(&Cmd::DeleteRemoteTag {
                worktree: worktree(),
                name: "v1.0".into(),
            }),
            Queue::Network
        );
    }

    /// A search is unbounded in cost and a preview is a file read: they must
    /// not share a queue, and neither may sit where a frame is waiting.
    #[test]
    fn a_search_waits_in_its_own_queue_and_its_preview_does_not() {
        assert_eq!(
            queue_of(&Cmd::Search {
                worktree: worktree(),
                query: crate::git::search::Query {
                    text: "todo".into(),
                    ..Default::default()
                },
                request: 1,
            }),
            Queue::Search
        );
        assert_eq!(
            queue_of(&Cmd::ReadPreview {
                worktree: worktree(),
                path: "src/main.rs".into(),
            }),
            Queue::Reads
        );
    }

    /// The editor's gutter waits on this one: a `git show` of a single blob is
    /// a read, and putting it anywhere slower would leave the change strip
    /// blank behind a fetch.
    /// A picture is what a frame is waiting for, exactly like the text next to
    /// it: the tab is already open and empty until it lands.
    #[test]
    fn an_image_is_a_read() {
        assert_eq!(
            queue_of(&Cmd::ReadImage {
                worktree: worktree(),
                path: "assets/logo.png".into(),
            }),
            Queue::Reads
        );
    }

    #[test]
    fn the_editors_base_is_a_read() {
        assert_eq!(
            queue_of(&Cmd::ReadFileBase {
                worktree: worktree(),
                path: "src/main.rs".into(),
            }),
            Queue::Reads
        );
    }

    /// The three-pane merge waits on both of these, and a frame waits on the
    /// merge: reading the three stages is three `git show` of one blob each, and
    /// writing the outcome is a local write like any other.
    #[test]
    fn a_merge_is_read_and_written_on_the_reads_queue() {
        assert_eq!(
            queue_of(&Cmd::ReadMerge {
                worktree: worktree(),
                path: "src/main.rs".into(),
            }),
            Queue::Reads
        );
        assert_eq!(
            queue_of(&Cmd::ResolveWith {
                worktree: worktree(),
                path: "src/main.rs".into(),
                content: String::new(),
            }),
            Queue::Reads
        );
    }

    #[test]
    fn the_slow_queues_are_the_only_slow_queues() {
        // A read is what a frame waits for: nothing that takes seconds may
        // land in that queue, and nothing that answers in milliseconds should
        // wait in the slow ones.
        assert_eq!(
            queue_of(&Cmd::LoadDiffFiles {
                worktree: worktree(),
                range: crate::git::DiffRange::Working,
            }),
            Queue::Reads
        );
        assert_eq!(
            queue_of(&Cmd::LoadSummaries {
                worktrees: vec![worktree()],
            }),
            Queue::Background
        );
        assert_eq!(
            queue_of(&Cmd::ScanAgents {
                worktrees: vec![worktree()],
                programs: vec!["claude".into()],
            }),
            Queue::Background
        );
        assert_eq!(
            queue_of(&Cmd::AutoFetch { main: worktree() }),
            Queue::Network
        );
    }

    /// A plugin never gets in front of a diff, and never behind a `composer
    /// install` either.
    ///
    /// The routing is read off the capability, which is what keeps one table
    /// for both: an HTTP call has Sentry's profile — seconds, a socket — and a
    /// shell command has the `wt` sweep's.
    #[test]
    fn a_plugins_request_lands_by_what_it_asks_for() {
        assert_eq!(
            queue_of(&Cmd::PluginCall {
                plugin: "ci".into(),
                call: 1,
                cap: crate::plugin::caps::Cap::Http {
                    method: "GET".into(),
                    url: "https://example.test".into(),
                    headers: Vec::new(),
                    body: None,
                    secret: None,
                },
            }),
            Queue::Network
        );
        assert_eq!(
            queue_of(&Cmd::PluginCall {
                plugin: "ci".into(),
                call: 2,
                cap: crate::plugin::caps::Cap::Shell {
                    worktree: worktree(),
                    command: "gh run list".into(),
                },
            }),
            Queue::Background
        );
    }

    /// Installing waits on a socket; removing a directory does not, and
    /// putting it in the network's single worker would make it queue behind a
    /// clone that has nothing to do with it.
    #[test]
    fn managing_a_plugin_lands_by_what_it_does() {
        use crate::plugin::install::Manage;
        let dir = PathBuf::from("/c/plugins/ci");
        assert_eq!(
            queue_of(&Cmd::PluginManage {
                dir: dir.clone(),
                op: Manage::Install {
                    url: "https://example.test/x.git".into()
                },
            }),
            Queue::Network
        );
        assert_eq!(
            queue_of(&Cmd::PluginManage {
                dir,
                op: Manage::Remove,
            }),
            Queue::Reads
        );
    }

    #[test]
    fn a_watch_order_goes_to_no_queue_at_all() {
        // The fifth path: setting up a watch is already deferred into the
        // watcher's thread, and making it wait behind a diff would make no
        // sense. With no watcher there is nothing to hand it to, and the order
        // is dropped rather than queued for a thread that does not exist.
        assert!(route_watch(
            None,
            Cmd::Watch {
                worktree: worktree()
            }
        )
        .is_none());
        // Anything else comes straight back.
        assert!(route_watch(
            None,
            Cmd::Fetch {
                worktree: worktree()
            }
        )
        .is_some());
    }
}
