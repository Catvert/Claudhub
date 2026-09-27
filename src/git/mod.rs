//! Claudhub's git layer.
//!
//! Everything goes through the `git` binary as a subprocess, never through
//! libgit2. A user launching Claudhub expects their `credential.helper`,
//! `includeIf`, hooks, `commit.gpgsign` and aliases to apply — that is, their
//! configuration, not a reimplementation covering half of it. The cost is one
//! `fork` per command; at the scale of a review panel refreshing on file
//! events, it is invisible.
//!
//! No function in this module may be called from the UI thread: they block.
//! They are meant to run in the worker (`crate::app::worker`), which sends its
//! results back as events.

pub mod branch;
pub mod diff;
// Named `history` and not `log`: a module `log` in this crate would shadow the
// logging library of the same name for the whole file.
pub mod history;
pub mod outline;
pub mod repo;
pub mod search;
pub mod snapshot;
pub mod stash;
pub mod status;
pub mod tags;

#[cfg(test)]
pub(crate) mod submodule_tests;

pub use branch::{Branch, BranchKind, Upstream};
pub use diff::{DiffFile, DiffLine, DiffLineKind, FileDiff, Hunk, Range as DiffRange};
pub use history::{Commit, GraphRow, LogRange};
pub use outline::Outline;
pub use repo::{Pending, Repo, Stages, Worktree};
pub use search::Query as SearchQuery;
pub use stash::Stash;
pub use status::{FileStatus, Status, StatusCode, Summary};
pub use tags::Tag;

use std::ffi::OsStr;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use anyhow::{Context, Result};

/// Goes **before** the subcommand of every command that names files: paths are
/// then paths, and not patterns.
///
/// A pathspec is a glob by default, so `app/[id]/page.tsx` — a Next.js route,
/// named exactly like that on disk — also matches `app/i/page.tsx` and
/// `app/d/page.tsx`: a discard of the first restored the others too, and
/// nothing said so. Every path this layer hands git is one the status or the
/// explorer read off the disk, never a pattern typed by someone.
///
/// Per command and not in `command()`: `git grep` is given the user's globs
/// (`*.php`, `!vendor`) on purpose, and needs them read as such.
pub(crate) const LITERAL_PATHS: &str = "--literal-pathspecs";

/// Goes with every diff this layer parses: without them a `diff.external`, a
/// `.gitattributes` driver or `color.diff = always` replaces the unified output
/// with a format we do not know how to read.
pub(crate) const PARSABLE: [&str; 2] = ["--no-ext-diff", "--no-color"];

/// Goes with every unified diff the review reads: `PARSABLE`, renames paired,
/// and a submodule shown as the commit it moved to whatever the user's
/// configuration hides.
pub(crate) const UNIFIED: [&str; 5] = [
    PARSABLE[0],
    PARSABLE[1],
    "-M",
    "--ignore-submodules=none",
    "--submodule=short",
];

/// Beyond this, the command is killed and the failure comes back as a message.
///
/// No git read takes thirty seconds: a `status` costs ten milliseconds on a
/// repository of forty thousand files. This timeout therefore does not exist
/// for slow commands but for those that **never finish** — an authentication
/// prompt nobody sees, a repository on a network mount that has vanished, a
/// lock held by another tool. Without it, one such command takes a worker away
/// for good, and three freeze the whole application without a single message.
const TIMEOUT: Duration = Duration::from_secs(30);

/// Runs `git -C <dir> <args…>` and returns its standard output, without the
/// trailing newline.
///
/// `stdin` is closed: without that, a command deciding to ask for a password
/// inherits the terminal Claudhub was launched from — at best nothing is
/// displayed, at worst the worker blocks forever on a prompt nobody sees.
/// `GIT_TERMINAL_PROMPT=0` makes git say no rather than letting it try, and
/// the failure comes back as an ordinary error message.
pub(crate) fn git<S: AsRef<OsStr>>(dir: &Path, args: &[S]) -> Result<String> {
    let out = exec(dir, args, Opts::default())?;
    Ok(strip_trailing_newline(into_text(out.stdout)))
}

