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
use std::time::{Duration, Instant};

use crate::agent_hooks::{Record, Signal};

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

/// What an agent is doing, as far as anything can tell.
///
/// Two sources say it, and they do not say the same things. The processor a
/// process burns tells `Working` from `Idle` and nothing more; the agent's own
/// hooks (`agent_hooks`) also say `Finished` and `Waiting` — the two words one
/// watches five agents for.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum Activity {
    /// Nothing to report: a session waiting for its first prompt, or a guess
    /// that sees no work.
    #[default]
    Idle,
    Working,
    /// It said its turn is over.
    Finished,
    /// It asks the user something — a permission, an answer — in its words.
    Waiting(String),
}

impl Activity {
    /// Which of two agents in one worktree the badge speaks of: the one that
    /// needs you, then the one at work, then the one done.
    fn rank(&self) -> u8 {
        match self {
            Self::Idle => 0,
            Self::Finished => 1,
            Self::Working => 2,
            Self::Waiting(_) => 3,
        }
    }
}

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
    /// True when the activity is `Working` — the one question the views asked
    /// before hooks, kept as a field so they need not match an enum for it.
    pub working: bool,
    pub activity: Activity,
    /// True when an agent's own word stands behind `activity`, not a guess.
    pub heard: bool,
}

/// The usage below which an agent is deemed to be waiting.
///
/// One tick is ten milliseconds of CPU. Three ticks over a two-second interval
/// is roughly one percent of a core: above that, something is happening; below,
/// it is a cursor blinking.
const BUSY_TICKS: u64 = 3;

/// How long a session said to be working may sit idle before its word lapses.
///
/// `Stop` does not fire when the user interrupts a turn, so "working" can
/// outlive the work. An agent at work redraws its spinner several times a
/// second; a full minute without a tick is an agent back at its prompt.
const WORKING_LAPSES: Duration = Duration::from_secs(60);

/// How long a waiting session must burn processor to be taken as answered.
///
/// A granted permission is followed by `PostToolUse` — but only once the tool
/// is done, which for a test suite is minutes of "waiting" on an agent visibly
/// at work. Several busy readings in a row, **after** the question was asked,
/// is the answer having been given.
const ANSWERED: Duration = Duration::from_secs(5);

/// A session seen for the first time is announced only if it spoke this
/// recently: opening a repository reads sessions that finished yesterday.
const FRESH: Duration = Duration::from_secs(10);

/// An agent that has just finished, or starts waiting — worth a balloon.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Transition {
    pub worktree: PathBuf,
    /// `Finished` or `Waiting`: nothing else is announced.
    pub activity: Activity,
}

/// One session's word, and what makes it belong to a process.
#[derive(Debug, Clone)]
struct Session {
    record: Record,
    /// When the word was said, on this machine's clock.
    said_at: Instant,
    /// Its pid has been seen among the agents. From then on the pid's
    /// disappearance is the session's end; before, the pid may be a wrapper's
    /// and says nothing — the worktree still holding an agent is the proof of
    /// life.
    confirmed: bool,
}

/// The processor seen of one process, or of a worktree's: since when it has
/// been busy, or since when idle — one of the two.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
struct Cpu {
    busy_since: Option<Instant>,
    idle_since: Option<Instant>,
}

/// Turns successive readings of `/proc` — and what the agents say through
/// their hooks — into what the sidebar shows.
///
/// It exists because "an agent is working" is not something a reading says: it
/// is the **difference** between two of them. The tracker is what holds the
/// previous one, and it is deliberately free of any view type — this is the one
/// decision of the sidebar that can be tested, and it lives here so the core's
/// test run covers it.
///
/// **An agent's word wins over the guess**, while it holds: see `resolve` for
/// when it lapses. The guess stays for every agent that has no hooks.
#[derive(Debug, Default)]
pub struct Tracker {
    /// CPU time at the previous reading, by pid. Rebuilt whole every time: a
    /// pid that has gone must not keep a slot, and a pid reused by another
    /// program would compare against a stranger.
    cpu: HashMap<u32, u64>,
    /// Each pid's current streak: busy or not, and since when.
    streaks: HashMap<u32, (bool, Instant)>,
    /// The last reading: which processes, where.
    processes: HashMap<PathBuf, Vec<Process>>,
    sessions: HashMap<String, Session>,
    /// A first hearing announces nothing: it is what was already there.
    heard_once: bool,
    now: Option<Instant>,
    states: HashMap<PathBuf, State>,
}

