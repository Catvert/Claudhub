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

use std::io::BufRead;
use std::process::{Command, Stdio};

use super::wire::{self, Hello};
use super::{Cmd, Evt, Handle};

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
/// Failure here is a **launch** failure (program not found); everything after
/// — handshake, death of the server — comes back through the event channel,
/// the window being open by then.
pub fn connect(argv: &[String]) -> anyhow::Result<(Handle, async_channel::Receiver<Evt>)> {
    let (program, args) = argv
        .split_first()
        .ok_or_else(|| anyhow::anyhow!("empty server target"))?;

    let mut child = Command::new(program)
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
    let (cmd_tx, cmd_rx) = async_channel::unbounded::<Cmd>();
    std::thread::Builder::new()
        .name("claudhub-remote-out".into())
        .spawn(move || {
            if wire::write_frame(&mut stdin, &Hello::current()).is_err() {
                return;
            }
            while let Ok(cmd) = cmd_rx.recv_blocking() {
                if wire::write_frame(&mut stdin, &cmd).is_err() {
                    return; // the reader will report the server's death
                }
            }
            // No handle left: stdin closes on the way out, and the server
            // reads that end of stream as the order to shut down.
        })?;

    // The reader: the handshake, then the events.
    let (evt_tx, evt_rx) = async_channel::unbounded::<Evt>();
    std::thread::Builder::new()
        .name("claudhub-remote-in".into())
        .spawn(move || {
            let lost = |message: String| {
                let _ = evt_tx.send_blocking(Evt::ServerLost { message });
            };
            let hello: Hello = match wire::read_frame(&mut stdout) {
                Ok(Some(hello)) => hello,
                Ok(None) => return lost("the server died before the handshake".into()),
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
            }
            match child.wait() {
                Ok(status) => log::info!(target: "claudhub_server", "server exited: {status}"),
                Err(e) => log::warn!("waiting for the server: {e}"),
            }
        })?;

    Ok((Handle::remote(cmd_tx), evt_rx))
}