/// What a command is run with, beyond its directory and its arguments.
#[derive(Default)]
struct Opts<'a> {
    /// Variables of its own in the command's environment.
    env: &'a [(&'a str, &'a OsStr)],
    /// Written on its standard input, which stays closed without it.
    input: Option<Vec<u8>>,
    /// The highest exit code that is an answer rather than a failure: zero
    /// for all but `git_tolerant`'s commands.
    max_code: i32,
}

/// The one runner behind every `git_*` of this module: launches the command,
/// waits for it without exceeding `TIMEOUT`, files it in the journal, and turns
/// an exit code past `max_code` into an error carrying what git said on stderr.
///
/// The variants differ only in what they make of the output — text or bytes,
/// stdout alone or stderr with it — which is why they are conversions of what
/// this returns and not copies of its wait.
fn exec<S: AsRef<OsStr>>(dir: &Path, args: &[S], opts: Opts) -> Result<std::process::Output> {
    let started = Instant::now();
    let mut cmd = command(dir, args);
    cmd.envs(opts.env.iter().copied())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    if opts.input.is_some() {
        cmd.stdin(Stdio::piped());
    }
    let out = wait_feeding(cmd, opts.input, TIMEOUT, || {
        format!("git {}", describe(args))
    })?;
    report(dir, args, started.elapsed(), &out);
    if refused(out.status, opts.max_code) {
        return Err(failure(args, &out.stderr));
    }
    Ok(out)
}

/// An exit code past what the caller accepts — or none at all, the command
/// having been killed by a signal.
fn refused(status: std::process::ExitStatus, max_code: i32) -> bool {
    let code = status.code().unwrap_or(-1);
    code < 0 || code > max_code
}

/// The error of a command git refused, in git's own words.
fn failure<S: AsRef<OsStr>>(args: &[S], stderr: &[u8]) -> anyhow::Error {
    anyhow::anyhow!(
        "git {}: {}",
        describe(args),
        String::from_utf8_lossy(stderr).trim()
    )
}

/// An output as text, **without copying it** when it is valid UTF-8 — which
/// nearly every output is, where `from_utf8_lossy(..).into_owned()` copied a
/// diff of megabytes whole to say so — and lossily when it is not.
pub(crate) fn into_text(bytes: Vec<u8>) -> String {
    String::from_utf8(bytes).unwrap_or_else(|e| String::from_utf8_lossy(e.as_bytes()).into_owned())
}

/// Files a command in the journal: what was run, where, how long it took, and
/// what came back.
///
/// **`debug` and not `warn`, failures included.** A failure here is not
/// necessarily one for the user: `git_opt` exists precisely for the reads whose
/// failure is the normal answer — a branch with no upstream, a file git does not
/// know. Warning on each of them would fill the journal with false alarms and
/// bury the real ones. What matters to the user is warned about one floor up,
/// in `runtime::fail`, which knows the operation it belonged to.
///
/// The exception is a command that **drags**: past a second it explains an
/// interface that seems stuck, which is worth saying without being asked. On a
/// Windows disk mounted by WSL a `git status` reaches that on its own — and
/// that is exactly the case one wants to be told about.
fn report<S: AsRef<OsStr>>(dir: &Path, args: &[S], elapsed: Duration, out: &std::process::Output) {
    let slow = elapsed >= Duration::from_secs(1);
    if !out.status.success() {
        // The code as well as the message: `git diff --no-index` says "there is
        // a difference" with 1 and "I could not read the file" with 2, and the
        // stderr of the first is empty.
        log::debug!(
            "git {} in {} — failed ({}) after {}: {}",
            describe(args),
            dir.display(),
            out.status.code().unwrap_or(-1),
            crate::logging::ms(elapsed),
            String::from_utf8_lossy(&out.stderr).trim()
        );
    } else if slow {
        log::info!(
            "git {} in {} — {} (slow)",
            describe(args),
            dir.display(),
            crate::logging::ms(elapsed)
        );
    } else {
        log::debug!(
            "git {} in {} — {}",
            describe(args),
            dir.display(),
            crate::logging::ms(elapsed)
        );
    }
}