impl Tracker {
    /// Takes a reading of `/proc` in, and works out who is busy.
    pub fn update(&mut self, agents: Agents, now: Instant) {
        let mut cpu = HashMap::new();
        let mut streaks = HashMap::new();
        for process in agents.values().flatten() {
            let before = self.cpu.get(&process.pid).copied();
            // A process seen for the first time has no variation: we call it
            // waiting, and the next reading will decide. The opposite would
            // make the list flicker on every agent that starts.
            let busy =
                before.is_some_and(|before| process.cpu.saturating_sub(before) >= BUSY_TICKS);
            let streak = match self.streaks.get(&process.pid) {
                Some(&(was, since)) if was == busy => (busy, since),
                _ => (busy, now),
            };
            streaks.insert(process.pid, streak);
            cpu.insert(process.pid, process.cpu);
        }
        self.cpu = cpu;
        self.streaks = streaks;
        self.processes = agents;
        self.now = Some(now);
        self.settle();
    }

    /// Takes in what the hooks last wrote, and returns what deserves saying.
    ///
    /// **Once per transition**: a session is announced when its word changes
    /// to `Finished` or `Waiting`, or when the same word is said again later —
    /// a second `Stop` is a second turn over. `idle_prompt` comes a minute
    /// after a `Stop` and is not a new end.
    pub fn hear(&mut self, records: Vec<Record>, now: Instant) -> Vec<Transition> {
        let mut before = std::mem::take(&mut self.sessions);
        let mut told = Vec::new();
        for record in records {
            if record.signal == Signal::Ended {
                continue;
            }
            let previous = before.remove(&record.session);
            let said_at = match &previous {
                Some(previous) if previous.record.stamp == record.stamp => previous.said_at,
                _ => now
                    .checked_sub(Duration::from_millis(record.age_ms))
                    .unwrap_or(now),
            };
            if self.heard_once && news(previous.as_ref().map(|p| &p.record), &record) {
                told.push(record.session.clone());
            }
            self.sessions.insert(
                record.session.clone(),
                Session {
                    confirmed: previous.is_some_and(|p| p.confirmed),
                    record,
                    said_at,
                },
            );
        }
        self.heard_once = true;
        self.now = Some(now);
        self.settle();
        told.into_iter()
            .filter_map(|id| {
                let session = self.sessions.get(&id)?;
                let worktree = self.home_of(session)?;
                let activity = match &session.record.signal {
                    Signal::Waiting(message) => Activity::Waiting(message.clone()),
                    _ => Activity::Finished,
                };
                Some(Transition { worktree, activity })
            })
            .collect()
    }

    /// What is known about this worktree, if anything was found there.
    pub fn get(&self, worktree: &Path) -> Option<&State> {
        self.states.get(worktree)
    }

    /// Where a session lives, if it is alive: the worktree its process was
    /// found in, or — its pid never seen — the one its directory names, as
    /// long as an agent still runs there.
    fn home_of(&self, session: &Session) -> Option<PathBuf> {
        if let Some(pid) = session.record.pid {
            if let Some((worktree, _)) = self
                .processes
                .iter()
                .find(|(_, processes)| processes.iter().any(|p| p.pid == pid))
            {
                return Some(worktree.clone());
            }
        }
        if session.confirmed {
            return None;
        }
        self.processes
            .get(&session.record.worktree)
            .is_some_and(|processes| !processes.is_empty())
            .then(|| session.record.worktree.clone())
    }

