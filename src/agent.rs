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
use std::path::{Path, PathBuf};

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

/// What is known about a worktree's agents, as the sidebar shows it.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct State {
    pub count: usize,
    /// The agents found, by program name and without duplicates.
    ///
    /// The sidebar says *which* agent runs: with two profiles, "an agent works
    /// here" does not say which, and that is precisely what one looks at while
    /// scanning the list.
    pub programs: Vec<String>,
    /// True when at least one agent has used CPU since the previous reading.
    ///
    /// It is an accepted approximation: nothing in a process says "I am
    /// thinking" or "I am waiting for an answer". An agent at work redraws its
    /// display several times a second and is seen; an agent waiting for the
    /// user's answer costs nothing.
    pub working: bool,
}

/// The usage below which an agent is deemed to be waiting.
///
/// One tick is ten milliseconds of CPU. Three ticks over a two-second interval
/// is roughly one percent of a core: above that, something is happening; below,
/// it is a cursor blinking.
const BUSY_TICKS: u64 = 3;

/// Turns successive readings of `/proc` into what the sidebar shows.
///
/// It exists because "an agent is working" is not something a reading says: it
/// is the **difference** between two of them. The tracker is what holds the
/// previous one, and it is deliberately free of any view type — this is the one
/// decision of the sidebar that can be tested, and it lives here so the core's
/// test run covers it.
#[derive(Debug, Default)]
pub struct Tracker {
    /// CPU time at the previous reading, by pid. Rebuilt whole every time: a
    /// pid that has gone must not keep a slot, and a pid reused by another
    /// program would compare against a stranger.
    cpu: HashMap<u32, u64>,
    states: HashMap<PathBuf, State>,
}

impl Tracker {
    /// Takes a reading in, and works out who is busy.
    pub fn update(&mut self, agents: Agents) {
        let mut states = HashMap::with_capacity(agents.len());
        let mut cpu = HashMap::new();
        for (worktree, processes) in agents {
            let working = processes.iter().any(|process| {
                let before = self.cpu.get(&process.pid).copied();
                // A process seen for the first time has no variation: we call it
                // waiting, and the next reading will decide. The opposite would
                // make the list flicker on every agent that starts.
                before.is_some_and(|before| process.cpu.saturating_sub(before) >= BUSY_TICKS)
            });
            for process in &processes {
                cpu.insert(process.pid, process.cpu);
            }
            let mut programs: Vec<String> = processes
                .iter()
                .map(|process| process.program.clone())
                .collect();
            programs.sort();
            programs.dedup();
            states.insert(
                worktree,
                State {
                    count: processes.len(),
                    programs,
                    working,
                },
            );
        }
        self.cpu = cpu;
        self.states = states;
    }

    /// What is known about this worktree, if anything was found there.
    pub fn get(&self, worktree: &Path) -> Option<&State> {
        self.states.get(worktree)
    }
}

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
        // **The working directory first.** It is one `readlink`, and it rules
        // out ninety-nine processes in a hundred — naming the program means
        // reading two files, and it used to be done for every process on the
        // machine before this filter had a say.
        //
        // A process's working directory is the worktree it works in; an
        // unresolved symlink — vanished process, permissions — simply makes it
        // skipped.
        let Ok(cwd) = std::fs::read_link(dir.join("cwd")) else {
            continue;
        };
        let Some(worktree) = owning_worktree(worktrees, &cwd) else {
            continue;
        };
        let Some(program) = program_of(&dir, &programs) else {
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

/// Which of the profiles this process answers to, if any.
///
/// The name alone (`comm`) is not enough: an agent launched by a script or by
/// a node version manager is called `node`, and it is its command line that
/// carries `claude`. Both files are read **once**, and then compared to every
/// profile — reading them per profile was the same two files three times over.
#[cfg(target_os = "linux")]
fn program_of(proc_dir: &Path, programs: &[&str]) -> Option<String> {
    let comm = std::fs::read_to_string(proc_dir.join("comm")).unwrap_or_default();
    let comm = comm.trim();
    let cmdline = std::fs::read(proc_dir.join("cmdline")).unwrap_or_default();
    programs
        .iter()
        .find(|program| comm == **program || cmdline_matches(&cmdline, program))
        .map(|program| program.to_string())
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

    // Reads what only exists under `/proc`.
    #[cfg(target_os = "linux")]
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

    // Reads what only exists under `/proc`.
    #[cfg(target_os = "linux")]
    #[test]
    fn cpu_ticks_survive_a_program_name_full_of_parentheses() {
        // The case every naive /proc parser gets wrong.
        let stat = "42 (my (funny) agent) S 1 42 42 0 -1 4194304 100 0 0 0 \
                    130 27 0 0 20 0 12 0 999";
        assert_eq!(parse_cpu_ticks(stat), Some(157));
    }

    fn process(pid: u32, program: &str, cpu: u64) -> Process {
        Process {
            pid,
            program: program.into(),
            cpu,
        }
    }

    fn reading(processes: Vec<Process>) -> Agents {
        Agents::from([(PathBuf::from("/p/repo"), processes)])
    }

    #[test]
    fn a_first_reading_never_says_working() {
        // Nothing to compare against yet: calling it busy would light up every
        // agent the moment it starts, and go out on the next reading.
        let mut tracker = Tracker::default();
        tracker.update(reading(vec![process(1, "claude", 5_000)]));
        let state = tracker.get(Path::new("/p/repo")).expect("the worktree");
        assert_eq!(state.count, 1);
        assert!(!state.working);
    }

    #[test]
    fn burnt_ticks_are_what_makes_an_agent_working() {
        let mut tracker = Tracker::default();
        tracker.update(reading(vec![process(1, "claude", 100)]));
        // Below the threshold: a blinking cursor, not a working agent.
        tracker.update(reading(vec![process(1, "claude", 102)]));
        assert!(!tracker.get(Path::new("/p/repo")).expect("state").working);
        tracker.update(reading(vec![process(1, "claude", 200)]));
        assert!(tracker.get(Path::new("/p/repo")).expect("state").working);
        // And it goes out again once the agent hands back to its prompt.
        tracker.update(reading(vec![process(1, "claude", 200)]));
        assert!(!tracker.get(Path::new("/p/repo")).expect("state").working);
    }

    #[test]
    fn a_counter_that_went_backwards_does_not_underflow() {
        // A reused pid: the new process has burnt less than the old one.
        let mut tracker = Tracker::default();
        tracker.update(reading(vec![process(1, "claude", 9_000)]));
        tracker.update(reading(vec![process(1, "aider", 12)]));
        assert!(!tracker.get(Path::new("/p/repo")).expect("state").working);
    }

    #[test]
    fn the_programs_are_named_once_each_and_in_order() {
        let mut tracker = Tracker::default();
        tracker.update(reading(vec![
            process(1, "claude", 0),
            process(2, "aider", 0),
            process(3, "claude", 0),
        ]));
        let state = tracker.get(Path::new("/p/repo")).expect("state");
        assert_eq!(state.count, 3);
        assert_eq!(state.programs, vec!["aider", "claude"]);
    }

    #[test]
    fn a_worktree_with_no_agent_left_is_forgotten() {
        // The states are rebuilt whole: a badge left behind would say an agent
        // is there long after it has gone.
        let mut tracker = Tracker::default();
        tracker.update(reading(vec![process(1, "claude", 0)]));
        tracker.update(Agents::new());
        assert!(tracker.get(Path::new("/p/repo")).is_none());
    }

    // Reads what only exists under `/proc`.
    #[cfg(target_os = "linux")]
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