/// The same as `git`, with something written on the command's standard input.
///
/// For the writes that take a patch rather than arguments: `git apply`, fed
/// what `git show` said or a hunk the review rebuilt. The input never touches
/// the disk — a temporary file would be a path to translate on the Windows
/// target — and the wait is `wait_feeding`'s, which writes from a thread so a
/// slow reader cannot deadlock against the pipes we drain.
pub(crate) fn git_feeding<S: AsRef<OsStr>>(
    dir: &Path,
    args: &[S],
    input: Vec<u8>,
) -> Result<String> {
    let opts = Opts {
        input: Some(input),
        ..Opts::default()
    };
    let out = exec(dir, args, opts)?;
    Ok(strip_trailing_newline(into_text(out.stdout)))
}

/// The same as `git`, with variables of its own in the command's environment.
///
/// For the review point (`snapshot`), whose commands work on an index that is
/// **not** the user's: `GIT_INDEX_FILE` is the only way to name one, git
/// having no option for it.
pub(crate) fn git_env<S: AsRef<OsStr>>(
    dir: &Path,
    args: &[S],
    env: &[(&str, &OsStr)],
) -> Result<String> {
    let opts = Opts {
        env,
        ..Opts::default()
    };
    let out = exec(dir, args, opts)?;
    Ok(strip_trailing_newline(into_text(out.stdout)))
}

/// Waits for a process to finish, or interrupts it once `limit` passes, with
/// something to write on its standard input if `input` says so.
///
/// Separated from `exec` so it can be verified: testing it with `git` would
/// need a git command that hangs reproducibly, and there is none. It is not
/// only git's: `outside::Bounded` waits for every other program this way, for
/// the same reason — a full pipe blocks the writer, and reading after the wait
/// is the classic deadlock.
///
/// The command must have asked for a piped `stdin` when there is an input to
/// give. The write goes through a thread of its own for the reason the reads
/// do: a program that reads its input slowly — an agent reading a diff of a
/// megabyte — blocks whoever writes it, and writing before waiting deadlocks
/// against the very pipes we have not started draining.
///
/// The write **ignores its own failure**: a program that exits before reading
/// everything closes the pipe, and it is its exit code that has to be reported,
/// not an `EPIPE` that explains nothing.
pub(crate) fn wait_feeding(
    mut cmd: Command,
    input: Option<Vec<u8>>,
    limit: Duration,
    describe: impl Fn() -> String,
) -> Result<std::process::Output> {
    let mut child = cmd
        .spawn()
        .with_context(|| format!("{}: program not found", describe()))?;

    let feeder = input.map(|bytes| {
        let mut stdin = child.stdin.take().expect("stdin requested as piped");
        std::thread::spawn(move || {
            let _ = std::io::Write::write_all(&mut stdin, &bytes);
        })
    });

    let stdout = child.stdout.take().expect("stdout requested as piped");
    let stderr = child.stderr.take().expect("stderr requested as piped");
    // Closing an output is the process's last act: whoever finishes reading one
    // wakes the wait below, which is what replaced polling every five
    // milliseconds for an answer that usually arrives in ten.
    let (closed, closes) = std::sync::mpsc::channel::<()>();
    let out_read = drain(stdout, closed.clone());
    let err_read = drain(stderr, closed);

    /// The pipes are closed and the process still has not been reaped: it is
    /// about to be, or a grandchild inherited them. Rare enough to be polled.
    const RESIDUAL: Duration = Duration::from_millis(50);

    let deadline = Instant::now() + limit;
    let mut outputs_closed = 0;
    let status = loop {
        if let Some(status) = child.try_wait()? {
            break status;
        }
        let now = Instant::now();
        if now >= deadline {
            return Err(interrupt(&mut child, &describe(), limit));
        }
        // Both outputs already closed: nothing will wake us any more, and the
        // exit status is a hair away — a short step, not the residual one.
        match closes.recv_timeout(RESIDUAL.min(deadline - now)) {
            Ok(()) => outputs_closed += 1,
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
                std::thread::sleep(Duration::from_millis(1))
            }
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {}
        }
    };

    // **The process has exited; its pipes may not have.** A hook that starts
    // a daemon — a `post-checkout` bringing up a watcher, a `wt` hook running
    // `docker compose up -d` — hands it our stdout and stderr, and the daemon
    // keeps them open for as long as it lives. Waiting for the end of file
    // then meant waiting for the daemon, with no ceiling at all: the command
    // was over and the worker gone for good. What was written before the exit
    // is what counts, and the rest of the linger is left to the readers.
    let linger = Instant::now() + LINGER;
    while outputs_closed < 2 {
        let now = Instant::now();
        if now >= linger {
            log::debug!(
                "{} exited but left its output open; not waiting for it",
                describe()
            );
            break;
        }
        match closes.recv_timeout(linger - now) {
            Ok(()) => outputs_closed += 1,
            Err(_) => break,
        }
    }

    // The feeder is not waited for either, for the same reason: a program that
    // exited has closed its end, and one that handed its stdin to a daemon
    // could hold the write forever. Dropping the handle detaches it.
    drop(feeder);
    Ok(std::process::Output {
        status,
        stdout: take(&out_read),
        stderr: take(&err_read),
    })
}

