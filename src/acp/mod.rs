//! The Agent Client Protocol: a coding agent spoken to as a chat, over
//! newline-delimited JSON-RPC on its stdin and stdout.
//!
//! **Why it lives in the core.** Like the language servers, an agent is a
//! process that has to run *where the files are* — inside the WSL distribution
//! when the interface is a Windows `.exe` — and it reads and writes the
//! worktree on its own. The view never launches it: it sends `Cmd::Acp*` and
//! reads `Evt::Acp*`, and the wire carries them like everything else.
//!
//! **Two halves.** [`Host`] is the lane: one process per chat, and nothing but
//! lines going in and coming out — it knows no method, holds no request. What
//! the lines mean is [`chat::Chat`], pure: the conversation, the handshake, the
//! answers owed. The view holds a `Chat`, feeds it what arrives and sends what
//! it returns. The protocol is thus tested without a process, and the server
//! never has to agree with the window on a version of anything but a line.
//!
//! **Why not a queue.** A chat lives for hours and pushes what nobody asked
//! for, and its lines must reach the agent in the order they were written: a
//! permission answer must not overtake the prompt it belongs to. A lane of its
//! own, beside the watcher's and the language servers'.

pub mod chat;

use std::collections::{BTreeMap, HashMap, VecDeque};
use std::io::{BufRead, BufReader, Write};
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Sender};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

use crate::runtime::protocol::{Evt, WorktreeId};

/// How many of the agent's stderr lines are kept to explain its death.
///
/// An agent that exits at once — a missing `node`, a package that will not
/// install, a login it wants — says why on stderr, and the tail is the part
/// that says it.
const STDERR_LINES: usize = 40;

/// How long an agent whose stdout closed is given to exit before it is killed.
const EXIT_GRACE: Duration = Duration::from_secs(2);

/// An agent as it is declared in the settings: a program speaking ACP on its
/// standard streams.
///
/// `command` and `args` are separate, for the reason the terminal's profiles
/// give: a command line split on spaces breaks on the first path holding one.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Agent {
    /// What the tab and the menu show. Empty, the program's name.
    pub name: String,
    pub command: String,
    pub args: Vec<String>,
    /// Added to the agent's environment. Sorted, so the settings file does not
    /// change with nothing having changed.
    pub env: BTreeMap<String, String>,
}

impl Agent {
    /// Claude, through the ACP adapter published on npm.
    pub fn claude() -> Self {
        Self::npx("Claude", "@agentclientprotocol/claude-agent-acp", &[])
    }

    /// Codex, through its adapter.
    pub fn codex() -> Self {
        Self::npx("Codex", "@agentclientprotocol/codex-acp", &[])
    }

    /// Gemini CLI, which speaks ACP itself behind a flag.
    pub fn gemini() -> Self {
        Self::npx("Gemini", "@google/gemini-cli", &["--acp"])
    }

    /// The agents offered before anyone has configured one.
    pub fn defaults() -> Vec<Self> {
        vec![Self::claude(), Self::codex(), Self::gemini()]
    }

    fn npx(name: &str, package: &str, args: &[&str]) -> Self {
        let mut all = vec!["-y".to_string(), package.to_string()];
        all.extend(args.iter().map(|arg| arg.to_string()));
        Self {
            name: name.into(),
            command: "npx".into(),
            args: all,
            env: BTreeMap::new(),
        }
    }

    /// The displayed name: its own, or the program's.
    pub fn label(&self) -> &str {
        if self.name.trim().is_empty() {
            Path::new(&self.command)
                .file_name()
                .and_then(|name| name.to_str())
                .unwrap_or(&self.command)
        } else {
            &self.name
        }
    }

    /// Whether there is anything to launch.
    pub fn is_runnable(&self) -> bool {
        !self.command.trim().is_empty()
    }
}

/// The running chats, one process each, keyed by the view's chat number.
pub struct Host {
    chats: Mutex<HashMap<u64, Running>>,
    events: async_channel::Sender<Evt>,
}

struct Running {
    /// Whole lines, to the thread that writes them.
    input: Sender<String>,
    child: Arc<Mutex<Child>>,
    /// Set when the view ended the chat: the death that follows is ours, and
    /// says nothing.
    stopped: Arc<AtomicBool>,
}

impl Host {
    pub fn new(events: async_channel::Sender<Evt>) -> Self {
        Self {
            chats: Mutex::new(HashMap::new()),
            events,
        }
    }

