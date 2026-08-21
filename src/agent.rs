//! Detecting the coding agents running in the worktrees.
//!
//! Claudhub does not launch every agent: one is started from a Claudhub tab,
//! but also from a terminal alongside, and it is the same work we want to see.
//! Detection therefore goes through `/proc` — a process's working directory
//! says which worktree it works in — rather than through the tabs we opened
//! ourselves.
//!
//! Linux only. Elsewhere the list is empty and the sidebar shows nothing more:
//! this is not a feature whose absence breaks anything.

use std::collections::HashMap;
// `Path` is only used by what reads `/proc`, so only on Linux: elsewhere the
// import would be a warning, and the project builds with `-D warnings`.
#[cfg(target_os = "linux")]
use std::path::Path;
use std::path::PathBuf;

/// The markers an agent session leaves in the environment.
///
/// These are Claude Code's, the only agent that sets any today; the list is
/// **explicit** and not a sweep of `CLAUDE_CODE_*`, which would also take the
/// user's configuration (`CLAUDE_CODE_USE_BEDROCK`, `ANTHROPIC_MODEL`, the
/// token limits) — precisely what has to be passed on.
const SESSION_MARKERS: &[&str] = &[
    "AI_AGENT",
    "CLAUDECODE",
    "CLAUDE_CODE_CHILD_SESSION",
    "CLAUDE_CODE_ENTRYPOINT",
    "CLAUDE_CODE_EXECPATH",
    "CLAUDE_CODE_MESSAGING_SOCKET",
    "CLAUDE_CODE_MESSAGING_TOKEN",
    "CLAUDE_CODE_SESSION_ID",
    "CLAUDE_CODE_SSE_PORT",
    "CLAUDE_EFFORT",
    "CLAUDE_PID",
];

/// Clears from our own environment the markers of the session that launched us.
///
/// Launching Claudhub from an agent is the **common** case: an agent writes
/// Claudhub, and it is from its terminal that we try it. Everything we started
/// then inherited its markers, and a `claude` opened in a tab believed itself
/// a sub-session of the one next door — so it no longer recorded its
/// transcript, and said so with nothing to be done about it from the tab.
///
/// Here and not in the pty's environment: the question is not limited to
/// terminals. `wt` runs the project's hooks, `commit_msg` runs an agent in one
/// pass — all of that is started by Claudhub, which is nobody's session.
///
/// **To be called at the very start of `main`**, before any thread exists:
/// `remove_var` touches an environment the process shares, and another thread
/// reading it meanwhile is undefined behaviour.
pub fn disinherit_session() {
    for marker in SESSION_MARKERS {
        std::env::remove_var(marker);
    }
}

/// An agent process found in a worktree.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Process {
    pub pid: u32,
    /// The recognised program, as the profiles name it.
    ///
    /// The sidebar says *which* agent runs and not only how many: with two
    /// profiles, "an agent works here" does not say which, and that is exactly
    /// what one looks for when scanning the list.
    pub program: String,
    /// CPU time consumed since startup, in clock ticks.
    ///
    /// It is a cumulative measure, of no interest by itself: it is its
    /// *variation* between two readings that tells a working agent from one
    /// waiting for the user to answer.
    pub cpu: u64,
}

/// The agents found, by worktree.
pub type Agents = HashMap<PathBuf, Vec<Process>>;

/// Outside Linux there is no `/proc`: the list is empty, and the sidebar
/// simply shows no agent.
///
/// The stub is explicit rather than accidental: the walk below would compile
/// everywhere and fail silently on opening `/proc`, which reads like broken
/// detection rather than a deliberate absence.
#[cfg(not(target_os = "linux"))]
pub fn scan(_worktrees: &[PathBuf], _programs: &[String]) -> Agents {
    Agents::new()
}

/// Walks `/proc` looking for the agents launched in these worktrees.
///
/// `programs` are the command names of **all** configured profiles, not of a
/// single one: an agent launched from a terminal alongside counts as much as
/// the one started here, and looking for only one would see only half of them.
#[cfg(target_os = "linux")]
pub fn scan(worktrees: &[PathBuf], programs: &[String]) -> Agents {
    let mut found: Agents = HashMap::new();
    let programs: Vec<&str> = programs
        .iter()
        .map(|program| command_name(program))
        .filter(|program| !program.is_empty())
        .collect();
    if programs.is_empty() {
        return found;
    }
    let Ok(entries) = std::fs::read_dir("/proc") else {
        return found;
    };
    for entry in entries.flatten() {
        let Some(pid) = entry
            .file_name()
            .to_str()
            .and_then(|n| n.parse::<u32>().ok())
        else {
            continue;
        };
        let dir = entry.path();
        let Some(program) = programs
            .iter()
            .find(|program| matches_program(&dir, program))
            .map(|program| program.to_string())
        else {
            continue;
        };
        // A process's working directory is the worktree it works in; an
        // unresolved symlink — vanished process, permissions — simply makes it
        // skipped.
        let Ok(cwd) = std::fs::read_link(dir.join("cwd")) else {
            continue;
        };
        let Some(worktree) = owning_worktree(worktrees, &cwd) else {
            continue;
        };
        let cpu = std::fs::read_to_string(dir.join("stat"))
            .ok()
            .and_then(|stat| parse_cpu_ticks(&stat))
            .unwrap_or(0);
        found
            .entry(worktree)
            .or_default()
            .push(Process { pid, program, cpu });
    }
    found
}

