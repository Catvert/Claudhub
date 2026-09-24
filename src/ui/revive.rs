//! Terminals that outlive the window: which session an agent's terminal is
//! running, and the command that takes it back up.
//!
//! **Only what can be started again without asking is kept**: a shell, and an
//! agent. A terminal launched on a command — a `wt` task, a `just` recipe —
//! is not: running `just deploy` again because the window was closed on it
//! would be a restart nobody asked for.
//!
//! **An agent comes back in its conversation**, which the hooks name: every
//! record carries the session and the pid of the agent that wrote it, and the
//! pty's child *is* the agent — `sh -lc` execs what it is given. Under WSL the
//! pty is a Windows process and the agent a Linux one, and no pid ties them;
//! there, a worktree with a single agent terminal takes the freshest session
//! heard in it.
//!
//! Pure: the view hands in its terminals and the records, this says which is
//! which.

use std::path::{Path, PathBuf};

/// True for Claude Code, the one agent whose resumption we know how to ask
/// for. A profile names a program: `claude`, a path to it, or a wrapper
/// whose name says it.
pub fn is_claude(program: &str) -> bool {
    Path::new(program)
        .file_name()
        .and_then(|name| name.to_str())
        .is_some_and(|name| name.starts_with("claude"))
}

/// The command that starts a kept agent again, in its conversation when
/// there is one to go back to and the agent is one we know how to ask.
///
/// **The hooks' session first** (`--resume`). Without one — the hooks are not
/// installed where `.claude/settings.local.json` is a tracked file, and
/// nothing then names a session — the worktree's **last** conversation
/// (`--continue`), as long as the worktree had one agent: two would both
/// continue the same one. And either way, **a fresh start when nothing is
/// found**: a `--resume` of a session Claude no longer has, or a `--continue`
/// where nothing was ever said, exits on an error, and the tab would be dead
/// on arrival. Hence `sh -c '<resume> || exec <fresh>'`.
pub fn relaunch(
    program: &str,
    args: &[String],
    session: Option<&str>,
    alone: bool,
) -> (String, Vec<String>) {
    let args = without_resume(args);
    let resume: Vec<String> = match session {
        _ if !is_claude(program) => Vec::new(),
        Some(session) => vec!["--resume".into(), session.into()],
        None if alone => vec!["--continue".into()],
        None => Vec::new(),
    };
    if resume.is_empty() {
        return (program.to_string(), args);
    }
    let fresh: Vec<&str> = std::iter::once(program)
        .chain(args.iter().map(String::as_str))
        .collect();
    let resumed: Vec<&str> = fresh
        .iter()
        .copied()
        .chain(resume.iter().map(String::as_str))
        .collect();
    let line = format!(
        "{} || exec {}",
        crate::cmdline::join_command(&resumed),
        crate::cmdline::join_command(&fresh)
    );
    ("sh".to_string(), vec!["-c".to_string(), line])
}

/// A profile's arguments without a `--resume` of their own: what is kept is
/// the profile's command, and the session is added back on the way out —
/// kept once, it would be resumed twice, with two ids.
pub fn without_resume(args: &[String]) -> Vec<String> {
    let mut kept = Vec::with_capacity(args.len());
    let mut skip = false;
    for arg in args {
        if skip {
            skip = false;
            continue;
        }
        if arg == "--resume" || arg == "-r" {
            skip = true;
            continue;
        }
        if arg.starts_with("--resume=") {
            continue;
        }
        kept.push(arg.clone());
    }
    kept
}

/// An open agent terminal, as the matching sees it.
#[derive(Debug, Clone)]
pub struct Open {
    pub worktree: PathBuf,
    /// The pty's child — the agent itself on Linux.
    pub pid: Option<u32>,
}

/// A session the hooks have heard of.
#[derive(Debug, Clone)]
pub struct Heard {
    pub session: String,
    pub worktree: PathBuf,
    pub pid: Option<u32>,
    /// How long ago it spoke: the freshest is the one being worked in.
    pub age_ms: u64,
}