    /// Launches an agent for a chat, replacing any process that chat had.
    pub fn start(&self, chat: u64, worktree: WorktreeId, agent: Agent) {
        self.stop(chat);
        match launch(chat, &worktree, &agent, self.events.clone()) {
            Ok(running) => {
                self.chats.lock().unwrap().insert(chat, running);
            }
            Err(e) => {
                log::warn!("agent {}: {e:#}", agent.label());
                let _ = self.events.try_send(Evt::AcpEnded {
                    chat,
                    reason: Some(format!("{}: {e:#}", agent.command)),
                });
            }
        }
    }

    /// Hands a line to a chat's agent. A chat with no process says so: the
    /// view is waiting on an answer that will never come otherwise.
    pub fn send(&self, chat: u64, line: String) {
        let mut chats = self.chats.lock().unwrap();
        let sent = chats
            .get(&chat)
            .is_some_and(|running| running.input.send(line).is_ok());
        if !sent {
            chats.remove(&chat);
            drop(chats);
            let _ = self.events.try_send(Evt::AcpEnded {
                chat,
                reason: Some("the agent is not running".into()),
            });
        }
    }

    /// Ends a chat's process, and everything it started.
    pub fn stop(&self, chat: u64) {
        let Some(running) = self.chats.lock().unwrap().remove(&chat) else {
            return;
        };
        running.stopped.store(true, Ordering::SeqCst);
        drop(running.input);
        kill(&running.child);
    }
}

/// Kills the agent's whole group: what is declared is usually `npx`, whose
/// child is the agent, and `Child::kill` reaches the launcher alone — the same
/// lesson as the language servers'.
fn kill(child: &Mutex<Child>) {
    let mut child = child.lock().unwrap();
    #[cfg(unix)]
    // SAFETY: the pid is still the child's own until it is reaped, and the
    // group it leads is the one `launch` made.
    unsafe {
        libc::kill(-(child.id() as i32), libc::SIGKILL)
    };
    let _ = child.kill();
}

fn launch(
    chat: u64,
    worktree: &Path,
    agent: &Agent,
    events: async_channel::Sender<Evt>,
) -> anyhow::Result<Running> {
    if !agent.is_runnable() {
        anyhow::bail!("no command");
    }
    let mut command = Command::new(&agent.command);
    command
        .args(&agent.args)
        .envs(&agent.env)
        .current_dir(worktree)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    #[cfg(unix)]
    std::os::unix::process::CommandExt::process_group(&mut command, 0);
    crate::wsl::no_console(&mut command);
    let mut child = command.spawn()?;
    let stdin = child.stdin.take().expect("stdin was piped");
    let stdout = child.stdout.take().expect("stdout was piped");
    let stderr = child.stderr.take().expect("stderr was piped");
    let child = Arc::new(Mutex::new(child));
    let stopped = Arc::new(AtomicBool::new(false));
    let tail = Arc::new(Mutex::new(VecDeque::new()));

    // The writer: the lane never blocks on a pipe an agent has stopped reading.
    let (input, lines) = mpsc::channel::<String>();
    std::thread::Builder::new()
        .name(format!("claudhub-acp-{chat}-out"))
        .spawn(move || {
            let mut stdin = stdin;
            for mut line in lines {
                line.push('\n');
                if let Err(e) = stdin
                    .write_all(line.as_bytes())
                    .and_then(|()| stdin.flush())
                {
                    log::debug!("the agent's input closed: {e}");
                    return;
                }
            }
        })?;

    {
        let tail = tail.clone();
        let label = agent.label().to_string();
        std::thread::Builder::new()
            .name(format!("claudhub-acp-{chat}-err"))
            .spawn(move || {
                for line in BufReader::new(stderr).lines().map_while(Result::ok) {
                    log::debug!(target: "acp", "{label}: {line}");
                    let mut tail = tail.lock().unwrap();
                    if tail.len() == STDERR_LINES {
                        tail.pop_front();
                    }
                    tail.push_back(line);
                }
            })?;
    }

    {
        let (child, stopped) = (child.clone(), stopped.clone());
        std::thread::Builder::new()
            .name(format!("claudhub-acp-{chat}-in"))
            .spawn(move || {
                for line in BufReader::new(stdout).lines().map_while(Result::ok) {
                    // stdout belongs to the protocol, but a launcher may still
                    // print a banner there: only what looks like a message
                    // goes up.
                    if !line.trim_start().starts_with('{') {
                        continue;
                    }
                    if events.send_blocking(Evt::AcpLine { chat, line }).is_err() {
                        return;
                    }
                }
                let status = reap(&child);
                // Let stderr's last lines land before they are read.
                std::thread::sleep(Duration::from_millis(100));
                let reason = (!stopped.load(Ordering::SeqCst)).then(|| {
                    let tail = tail.lock().unwrap();
                    ended_because(status, tail.iter().map(String::as_str))
                });
                let _ = events.send_blocking(Evt::AcpEnded { chat, reason });
            })?;
    }

    Ok(Running {
        input,
        child,
        stopped,
    })
}