    fn cpu_of(&self, pids: impl Iterator<Item = u32>) -> Cpu {
        let mut cpu = Cpu::default();
        let mut busy = false;
        for pid in pids {
            let Some(&(is_busy, since)) = self.streaks.get(&pid) else {
                continue;
            };
            if is_busy {
                busy = true;
                cpu.busy_since = Some(cpu.busy_since.map_or(since, |s| s.min(since)));
            } else {
                cpu.idle_since = Some(cpu.idle_since.map_or(since, |s| s.max(since)));
            }
        }
        if busy {
            cpu.idle_since = None;
        }
        cpu
    }

    /// Rebuilds every worktree's state from the last reading and the last
    /// words heard.
    fn settle(&mut self) {
        let now = self.now.unwrap_or_else(Instant::now);
        // Confirm first: a pid found among the agents is the session's own.
        let running: std::collections::HashSet<u32> =
            self.processes.values().flatten().map(|p| p.pid).collect();
        for session in self.sessions.values_mut() {
            if session.record.pid.is_some_and(|pid| running.contains(&pid)) {
                session.confirmed = true;
            }
        }
        let mut live: HashMap<PathBuf, Vec<&Session>> = HashMap::new();
        for session in self.sessions.values() {
            if let Some(home) = self.home_of(session) {
                live.entry(home).or_default().push(session);
            }
        }
        let mut states = HashMap::with_capacity(self.processes.len());
        for (worktree, processes) in &self.processes {
            let sessions = live.get(worktree).map(Vec::as_slice).unwrap_or_default();
            let claimed: Vec<u32> = sessions
                .iter()
                .filter(|s| s.confirmed)
                .filter_map(|s| s.record.pid)
                .collect();
            let unconfirmed = sessions.iter().filter(|s| !s.confirmed).count();
            let all = processes.iter().map(|p| p.pid);
            let mut candidates: Vec<Activity> = sessions
                .iter()
                .map(|session| {
                    let cpu = match (session.confirmed, session.record.pid) {
                        (true, Some(pid)) => self.cpu_of(std::iter::once(pid)),
                        _ => self.cpu_of(all.clone()),
                    };
                    resolve(&session.record.signal, session.said_at, cpu, now)
                })
                .collect();
            // The agents no word accounts for — no hooks, or another program —
            // are guessed as before.
            let unclaimed: Vec<u32> = processes
                .iter()
                .map(|p| p.pid)
                .filter(|pid| !claimed.contains(pid))
                .collect();
            if unclaimed.len() > unconfirmed {
                let guess = self.cpu_of(unclaimed.into_iter());
                candidates.push(match guess.busy_since {
                    Some(_) => Activity::Working,
                    None => Activity::Idle,
                });
            }
            let activity = candidates
                .into_iter()
                .max_by_key(Activity::rank)
                .unwrap_or_default();
            let mut programs: Vec<String> = processes.iter().map(|p| p.program.clone()).collect();
            programs.sort();
            programs.dedup();
            states.insert(
                worktree.clone(),
                State {
                    count: processes.len(),
                    programs,
                    working: activity == Activity::Working,
                    activity,
                    heard: !sessions.is_empty(),
                },
            );
        }
        self.states = states;
    }
}

/// Whether a session's new word is worth announcing, against its previous one.
fn news(previous: Option<&Record>, record: &Record) -> bool {
    match (&record.signal, previous) {
        // Seen for the first time: only if it has just spoken.
        (Signal::Finished | Signal::Waiting(_) | Signal::Idle, None) => {
            Duration::from_millis(record.age_ms) <= FRESH
        }
        (Signal::Finished | Signal::Waiting(_), Some(previous)) => {
            previous.signal != record.signal || previous.stamp != record.stamp
        }
        // A minute at the prompt after a turn that ended without `Stop` — an
        // interruption — is the end one was not told of.
        (Signal::Idle, Some(previous)) => !matches!(
            previous.signal,
            Signal::Finished | Signal::Idle | Signal::Waiting(_)
        ),
        _ => false,
    }
}