/// Kills a command that overran its ceiling, and says so.
fn interrupt(child: &mut std::process::Child, what: &str, limit: Duration) -> anyhow::Error {
    let _ = child.kill();
    let _ = child.wait();
    anyhow::anyhow!("{what} did not answer within {limit:?} and was interrupted")
}

/// How long the outputs of an exited process are still waited for.
///
/// The bytes a process wrote before exiting are in the pipe already, and a
/// reader needs microseconds to take them: this is not a delay anything pays in
/// the ordinary case, only a ceiling for the one where a grandchild holds the
/// pipe open.
const LINGER: Duration = Duration::from_secs(2);

/// Reads an output to its end on a thread of its own, into a buffer the waiter
/// can take **at any moment** — which a `read_to_end` returning its buffer on
/// `join` could not give, the join being exactly the wait with no ceiling.
fn drain(
    mut pipe: impl Read + Send + 'static,
    closed: std::sync::mpsc::Sender<()>,
) -> std::sync::Arc<std::sync::Mutex<Vec<u8>>> {
    let buffer = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let filled = buffer.clone();
    std::thread::spawn(move || {
        let mut chunk = [0u8; 64 * 1024];
        loop {
            match pipe.read(&mut chunk) {
                Ok(0) => break,
                Ok(n) => filled
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .extend_from_slice(&chunk[..n]),
                Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
                Err(_) => break,
            }
        }
        let _ = closed.send(());
    });
    buffer
}

/// What a reader has gathered so far, taken out of its buffer.
fn take(buffer: &std::sync::Mutex<Vec<u8>>) -> Vec<u8> {
    std::mem::take(&mut *buffer.lock().unwrap_or_else(|e| e.into_inner()))
}

/// Runs git and returns **what it told the user**, stderr included.
///
/// For the network commands, and for them only: `git push` and `git fetch`
/// write their whole account on **stderr** — `To github.com:…`, the refs they
/// moved, `From origin` — and their stdout is empty. Read through `git`, which
/// keeps only stdout, a push that had just published three commits came back
/// with nothing to say: the status bar fell back on its own success label and
/// no balloon was raised, since there was not a line to read. What one wants to
/// know after a push is precisely what git wrote there.
///
/// stderr **first**: git leads with the account (`To …`) and finishes with the
/// advice, and it is the first line the bar keeps.
pub(crate) fn git_reporting<S: AsRef<OsStr>>(dir: &Path, args: &[S]) -> Result<String> {
    let out = exec(dir, args, Opts::default())?;
    let (stderr, stdout) = (into_text(out.stderr), into_text(out.stdout));
    let mut output = String::with_capacity(stderr.len() + stdout.len());
    output.push_str(stderr.trim_end());
    if !output.is_empty() && !stdout.trim().is_empty() {
        output.push('\n');
    }
    output.push_str(stdout.trim_end());
    Ok(output)
}

/// Runs git and returns its output **byte for byte**.
///
/// For what is a *file* and not an answer: the stages of a conflicted file,
/// which are read to be merged and written back to disk. `git` strips the
/// trailing newlines, which is right for the output of a command and wrong for
/// a blob — the newline it eats is a change nobody made, and it would land in
/// the file being resolved.
///
/// Invalid UTF-8 is an error rather than a lossy conversion: a binary file has
/// nothing to show in three columns, and replacing its bytes with question
/// marks would resolve it into something no one wrote.
pub(crate) fn git_blob<S: AsRef<OsStr>>(dir: &Path, args: &[S]) -> Result<String> {
    let out = exec(dir, args, Opts::default())?;
    String::from_utf8(out.stdout).context("this file is binary")
}