/// Waits for an agent whose stdout has closed, killing it past the grace
/// period — the lock is never held across a blocking wait, so `stop` can
/// always get in.
fn reap(child: &Mutex<Child>) -> Option<std::process::ExitStatus> {
    let deadline = Instant::now() + EXIT_GRACE;
    loop {
        if let Ok(Some(status)) = child.lock().unwrap().try_wait() {
            return Some(status);
        }
        if Instant::now() > deadline {
            kill(child);
            return child.lock().unwrap().wait().ok();
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}

/// What the view is told of an agent that died on its own.
fn ended_because<'a>(
    status: Option<std::process::ExitStatus>,
    stderr: impl Iterator<Item = &'a str>,
) -> String {
    let tail: Vec<&str> = stderr.collect();
    let head = match status.and_then(|status| status.code()) {
        Some(code) => format!("exit code {code}"),
        None => "ended".to_string(),
    };
    if tail.is_empty() {
        head
    } else {
        format!("{head}\n\n{}", tail.join("\n"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_default_agent_is_a_package_run_by_npx() {
        let claude = Agent::claude();
        assert_eq!(claude.command, "npx");
        assert_eq!(claude.args[0], "-y");
        assert_eq!(claude.label(), "Claude");
        assert!(Agent::gemini().args.contains(&"--acp".to_string()));
        let bare = Agent {
            command: "/opt/bin/my-agent".into(),
            ..Agent::default()
        };
        assert_eq!(bare.label(), "my-agent");
        assert!(!Agent::default().is_runnable());
    }

    /// The lane, end to end, with a shell standing in for an agent: what it
    /// writes on stdout comes back line by line, banner excluded, and its death
    /// is an event carrying its last words.
    #[cfg(unix)]
    #[test]
    fn an_agent_is_a_stream_of_lines_and_an_end() {
        let (tx, rx) = async_channel::unbounded();
        let host = Host::new(tx);
        let agent = Agent {
            name: "echo".into(),
            command: "sh".into(),
            args: vec![
                "-c".into(),
                "echo banner; read line; echo \"$line\"; echo oops >&2; exit 3".into(),
            ],
            env: BTreeMap::new(),
        };
        host.start(7, std::env::temp_dir(), agent);
        host.send(7, r#"{"jsonrpc":"2.0","method":"x"}"#.into());
        match rx.recv_blocking().unwrap() {
            Evt::AcpLine { chat, line } => {
                assert_eq!(chat, 7);
                assert_eq!(line, r#"{"jsonrpc":"2.0","method":"x"}"#);
            }
            other => panic!("{other:?}"),
        }
        match rx.recv_blocking().unwrap() {
            Evt::AcpEnded { chat, reason } => {
                assert_eq!(chat, 7);
                let reason = reason.unwrap();
                assert!(reason.contains("exit code 3"), "{reason}");
                assert!(reason.contains("oops"), "{reason}");
            }
            other => panic!("{other:?}"),
        }
    }

    #[cfg(unix)]
    #[test]
    fn a_chat_stopped_by_the_view_ends_in_silence() {
        let (tx, rx) = async_channel::unbounded();
        let host = Host::new(tx);
        let agent = Agent {
            name: "sleeper".into(),
            command: "sleep".into(),
            args: vec!["30".into()],
            env: BTreeMap::new(),
        };
        host.start(1, std::env::temp_dir(), agent);
        host.stop(1);
        match rx.recv_blocking().unwrap() {
            Evt::AcpEnded { chat: 1, reason } => assert_eq!(reason, None),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn a_missing_program_is_an_end_and_not_a_silence() {
        let (tx, rx) = async_channel::unbounded();
        let host = Host::new(tx);
        let agent = Agent {
            command: "claudhub-no-such-agent".into(),
            ..Agent::default()
        };
        host.start(2, std::env::temp_dir(), agent);
        assert!(matches!(
            rx.try_recv().unwrap(),
            Evt::AcpEnded {
                chat: 2,
                reason: Some(_)
            }
        ));
        host.send(2, "{}".into());
        assert!(matches!(
            rx.try_recv().unwrap(),
            Evt::AcpEnded { chat: 2, .. }
        ));
    }
}