/// What a session's word means now, given what its process has done since.
///
/// Its word stands, with two exceptions, each for an event Claude Code does
/// not send: `Working` lapses after a minute without a tick (an interrupted
/// turn has no `Stop`), and `Waiting` yields to a few seconds of work **after**
/// the question (a granted permission says so only when the tool is done).
fn resolve(signal: &Signal, said_at: Instant, cpu: Cpu, now: Instant) -> Activity {
    let since = |start: Instant| now.saturating_duration_since(start.max(said_at));
    match signal {
        Signal::Working => match cpu.idle_since {
            Some(idle) if since(idle) >= WORKING_LAPSES => Activity::Idle,
            _ => Activity::Working,
        },
        Signal::Waiting(message) => match cpu.busy_since {
            Some(busy) if since(busy) >= ANSWERED => Activity::Working,
            _ => Activity::Waiting(message.clone()),
        },
        Signal::Finished | Signal::Idle => Activity::Finished,
        Signal::Ready | Signal::Ended => Activity::Idle,
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

/// A Claude Code process, as it writes itself down: Claude keeps a file per
/// running process in `~/.claude/sessions/<pid>.json`, naming the
/// conversation it is in and updating it when that changes.
///
/// It is what ties a terminal to its conversation **without our hooks**,
/// which are missing wherever `.claude/settings.local.json` is tracked or the
/// worktree predates the window — and exactly, by pid: `--continue` took the
/// directory's last conversation, which may be another Claude's, running
/// outside Claudhub in the same checkout.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ClaudeProcess {
    pub pid: u32,
    pub session: String,
    pub cwd: PathBuf,
    /// `busy` or `idle`, as Claude says it: what tells a turn still under way
    /// from one done.
    pub status: Option<String>,
}

/// Reads one of those files, field by field — Sentry's rule: a shape that
/// grows fields, or writes `null` in one, must not lose the two we read.
pub fn parse_claude_process(text: &str) -> Option<ClaudeProcess> {
    let value: serde_json::Value = serde_json::from_str(text).ok()?;
    Some(ClaudeProcess {
        pid: u32::try_from(value.get("pid")?.as_u64()?).ok()?,
        session: value.get("sessionId")?.as_str()?.to_string(),
        cwd: PathBuf::from(
            value
                .get("cwd")
                .and_then(|cwd| cwd.as_str())
                .unwrap_or_default(),
        ),
        status: value
            .get("status")
            .and_then(|status| status.as_str())
            .map(str::to_string),
    })
}

/// Every Claude Code process this user runs, from `~/.claude/sessions` —
/// `$CLAUDE_CONFIG_DIR/sessions` when that is set, as Claude reads it. Empty
/// when there is no such folder: an older Claude, or none at all.
pub fn claude_processes() -> Vec<ClaudeProcess> {
    let root = std::env::var_os("CLAUDE_CONFIG_DIR")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".claude")));
    let Some(dir) = root.map(|root| root.join("sessions")) else {
        return Vec::new();
    };
    let Ok(entries) = std::fs::read_dir(&dir) else {
        return Vec::new();
    };
    entries
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| path.extension().is_some_and(|ext| ext == "json"))
        .filter_map(|path| std::fs::read_to_string(path).ok())
        .filter_map(|text| parse_claude_process(&text))
        .collect()
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
pub fn owning_worktree(worktrees: &[PathBuf], cwd: &Path) -> Option<PathBuf> {
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

    #[test]
    fn a_claude_process_file_names_its_pid_and_its_conversation() {
        let text =
            r#"{"pid":1153715,"sessionId":"0023212f","cwd":"/r/wt","status":"idle","name":null}"#;
        assert_eq!(
            parse_claude_process(text),
            Some(ClaudeProcess {
                pid: 1153715,
                session: "0023212f".into(),
                cwd: PathBuf::from("/r/wt"),
                status: Some("idle".into()),
            })
        );
        // Without the two fields it is read for, it is nothing.
        assert_eq!(parse_claude_process(r#"{"pid":1}"#), None);
        assert_eq!(parse_claude_process("not json"), None);
    }

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
        tracker.update(reading(vec![process(1, "claude", 5_000)]), Instant::now());
        let state = tracker.get(Path::new("/p/repo")).expect("the worktree");
        assert_eq!(state.count, 1);
        assert!(!state.working);
    }

    #[test]
    fn burnt_ticks_are_what_makes_an_agent_working() {
        let mut tracker = Tracker::default();
        tracker.update(reading(vec![process(1, "claude", 100)]), Instant::now());
        // Below the threshold: a blinking cursor, not a working agent.
        tracker.update(reading(vec![process(1, "claude", 102)]), Instant::now());
        assert!(!tracker.get(Path::new("/p/repo")).expect("state").working);
        tracker.update(reading(vec![process(1, "claude", 200)]), Instant::now());
        assert!(tracker.get(Path::new("/p/repo")).expect("state").working);
        // And it goes out again once the agent hands back to its prompt.
        tracker.update(reading(vec![process(1, "claude", 200)]), Instant::now());
        assert!(!tracker.get(Path::new("/p/repo")).expect("state").working);
    }

    #[test]
    fn a_counter_that_went_backwards_does_not_underflow() {
        // A reused pid: the new process has burnt less than the old one.
        let mut tracker = Tracker::default();
        tracker.update(reading(vec![process(1, "claude", 9_000)]), Instant::now());
        tracker.update(reading(vec![process(1, "aider", 12)]), Instant::now());
        assert!(!tracker.get(Path::new("/p/repo")).expect("state").working);
    }

    #[test]
    fn the_programs_are_named_once_each_and_in_order() {
        let mut tracker = Tracker::default();
        tracker.update(
            reading(vec![
                process(1, "claude", 0),
                process(2, "aider", 0),
                process(3, "claude", 0),
            ]),
            Instant::now(),
        );
        let state = tracker.get(Path::new("/p/repo")).expect("state");
        assert_eq!(state.count, 3);
        assert_eq!(state.programs, vec!["aider", "claude"]);
    }

    #[test]
    fn a_worktree_with_no_agent_left_is_forgotten() {
        // The states are rebuilt whole: a badge left behind would say an agent
        // is there long after it has gone.
        let mut tracker = Tracker::default();
        tracker.update(reading(vec![process(1, "claude", 0)]), Instant::now());
        tracker.update(Agents::new(), Instant::now());
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

/// The merge of the two sources — what an agent says, what its process does.
#[cfg(test)]
mod hook_tests {
    use super::*;

    const WT: &str = "/p/repo";

    fn agents(processes: &[(u32, u64)]) -> Agents {
        Agents::from([(
            PathBuf::from(WT),
            processes
                .iter()
                .map(|&(pid, cpu)| Process {
                    pid,
                    program: "claude".into(),
                    cpu,
                })
                .collect(),
        )])
    }

    fn said(session: &str, pid: Option<u32>, signal: Signal, stamp: u64) -> Record {
        Record {
            session: session.into(),
            worktree: PathBuf::from(WT),
            pid,
            signal,
            stamp,
            age_ms: 0,
        }
    }

    fn activity(tracker: &Tracker) -> Activity {
        tracker
            .get(Path::new(WT))
            .map(|state| state.activity.clone())
            .unwrap_or_default()
    }

    fn secs(n: u64) -> Duration {
        Duration::from_secs(n)
    }

    #[test]
    fn the_agents_word_wins_over_the_guess() {
        let t0 = Instant::now();
        let mut tracker = Tracker::default();
        tracker.update(agents(&[(10, 100)]), t0);
        tracker.hear(vec![said("s", Some(10), Signal::Finished, 1)], t0);
        // Busy by the processor, finished by its word: the word stands.
        tracker.update(agents(&[(10, 500)]), t0 + secs(2));
        let state = tracker.get(Path::new(WT)).expect("state");
        assert_eq!(state.activity, Activity::Finished);
        assert!(state.heard);
        assert!(!state.working);
    }

    #[test]
    fn without_a_word_the_guess_remains() {
        let t0 = Instant::now();
        let mut tracker = Tracker::default();
        tracker.hear(Vec::new(), t0);
        tracker.update(agents(&[(10, 100)]), t0);
        tracker.update(agents(&[(10, 500)]), t0 + secs(2));
        let state = tracker.get(Path::new(WT)).expect("state");
        assert_eq!(state.activity, Activity::Working);
        assert!(!state.heard);
    }

    #[test]
    fn a_word_lapses_when_its_process_is_gone() {
        let t0 = Instant::now();
        let mut tracker = Tracker::default();
        tracker.update(agents(&[(10, 0), (11, 0)]), t0);
        tracker.hear(
            vec![said("s", Some(10), Signal::Waiting("Bash?".into()), 1)],
            t0,
        );
        assert_eq!(activity(&tracker), Activity::Waiting("Bash?".into()));
        // The session's own process has gone, another agent remains: that one
        // is guessed, the dead session's question is not asked any more.
        tracker.update(agents(&[(11, 0)]), t0 + secs(2));
        assert_eq!(activity(&tracker), Activity::Idle);
    }

    #[test]
    fn an_unconfirmed_pid_lives_as_long_as_its_worktree_has_an_agent() {
        // The hook's `$PPID` was some wrapper: never seen among the agents.
        let t0 = Instant::now();
        let mut tracker = Tracker::default();
        tracker.update(agents(&[(10, 0)]), t0);
        tracker.hear(vec![said("s", Some(999), Signal::Finished, 1)], t0);
        assert_eq!(activity(&tracker), Activity::Finished);
        tracker.update(Agents::new(), t0 + secs(2));
        assert!(tracker.get(Path::new(WT)).is_none());
    }

    #[test]
    fn working_lapses_after_a_minute_at_rest() {
        // An interrupted turn has no `Stop`.
        let t0 = Instant::now();
        let mut tracker = Tracker::default();
        tracker.update(agents(&[(10, 0)]), t0);
        tracker.hear(vec![said("s", Some(10), Signal::Working, 1)], t0);
        tracker.update(agents(&[(10, 0)]), t0 + secs(30));
        assert_eq!(activity(&tracker), Activity::Working);
        tracker.update(agents(&[(10, 0)]), t0 + secs(61));
        assert_eq!(activity(&tracker), Activity::Idle);
    }

    #[test]
    fn a_question_is_answered_by_work_after_it_not_before() {
        let t0 = Instant::now();
        let mut tracker = Tracker::default();
        tracker.update(agents(&[(10, 0)]), t0);
        // Busy for a while before the question...
        tracker.update(agents(&[(10, 100)]), t0 + secs(2));
        tracker.update(agents(&[(10, 200)]), t0 + secs(8));
        // ...then the question, heard at the next sweep with its work behind.
        tracker.hear(
            vec![said("s", Some(10), Signal::Waiting("Bash?".into()), 1)],
            t0 + secs(9),
        );
        tracker.update(agents(&[(10, 300)]), t0 + secs(10));
        assert_eq!(activity(&tracker), Activity::Waiting("Bash?".into()));
        // Still busy five seconds **after** it was asked: it was answered.
        tracker.update(agents(&[(10, 400)]), t0 + secs(12));
        tracker.update(agents(&[(10, 500)]), t0 + secs(15));
        assert_eq!(activity(&tracker), Activity::Working);
    }

    #[test]
    fn the_worktree_speaks_of_the_agent_that_needs_you() {
        let t0 = Instant::now();
        let mut tracker = Tracker::default();
        tracker.update(agents(&[(10, 0), (11, 0)]), t0);
        tracker.hear(
            vec![
                said("a", Some(10), Signal::Finished, 1),
                said("b", Some(11), Signal::Waiting("Edit?".into()), 1),
            ],
            t0,
        );
        let state = tracker.get(Path::new(WT)).expect("state");
        assert_eq!(state.count, 2);
        assert_eq!(state.activity, Activity::Waiting("Edit?".into()));
    }

    #[test]
    fn an_agent_without_hooks_beside_one_with_them_is_still_guessed() {
        let t0 = Instant::now();
        let mut tracker = Tracker::default();
        tracker.update(agents(&[(10, 0), (11, 0)]), t0);
        tracker.hear(vec![said("a", Some(10), Signal::Finished, 1)], t0);
        tracker.update(agents(&[(10, 0), (11, 400)]), t0 + secs(2));
        assert_eq!(activity(&tracker), Activity::Working);
    }

    #[test]
    fn each_transition_is_announced_once() {
        let t0 = Instant::now();
        let mut tracker = Tracker::default();
        tracker.update(agents(&[(10, 0)]), t0);
        // What was there when the window opened is not news.
        assert!(tracker
            .hear(vec![said("s", Some(10), Signal::Finished, 1)], t0)
            .is_empty());
        let told = tracker.hear(vec![said("s", Some(10), Signal::Working, 2)], t0);
        assert!(told.is_empty());
        let told = tracker.hear(vec![said("s", Some(10), Signal::Finished, 3)], t0);
        assert_eq!(
            told,
            vec![Transition {
                worktree: PathBuf::from(WT),
                activity: Activity::Finished
            }]
        );
        // The same file read again: nothing new.
        assert!(tracker
            .hear(vec![said("s", Some(10), Signal::Finished, 3)], t0)
            .is_empty());
        // `idle_prompt` a minute after: the same end, not a second one.
        assert!(tracker
            .hear(vec![said("s", Some(10), Signal::Idle, 4)], t0)
            .is_empty());
        // A second turn over, even missed in between: a new `Stop`.
        let told = tracker.hear(vec![said("s", Some(10), Signal::Finished, 5)], t0);
        assert_eq!(told.len(), 1);
        let told = tracker.hear(
            vec![said("s", Some(10), Signal::Waiting("Bash?".into()), 6)],
            t0,
        );
        assert_eq!(told[0].activity, Activity::Waiting("Bash?".into()));
    }

    #[test]
    fn an_old_session_of_a_repository_just_opened_is_not_news() {
        let t0 = Instant::now();
        let mut tracker = Tracker::default();
        tracker.update(agents(&[(10, 0)]), t0);
        tracker.hear(Vec::new(), t0);
        let mut old = said("s", Some(10), Signal::Finished, 1);
        old.age_ms = 3_600_000;
        assert!(tracker.hear(vec![old], t0).is_empty());
        let fresh = said("t", Some(10), Signal::Finished, 1);
        assert_eq!(tracker.hear(vec![fresh], t0).len(), 1);
    }

    #[test]
    fn a_dead_sessions_news_is_not_announced() {
        let t0 = Instant::now();
        let mut tracker = Tracker::default();
        tracker.update(agents(&[(10, 0)]), t0);
        tracker.hear(vec![said("s", Some(10), Signal::Working, 1)], t0);
        tracker.update(Agents::new(), t0 + secs(2));
        assert!(tracker
            .hear(vec![said("s", Some(10), Signal::Finished, 2)], t0 + secs(2))
            .is_empty());
    }
}
