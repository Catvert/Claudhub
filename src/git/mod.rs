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
pub mod repo;
pub mod status;
pub mod tags;

pub use branch::{Branch, BranchKind, Upstream};
pub use diff::{DiffFile, DiffLine, DiffLineKind, FileDiff, Hunk, Range as DiffRange};
pub use history::{Commit, GraphRow, LogRange};
pub use repo::{Pending, Repo, Worktree};
pub use status::{FileStatus, Status, StatusCode, Summary};
pub use tags::Tag;

use std::ffi::OsStr;
use std::io::Read;
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use anyhow::{bail, Context, Result};

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
    let started = Instant::now();
    let out = run(dir, args)?;
    report(dir, args, started.elapsed(), &out);
    if !out.status.success() {
        let stderr = String::from_utf8_lossy(&out.stderr);
        bail!("git {}: {}", describe(args), stderr.trim());
    }
    Ok(strip_trailing_newline(
        String::from_utf8_lossy(&out.stdout).into_owned(),
    ))
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

/// Launches the command and waits for it, without exceeding `TIMEOUT`.
///
/// Both outputs are read by threads: a full pipe blocks the writer, and `git
/// diff` of a large file fills the pipe's sixty-four kilobytes well before it
/// finishes. Reading them after the wait would deadlock — the process waits
/// for us to drain the pipe, we wait for it to finish.
fn run<S: AsRef<OsStr>>(dir: &Path, args: &[S]) -> Result<std::process::Output> {
    let mut cmd = command(dir, args);
    cmd.stdout(Stdio::piped()).stderr(Stdio::piped());
    wait_with_timeout(cmd, TIMEOUT, || format!("git {}", describe(args)))
}

/// Waits for a process to finish, or interrupts it once `limit` passes.
///
/// Separated from `run` so it can be verified: testing it with `git` would
/// need a git command that hangs reproducibly, and there is none.
///
/// `pub(crate)` and taking its ceiling as an argument because it is not only
/// git's any more: a plugin's shell capability waits for a process in exactly
/// the same way, and for the same reason — a full pipe blocks the writer, and
/// reading after the wait is the classic deadlock. What differs is only how
/// long one is willing to wait, which is why the constant became a parameter.
pub(crate) fn wait_with_timeout(
    mut cmd: Command,
    limit: Duration,
    describe: impl Fn() -> String,
) -> Result<std::process::Output> {
    let mut child = cmd
        .spawn()
        .with_context(|| format!("{}: program not found", describe()))?;

    let mut stdout = child.stdout.take().expect("stdout requested as piped");
    let mut stderr = child.stderr.take().expect("stderr requested as piped");
    let out_reader = std::thread::spawn(move || {
        let mut buffer = Vec::new();
        let _ = stdout.read_to_end(&mut buffer);
        buffer
    });
    let err_reader = std::thread::spawn(move || {
        let mut buffer = Vec::new();
        let _ = stderr.read_to_end(&mut buffer);
        buffer
    });

    let deadline = Instant::now() + limit;
    let status = loop {
        match child.try_wait()? {
            Some(status) => break status,
            None if Instant::now() >= deadline => {
                let _ = child.kill();
                let _ = child.wait();
                bail!(
                    "{} did not answer within {:?} and was interrupted",
                    describe(),
                    limit
                );
            }
            // Short enough that a ten-millisecond command does not look like
            // fifty, long enough not to spin.
            None => std::thread::sleep(Duration::from_millis(5)),
        }
    };

    Ok(std::process::Output {
        status,
        stdout: out_reader.join().unwrap_or_default(),
        stderr: err_reader.join().unwrap_or_default(),
    })
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
    let started = Instant::now();
    let out = run(dir, args)?;
    report(dir, args, started.elapsed(), &out);
    let stderr = String::from_utf8_lossy(&out.stderr);
    if !out.status.success() {
        bail!("git {}: {}", describe(args), stderr.trim());
    }
    let stdout = String::from_utf8_lossy(&out.stdout);
    let mut output = String::with_capacity(stderr.len() + stdout.len());
    output.push_str(stderr.trim_end());
    if !output.is_empty() && !stdout.trim().is_empty() {
        output.push('\n');
    }
    output.push_str(stdout.trim_end());
    Ok(output)
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
    let started = Instant::now();
    let out = run(dir, args)?;
    report(dir, args, started.elapsed(), &out);
    let code = out.status.code().unwrap_or(-1);
    if code < 0 || code > max_code {
        let stderr = String::from_utf8_lossy(&out.stderr);
        bail!("git {}: {}", describe(args), stderr.trim());
    }
    Ok(strip_trailing_newline(
        String::from_utf8_lossy(&out.stdout).into_owned(),
    ))
}

/// True if the command exits with code 0. For closed questions
/// (`show-ref --verify --quiet`) whose output interests nobody.
pub(crate) fn git_ok<S: AsRef<OsStr>>(dir: &Path, args: &[S]) -> bool {
    command(dir, args)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
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
        // ceiling is an argument since a plugin's shell capability wants
        // another one, and that is what removed the test-only override that
        // used to stand here.
        let result = wait_with_timeout(cmd, Duration::from_millis(300), || "sleep 30".into());
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

        let out = wait_with_timeout(cmd, TIMEOUT, || "large output".into())
            .expect("the command must finish");
        assert_eq!(out.stdout.len(), 2_000_000);
    }
}