/// Runs git and returns its standard output as bytes, untouched.
///
/// For what is read to be **written back**: a patch. `git` converts lossily and
/// strips the trailing newlines *and carriage returns*, so the last line of a
/// CRLF file lost its `\r` on the way — and a patch whose context line lacks it
/// no longer applies to the file it came from.
pub(crate) fn git_bytes<S: AsRef<OsStr>>(dir: &Path, args: &[S]) -> Result<Vec<u8>> {
    Ok(exec(dir, args, Opts::default())?.stdout)
}

/// The same, but failure counts as `None`: for optional reads (an upstream
/// branch that does not exist is not an error).
pub(crate) fn git_opt<S: AsRef<OsStr>>(dir: &Path, args: &[S]) -> Option<String> {
    git(dir, args).ok()
}

/// Runs git and returns its output **even when the exit code is not zero**.
///
/// For the handful of commands where a non-zero code is the normal case:
/// `diff --no-index` exits with 1 as soon as there is a difference, which is
/// exactly what it was asked to find. Going through `git` would throw the
/// output away along with the "error".
///
/// Past `max_code` it is a real failure: `--no-index` exits with 2 when the
/// file does not exist or cannot be read.
pub(crate) fn git_tolerant<S: AsRef<OsStr>>(
    dir: &Path,
    args: &[S],
    max_code: i32,
) -> Result<String> {
    let opts = Opts {
        max_code,
        ..Opts::default()
    };
    let out = exec(dir, args, opts)?;
    Ok(strip_trailing_newline(into_text(out.stdout)))
}

/// A reader of a command's standard output, line by line, free to decide it has
/// seen enough.
pub(crate) trait Sink {
    type Output;
    /// `false` stops the command there: what follows would be thrown away.
    fn line(&mut self, line: &str) -> bool;
    /// `interrupted` says the command was stopped rather than left to finish.
    fn finish(self, interrupted: bool) -> Self::Output;
}

/// Runs a git command and hands its standard output to `sink`, one line at a
/// time, **killing it as soon as the sink has had enough**.
///
/// This exists for `git grep`, whose answer is capped: a common word on a large
/// checkout writes tens of megabytes for the two thousand lines that are kept,
/// and reading the lot before cutting means waiting for a walk whose outcome was
/// already decided. Nothing else needs it — a status or a diff is read whole
/// because all of it is used.
///
/// `max_code` is `git_tolerant`'s: the codes that are an answer rather than a
/// failure. An interrupted command has no code worth reading, and does not get
/// one looked at.
pub(crate) fn run_streaming<S: AsRef<OsStr>, K: Sink>(
    dir: &Path,
    args: &[S],
    max_code: i32,
    mut sink: K,
) -> Result<K::Output> {
    let started = Instant::now();
    let mut cmd = command(dir, args);
    cmd.stdout(Stdio::piped()).stderr(Stdio::piped());
    let mut child = cmd
        .spawn()
        .with_context(|| format!("git {}: program not found", describe(args)))?;

    let stdout = child.stdout.take().expect("stdout requested as piped");
    let mut stderr = child.stderr.take().expect("stderr requested as piped");
    let err_reader = std::thread::spawn(move || {
        let mut buffer = Vec::new();
        let _ = stderr.read_to_end(&mut buffer);
        buffer
    });

    // Nothing to poll: only the sink and the ceiling end a search.
    let heard = follow_lines(
        vec![((), Box::new(stdout) as Box<dyn Read + Send>)],
        Instant::now() + TIMEOUT,
        TIMEOUT,
        || false,
        |(), line| sink.line(&line),
    );
    let interrupted = match heard {
        Heard::Closed => false,
        Heard::Enough => {
            let _ = child.kill();
            true
        }
        Heard::Overran => {
            let what = format!("git {}", describe(args));
            return Err(interrupt(&mut child, &what, TIMEOUT));
        }
    };
    let status = child.wait()?;
    let elapsed = started.elapsed();
    log::debug!(
        "git {} in {} — {}{}",
        describe(args),
        dir.display(),
        crate::logging::ms(elapsed),
        if interrupted { " (capped)" } else { "" }
    );

    if !interrupted && refused(status, max_code) {
        return Err(failure(args, &err_reader.join().unwrap_or_default()));
    }
    Ok(sink.finish(interrupted))
}