/// Which session each agent terminal runs, by index into `open`.
///
/// The pid first, which is certain. Then, for a worktree where one agent
/// terminal is left without an answer, the freshest session of that worktree
/// no pid claimed: under WSL no pid matches, and one agent per worktree is
/// the common case. Two agents in one worktree with no pid to tell them
/// apart get nothing — a wrong conversation resumed is worse than a fresh one.
pub fn sessions(open: &[Open], heard: &[Heard]) -> Vec<(usize, String)> {
    let mut found: Vec<(usize, String)> = Vec::new();
    let mut claimed: Vec<&str> = Vec::new();
    for (index, terminal) in open.iter().enumerate() {
        let Some(pid) = terminal.pid else { continue };
        if let Some(record) = heard.iter().find(|h| h.pid == Some(pid)) {
            found.push((index, record.session.clone()));
            claimed.push(&record.session);
        }
    }
    for (index, terminal) in open.iter().enumerate() {
        if found.iter().any(|(i, _)| *i == index) {
            continue;
        }
        let alone = open
            .iter()
            .enumerate()
            .filter(|(i, t)| t.worktree == terminal.worktree && !found.iter().any(|(f, _)| f == i))
            .count()
            == 1;
        if !alone {
            continue;
        }
        let freshest = heard
            .iter()
            .filter(|h| h.worktree == terminal.worktree && !claimed.contains(&h.session.as_str()))
            .min_by_key(|h| h.age_ms);
        if let Some(record) = freshest {
            found.push((index, record.session.clone()));
            claimed.push(&record.session);
        }
    }
    found
}

#[cfg(test)]
mod tests {
    use super::*;

    fn strings(args: &[&str]) -> Vec<String> {
        args.iter().map(|a| a.to_string()).collect()
    }

    fn shell(line: &str) -> (String, Vec<String>) {
        ("sh".into(), strings(&["-c", line]))
    }

    #[test]
    fn claude_is_resumed_in_its_session_or_started_fresh() {
        assert_eq!(
            relaunch("claude", &strings(&["--model", "opus"]), Some("abc"), false),
            shell("claude --model opus --resume abc || exec claude --model opus")
        );
        // No session named, one agent in the worktree: its last conversation.
        assert_eq!(
            relaunch("/usr/local/bin/claude", &[], None, true),
            shell("/usr/local/bin/claude --continue || exec /usr/local/bin/claude")
        );
        // Two agents and no names: a fresh conversation, not the same one twice.
        assert_eq!(
            relaunch("claude", &[], None, false),
            ("claude".to_string(), Vec::new())
        );
        // Another agent: its command as it was.
        assert_eq!(
            relaunch("codex", &strings(&["x"]), Some("abc"), true),
            ("codex".to_string(), strings(&["x"]))
        );
    }

    #[test]
    fn a_resume_already_in_the_profile_is_replaced_not_doubled() {
        assert_eq!(
            relaunch(
                "claude",
                &strings(&["--resume", "old", "-p"]),
                Some("new"),
                true
            ),
            shell("claude -p --resume new || exec claude -p")
        );
        assert_eq!(
            without_resume(&strings(&["--resume=old", "-r", "x", "--verbose"])),
            strings(&["--verbose"])
        );
    }

    fn open(worktree: &str, pid: Option<u32>) -> Open {
        Open {
            worktree: PathBuf::from(worktree),
            pid,
        }
    }

    fn heard(session: &str, worktree: &str, pid: Option<u32>, age_ms: u64) -> Heard {
        Heard {
            session: session.into(),
            worktree: PathBuf::from(worktree),
            pid,
            age_ms,
        }
    }

    #[test]
    fn a_pid_says_which_session_a_terminal_runs() {
        let terminals = [open("/a", Some(10)), open("/a", Some(20))];
        let records = [
            heard("one", "/a", Some(20), 5),
            heard("two", "/a", Some(10), 50),
        ];
        assert_eq!(
            sessions(&terminals, &records),
            vec![(0, "two".to_string()), (1, "one".to_string())]
        );
    }

    #[test]
    fn without_a_pid_a_lone_terminal_takes_the_freshest_session_of_its_worktree() {
        // WSL: the pty's pid is a Windows one.
        let terminals = [open("/a", Some(9999)), open("/b", None)];
        let records = [
            heard("old", "/a", Some(1), 90_000),
            heard("new", "/a", Some(2), 1_000),
            heard("elsewhere", "/c", Some(3), 10),
        ];
        assert_eq!(sessions(&terminals, &records), vec![(0, "new".to_string())]);
    }

    #[test]
    fn two_agents_in_one_worktree_without_pids_get_no_guess() {
        let terminals = [open("/a", None), open("/a", None)];
        let records = [
            heard("one", "/a", Some(1), 10),
            heard("two", "/a", Some(2), 20),
        ];
        assert!(sessions(&terminals, &records).is_empty());
    }

    #[test]
    fn a_session_claimed_by_its_pid_is_not_guessed_for_another() {
        let terminals = [open("/a", Some(1)), open("/b", None)];
        let records = [heard("one", "/a", Some(1), 10)];
        assert_eq!(sessions(&terminals, &records), vec![(0, "one".to_string())]);
    }
}