/// The command name, stripped of its path and its arguments.
pub fn command_name(command: &str) -> &str {
    let program = command.split_whitespace().next().unwrap_or("");
    program.rsplit('/').next().unwrap_or(program)
}

/// True if this process is the agent we are looking for.
///
/// The name alone (`comm`) is not enough: an agent launched by a script or by
/// a node version manager is called `node`, and it is its command line that
/// carries `claude`.
#[cfg(target_os = "linux")]
fn matches_program(proc_dir: &Path, program: &str) -> bool {
    if let Ok(comm) = std::fs::read_to_string(proc_dir.join("comm")) {
        if comm.trim() == program {
            return true;
        }
    }
    let Ok(cmdline) = std::fs::read(proc_dir.join("cmdline")) else {
        return false;
    };
    cmdline_matches(&cmdline, program)
}

/// `/proc/<pid>/cmdline` separates the arguments with null bytes.
#[cfg(target_os = "linux")]
fn cmdline_matches(cmdline: &[u8], program: &str) -> bool {
    cmdline
        .split(|b| *b == 0)
        .filter_map(|arg| std::str::from_utf8(arg).ok())
        .any(|arg| arg.rsplit('/').next().unwrap_or(arg) == program)
}

/// The deepest worktree containing this directory.
///
/// The deepest, and not the first found: a worktree nested in another would
/// otherwise hand its agents to the wrong one.
#[cfg(target_os = "linux")]
fn owning_worktree(worktrees: &[PathBuf], cwd: &Path) -> Option<PathBuf> {
    worktrees
        .iter()
        .filter(|worktree| cwd.starts_with(worktree))
        .max_by_key(|worktree| worktree.as_os_str().len())
        .cloned()
}

/// A process's cumulative CPU time, from `/proc/<pid>/stat`.
///
/// The program name is the second field, in parentheses, and it **may contain
/// spaces and parentheses**: splitting the line on whitespace shifts every
/// field as soon as a program is called "(my agent)". So we start again from
/// the last closing parenthesis.
#[cfg(target_os = "linux")]
pub fn parse_cpu_ticks(stat: &str) -> Option<u64> {
    let rest = &stat[stat.rfind(')')? + 1..];
    let fields: Vec<&str> = rest.split_whitespace().collect();
    // After the name come state, ppid, pgrp, session, tty, tpgid, flags, then
    // the four page-fault counters: `utime` is the 12th field of that
    // remainder, `stime` the 13th.
    let utime: u64 = fields.get(11)?.parse().ok()?;
    let stime: u64 = fields.get(12)?.parse().ok()?;
    Some(utime + stime)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_program_name_ignores_its_path_and_arguments() {
        assert_eq!(command_name("claude"), "claude");
        assert_eq!(command_name("/usr/bin/claude --resume"), "claude");
        assert_eq!(command_name(""), "");
    }
}

/// What speaks the `/proc` format only compiles — and is only tested — where
/// `/proc` exists.
#[cfg(all(test, target_os = "linux"))]
mod proc_tests {
    use super::*;

    #[test]
    fn the_command_line_is_matched_argument_by_argument() {
        // An agent launched through node: the second argument is what names it.
        let cmdline = b"/nix/store/x/bin/node\0/home/a/.bun/bin/claude\0--resume\0";
        assert!(cmdline_matches(cmdline, "claude"));
        assert!(!cmdline_matches(cmdline, "aider"));
        // A partial match is not a match: `claudia` is not `claude`.
        assert!(!cmdline_matches(b"/usr/bin/claudia\0", "claude"));
    }

    #[test]
    fn cpu_ticks_survive_a_program_name_full_of_parentheses() {
        // The case every naive /proc parser gets wrong.
        let stat = "42 (my (funny) agent) S 1 42 42 0 -1 4194304 100 0 0 0 \
                    130 27 0 0 20 0 12 0 999";
        assert_eq!(parse_cpu_ticks(stat), Some(157));
    }

    #[test]
    fn the_deepest_worktree_claims_the_process() {
        let worktrees = vec![PathBuf::from("/p/repo"), PathBuf::from("/p/repo/nested")];
        assert_eq!(
            owning_worktree(&worktrees, Path::new("/p/repo/nested/src")),
            Some(PathBuf::from("/p/repo/nested"))
        );
        assert_eq!(
            owning_worktree(&worktrees, Path::new("/p/repo/src")),
            Some(PathBuf::from("/p/repo"))
        );
        assert_eq!(owning_worktree(&worktrees, Path::new("/elsewhere")), None);
    }
}