/// How [`follow_lines`] stopped listening.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Heard {
    /// Every stream reached its end: the process is exiting, and its status is
    /// the caller's to wait for.
    Closed,
    /// The caller had enough — a line said so, or `should_stop` did.
    Enough,
    /// The deadline passed first.
    Overran,
}

/// Hands a process's output streams to `line`, one line at a time and tagged
/// with the stream it came from, **until the deadline** — the shape both
/// `run_streaming` and a test run (`suite::follow`) follow.
///
/// The lines travel through a channel rather than being read here: a read
/// blocked on a command that says nothing would have no ceiling, and the
/// ceiling is the whole reason `wait_feeding` exists. The wait wakes at least
/// every `poll` to ask `should_stop`, so a flag raised elsewhere is heard
/// while the process is silent.
///
/// **On anything but `Closed`, killing the process is the caller's**, and so
/// is the way: one process, or its whole group. The channel is closed on the
/// way out, so a reader blocked in `send` on a full channel wakes and ends;
/// one blocked in `read` ends when the pipe closes. Neither is waited for — a
/// grandchild holding the pipe would make that wait endless.
pub(crate) fn follow_lines<T: Clone + Send + 'static>(
    streams: Vec<(T, Box<dyn Read + Send>)>,
    deadline: Instant,
    poll: Duration,
    mut should_stop: impl FnMut() -> bool,
    mut line: impl FnMut(T, String) -> bool,
) -> Heard {
    use std::io::BufRead;
    use std::sync::mpsc::RecvTimeoutError;

    let (lines, incoming) = std::sync::mpsc::sync_channel::<(T, String)>(256);
    for (tag, stream) in streams {
        let lines = lines.clone();
        std::thread::spawn(move || {
            for read in std::io::BufReader::new(stream).split(b'\n') {
                let Ok(read) = read else { break };
                // A line is not necessarily UTF-8 — a match inside a file with
                // an odd encoding — and losing it beats losing the output.
                if lines.send((tag.clone(), into_text(read))).is_err() {
                    break; // the caller has had enough
                }
            }
        });
    }
    // Only the readers hold a sender now: the channel disconnects when the
    // last stream ends.
    drop(lines);

    loop {
        if should_stop() {
            return Heard::Enough;
        }
        let now = Instant::now();
        if now >= deadline {
            return Heard::Overran;
        }
        match incoming.recv_timeout((deadline - now).min(poll)) {
            Ok((tag, read)) => {
                if !line(tag, read) {
                    return Heard::Enough;
                }
            }
            Err(RecvTimeoutError::Disconnected) => return Heard::Closed,
            Err(RecvTimeoutError::Timeout) => continue,
        }
    }
}

/// True if the command exits with code 0. For closed questions
/// (`show-ref --verify --quiet`) whose output interests nobody.
///
/// Through `exec` like the rest, for its ceiling: `status()` waited without
/// one, and a question asked of a vanished network mount took its worker away
/// for good.
pub(crate) fn git_ok<S: AsRef<OsStr>>(dir: &Path, args: &[S]) -> bool {
    exec(dir, args, Opts::default()).is_ok()
}

fn command<S: AsRef<OsStr>>(dir: &Path, args: &[S]) -> Command {
    let mut cmd = Command::new("git");
    cmd.arg("-C")
        .arg(dir)
        .args(args)
        .stdin(Stdio::null())
        // A pager would leave the command waiting for a reader that does not exist.
        .env("GIT_PAGER", "cat")
        .env("GIT_TERMINAL_PROMPT", "0")
        // Same reason as the password prompt: no command launched here has a
        // message to be written, and those that would open an editor —
        // `merge --continue`, `rebase --continue` — would block the worker
        // forever on an editor nobody sees. `true` exits with zero without
        // changing anything, which git reads as "the message is fine".
        .env("GIT_EDITOR", "true")
        .env("GIT_SEQUENCE_EDITOR", "true")
        // Porcelain outputs are stable, but the error messages we display as
        // they are are not: reading them in English avoids depending on the
        // machine's locale to recognise them.
        .env("LC_ALL", "C")
        // **Do not rewrite the index in passing.** `git status` refreshes the
        // `stat` information it caches there, which touches `.git/index` —
        // which we watch. Every read therefore triggered the next: a `git
        // status` every sixty milliseconds, in a loop, and a file list
        // flickering at the same rate.
        //
        // This is the lock git itself calls optional, and this variable exists
        // for tools that poll a repository continuously. Writes take the real
        // lock and are not affected.
        .env("GIT_OPTIONAL_LOCKS", "0");
    cmd
}

