//! The remote transport client: a `claudhub-server` child process.
//!
//! `connect` launches the child and returns the same pair as `runtime::spawn`
//! — a [`Handle`] and an [`Evt`] receiver — so the view does not know where
//! the workers live. Three threads bridge it: the writer (`Cmd` queue → the
//! child's stdin), the reader (the child's stdout → the `Evt` channel), and
//! the stderr pump (→ our traces).
//!
//! **Nothing here blocks the caller.** `connect` returns as soon as the child
//! is launched: a cold `wsl.exe` takes seconds to start, and it is the
//! interface thread calling. The handshake happens in the reader, which
//! translates it into [`Evt::ServerHello`] — or [`Evt::ServerLost`] if the
//! versions disagree, with something to tell the user.

use std::io::{BufRead, Write};
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use super::wire::{self, Hello};
use super::{Evt, Handle, Order};

/// The command line dictated by the environment, if there is one.
///
/// This is the test lever: on Linux,
/// `CLAUDHUB_SERVER_CMD=target/debug/claudhub-server` exercises the whole wire
/// without Windows or WSL. It wins over the automatic startup, which also
/// makes it the escape hatch when that startup gets it wrong.
pub fn command_from_env() -> Option<Vec<String>> {
    let line = std::env::var("CLAUDHUB_SERVER_CMD").ok()?;
    let parts = crate::cmdline::split_command(&line);
    (!parts.is_empty()).then_some(parts)
}

/// Installs the server into the distribution if needed, then connects to it.
///
/// The first two steps talk to `wsl.exe` and can take several seconds — a
/// sleeping distribution is slow to wake, and twelve megabytes to copy are not
/// free. **Never from the interface thread**: the window would be frozen for
/// that whole time.
pub fn connect_wsl(
    distro: &str,
    cwd: Option<&str>,
) -> anyhow::Result<(Handle, async_channel::Receiver<Evt>, crate::wsl::Probe)> {
    let probe = crate::wsl::probe(distro)?;
    let server = crate::wsl::ensure_installed(distro, &probe)?;
    let (handle, events) = connect(&crate::wsl::launch_argv(distro, &server, cwd))?;
    Ok((handle, events, probe))
}

/// Launches the server and returns what is needed to talk and listen to it.
///
/// Failure here is a **launch** failure (program not found, or a handshake
/// this end cannot write); everything after — the server's handshake, its
/// death — comes back through the event channel, the window being open by
/// then.
pub fn connect(argv: &[String]) -> anyhow::Result<(Handle, async_channel::Receiver<Evt>)> {
    let (program, args) = argv
        .split_first()
        .ok_or_else(|| anyhow::anyhow!("empty server target"))?;

    // Our handshake, framed **before** anything is launched: one that cannot
    // be written is refused here, where the caller hears of it, rather than in
    // the writer thread — where it left the server waiting for it and the
    // window waiting for the server, for ever.
    let mut hello = Vec::new();
    wire::write_hello(&mut hello, &Hello::current())?;

    let mut command = Command::new(program);
    // The server is a console program launched by a windowed one: without this
    // it would come with a console window of its own.
    crate::wsl::no_console(&mut command);
    let mut child = command
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| anyhow::anyhow!("{program} could not be launched: {e}"))?;

    let mut stdin = child.stdin.take().expect("stdin requested");
    let mut stdout = child.stdout.take().expect("stdout requested");
    let stderr = child.stderr.take().expect("stderr requested");

    // The writer. Our handshake goes first, outside the queue: it must precede
    // any `Cmd`, and the server waits for it before serving.
    let (cmd_tx, cmd_rx) = async_channel::unbounded::<Order>();
    std::thread::Builder::new()
        .name("claudhub-remote-out".into())
        .spawn(move || {
            if stdin
                .write_all(&hello)
                .and_then(|()| stdin.flush())
                .is_err()
            {
                return;
            }
            while let Ok(order) = cmd_rx.recv_blocking() {
                if wire::write_frame(&mut stdin, &order).is_err() {
                    return; // the reader will report the server's death
                }
            }
            // No handle left: stdin closes on the way out, and the server
            // reads that end of stream as the order to shut down.
        })?;

    // What the server last said on stderr, and the moment it stops talking.
    // One that dies before its handshake says why there — a handshake it could
    // not write, a `wsl.exe` that found no distribution — and a `ServerLost`
    // that only said "it died" sent the user to the journal for the one
    // sentence that mattered. Before the handshake only: after it, the last
    // line is whatever the server's journal said last.
    let last_words = Arc::new(Mutex::new(None::<String>));
    let (silent_tx, silent_rx) = std::sync::mpsc::channel::<()>();

    // The reader: the handshake, then the events.
    let (evt_tx, evt_rx) = async_channel::unbounded::<Evt>();
    let heard = Arc::clone(&last_words);
    std::thread::Builder::new()
        .name("claudhub-remote-in".into())
        .spawn(move || {
            let lost = |message: String| {
                let _ = evt_tx.send_blocking(Evt::ServerLost { message });
            };
            // The end of stdout and the end of stderr are two pipes closing,
            // in no set order: the other thread is given a moment to read the
            // last line before it is looked for.
            let died_early = || {
                let what = "the server died before the handshake";
                let _ = silent_rx.recv_timeout(Duration::from_millis(500));
                let words = heard.lock().ok().and_then(|mut words| words.take());
                lost(match words {
                    Some(words) => format!("{what}: {words}"),
                    None => what.to_string(),
                })
            };
            let hello: Hello = match wire::read_frame(&mut stdout) {
                Ok(Some(hello)) => hello,
                Ok(None) => return died_early(),
                Err(e) => return lost(format!("unreadable handshake: {e}")),
            };
            if hello.protocol != wire::PROTOCOL_VERSION {
                return lost(format!(
                    "version mismatch: server {} ({}), interface {}",
                    hello.protocol,
                    hello.build,
                    wire::PROTOCOL_VERSION
                ));
            }
            let sent = evt_tx.send_blocking(Evt::ServerHello {
                build: hello.build,
                cwd: hello.cwd,
                running_under_wsl: hello.running_under_wsl,
                shells: hello.shells,
            });
            if sent.is_err() {
                return;
            }
            loop {
                match wire::read_frame::<Evt>(&mut stdout) {
                    Ok(Some(evt)) => {
                        if evt_tx.send_blocking(evt).is_err() {
                            return; // the window is gone
                        }
                    }
                    Ok(None) => return lost("the server died".into()),
                    Err(e) => return lost(format!("unreadable server stream: {e}")),
                }
            }
        })?;

    // The pump: the child's stderr into our traces, plus the `wait` that
    // avoids a zombie — stderr only closes when the process dies.
    std::thread::Builder::new()
        .name("claudhub-remote-err".into())
        .spawn(move || {
            for line in std::io::BufReader::new(stderr).lines() {
                let Ok(line) = line else { break };
                log::info!(target: "claudhub_server", "{line}");
                if let (Ok(mut words), false) = (last_words.lock(), line.trim().is_empty()) {
                    *words = Some(line);
                }
            }
            drop(silent_tx);
            match child.wait() {
                Ok(status) => log::info!(target: "claudhub_server", "server exited: {status}"),
                Err(e) => log::warn!("waiting for the server: {e}"),
            }
        })?;

    Ok((Handle::remote(cmd_tx), evt_rx))
}