fn describe<S: AsRef<OsStr>>(args: &[S]) -> String {
    args.iter()
        .map(|a| a.as_ref().to_string_lossy().into_owned())
        .collect::<Vec<_>>()
        .join(" ")
}

fn strip_trailing_newline(mut s: String) -> String {
    while s.ends_with('\n') || s.ends_with('\r') {
        s.pop();
    }
    s
}

/// Records a repository about to be walked into, and says whether it is the
/// first time.
///
/// For the reads that descend into submodules — status, diff, file list,
/// watch plan: a gitlink can point back at a checkout already on the way down,
/// through a symbolic link or a relative `.git`, and the walk would not end.
/// The path is canonical so that two spellings of one folder are one visit.
pub(crate) fn first_visit(seen: &mut std::collections::HashSet<PathBuf>, dir: &Path) -> bool {
    seen.insert(dir.canonicalize().unwrap_or_else(|_| dir.to_path_buf()))
}

/// Splits a `-z` output (records separated by null bytes).
///
/// The `--porcelain=v1 -z`, `diff --name-status -z` and similar formats exist
/// precisely because a path may contain a newline or a quote; splitting on
/// `\n` works right up until a file is badly named.
pub(crate) fn split_nul(s: &str) -> impl Iterator<Item = &str> {
    s.split('\0').filter(|r| !r.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A command that never returns must not take its worker with it: without
    /// this interruption, three of them freeze the whole application — no more
    /// status, no more diff — without a single message.
    #[test]
    fn a_command_that_never_returns_is_interrupted() {
        let mut cmd = Command::new("sleep");
        cmd.arg("30").stdout(Stdio::piped()).stderr(Stdio::piped());

        let started = Instant::now();
        // Three hundred milliseconds and not the real thirty seconds: the
        // ceiling is an argument since the CI view's shell request wants
        // another one, and that is what removed the test-only override that
        // used to stand here.
        let result = wait_feeding(cmd, None, Duration::from_millis(300), || "sleep 30".into());
        let elapsed = started.elapsed();

        let message = result.expect_err("the command should have been interrupted");
        assert!(
            message.to_string().contains("interrupted"),
            "unexpected message: {message}"
        );
        assert!(
            elapsed < Duration::from_secs(3),
            "the interruption took {elapsed:?}"
        );
    }

    #[test]
    fn a_large_output_is_read_while_waiting() {
        // Far more than a pipe's size: if the outputs were not read during the
        // wait, the process would stay blocked writing and we blocked waiting
        // for it — the classic `spawn` + `wait` deadlock.
        let mut cmd = Command::new("sh");
        cmd.args(["-c", "head -c 2000000 /dev/zero | tr '\\0' 'x'"])
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());

        let out = wait_feeding(cmd, None, TIMEOUT, || "large output".into())
            .expect("the command must finish");
        assert_eq!(out.stdout.len(), 2_000_000);
    }

    /// A process that exits while a grandchild still holds its outputs — the
    /// daemon a hook starts — is answered from what it wrote before exiting,
    /// and not after the daemon dies.
    #[test]
    fn an_exited_process_is_not_waited_for_through_its_daemon() {
        let mut cmd = Command::new("sh");
        cmd.args(["-c", "sleep 20 & echo written; echo said >&2"])
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());

        let started = Instant::now();
        let out = wait_feeding(cmd, None, TIMEOUT, || "daemon".into()).expect("it exited");
        let elapsed = started.elapsed();

        assert!(out.status.success());
        assert_eq!(out.stdout, b"written\n");
        assert_eq!(out.stderr, b"said\n");
        assert!(
            elapsed < LINGER + Duration::from_secs(3),
            "the wait took {elapsed:?}: it waited for the daemon"
        );
    }

    /// Both outputs of a process, tagged `true` for stderr.
    type Streams = Vec<(bool, Box<dyn Read + Send>)>;

    /// Spawns `sh -c line` with both outputs piped, and hands them over tagged.
    fn streams_of(line: &str) -> (std::process::Child, Streams) {
        let mut child = Command::new("sh")
            .args(["-c", line])
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("sh runs");
        let stdout = child.stdout.take().unwrap();
        let stderr = child.stderr.take().unwrap();
        let streams: Streams = vec![(false, Box::new(stdout)), (true, Box::new(stderr))];
        (child, streams)
    }

    /// Each line arrives with the stream it came from, whole, and the end of
    /// both streams is the end of the following.
    #[test]
    fn followed_lines_say_which_stream_they_came_from() {
        let (mut child, streams) = streams_of("echo out; echo err >&2; printf 'caf\\351'");
        let mut heard = Vec::new();
        let end = follow_lines(
            streams,
            Instant::now() + TIMEOUT,
            TIMEOUT,
            || false,
            |is_err, line| {
                heard.push((is_err, line));
                true
            },
        );
        assert_eq!(end, Heard::Closed);
        heard.sort();
        assert_eq!(
            heard,
            [
                (false, "caf\u{fffd}".to_string()),
                (false, "out".to_string()),
                (true, "err".to_string())
            ]
        );
        let _ = child.wait();
    }

    /// A silent process still hears the stop, at the next poll — and the
    /// ceiling, when nothing stops it.
    #[test]
    fn a_silent_process_hears_the_stop_and_the_ceiling() {
        let (mut child, streams) = streams_of("sleep 30");
        let started = Instant::now();
        let mut polls = 0;
        let end = follow_lines(
            streams,
            started + TIMEOUT,
            Duration::from_millis(20),
            || {
                polls += 1;
                polls > 3
            },
            |_, _| true,
        );
        assert_eq!(end, Heard::Enough);
        assert!(started.elapsed() < Duration::from_secs(3));
        let _ = child.kill();
        let _ = child.wait();

        let (mut child, streams) = streams_of("sleep 30");
        let started = Instant::now();
        let end = follow_lines(
            streams,
            started + Duration::from_millis(200),
            TIMEOUT,
            || false,
            |_, _| true,
        );
        assert_eq!(end, Heard::Overran);
        assert!(started.elapsed() < Duration::from_secs(3));
        let _ = child.kill();
        let _ = child.wait();
    }

    /// Valid UTF-8 is taken as it is; anything else is replaced, not lost.
    #[test]
    fn an_output_becomes_text_whether_or_not_it_is_utf8() {
        assert_eq!(into_text(b"caf\xc3\xa9".to_vec()), "café");
        assert_eq!(into_text(b"caf\xe9!".to_vec()), "caf\u{fffd}!");
    }

    /// One runner, and the exit code judged against what each caller accepts:
    /// `diff --no-index` says "they differ" with 1, which `git` refuses and
    /// `git_tolerant` reads as the answer.
    #[test]
    fn the_exit_code_is_judged_against_what_the_caller_accepts() {
        let dir = std::env::temp_dir().join(format!("claudhub-exec-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("a"), "one\n").unwrap();
        std::fs::write(dir.join("b"), "two\n").unwrap();
        let args = ["diff", "--no-index", "--no-color", "a", "b"];

        let refused = git(&dir, &args).expect_err("a difference exits with 1");
        assert!(
            refused.to_string().starts_with("git diff --no-index"),
            "{refused}"
        );
        let diff = git_tolerant(&dir, &args, 1).expect("1 is an answer here");
        assert!(diff.contains("+two"), "{diff}");
        assert!(!git_ok(&dir, &args));
        assert!(git_ok(&dir, &["diff", "--no-index", "a", "a"]));
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Writing and reading at the same time, both past a pipe's size: writing
    /// everything before starting to read would block on both ends at once.
    #[test]
    fn a_large_input_goes_out_while_the_answer_comes_back() {
        let mut cmd = Command::new("cat");
        cmd.stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());

        let input = vec![b'x'; 1_000_000];
        let out = wait_feeding(cmd, Some(input), TIMEOUT, || "cat".into())
            .expect("the command must finish");
        assert!(out.status.success());
        assert_eq!(out.stdout.len(), 1_000_000);
    }
}
