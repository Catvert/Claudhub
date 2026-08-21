//! Le client du transport distant : un `claudhub-server` en processus enfant.
//!
//! `connect` lance l'enfant et rend la même paire que `runtime::spawn` — un
//! [`Handle`] et un récepteur d'[`Evt`] — si bien que la vue ne sait pas où
//! les workers vivent. Trois threads font le pont : l'écrivain (file de `Cmd`
//! → stdin de l'enfant), le lecteur (stdout de l'enfant → canal d'`Evt`), et
//! le pompier de stderr (→ nos traces).
//!
//! **Rien ici ne bloque l'appelant.** `connect` rend la main dès l'enfant
//! lancé : un `wsl.exe` froid met des secondes à démarrer, et c'est le thread
//! d'interface qui appelle. La poignée de main se fait dans le lecteur, qui
//! la traduit en [`Evt::ServerHello`] — ou en [`Evt::ServerLost`] si les
//! versions ne s'accordent pas, avec de quoi le dire à l'utilisateur.

use std::io::BufRead;
use std::process::{Command, Stdio};

use super::wire::{self, Hello};
use super::{Cmd, Evt, Handle};

/// La ligne de commande dictée par l'environnement, s'il y en a une.
///
/// C'est le levier de test : sous Linux,
/// `CLAUDHUB_SERVER_CMD=target/debug/claudhub-server` exerce tout le fil sans
/// Windows ni WSL. Il l'emporte sur la mise en route automatique, ce qui en
/// fait aussi la sortie de secours quand celle-ci se trompe.
pub fn command_from_env() -> Option<Vec<String>> {
    let line = std::env::var("CLAUDHUB_SERVER_CMD").ok()?;
    let parts = crate::cmdline::split_command(&line);
    (!parts.is_empty()).then_some(parts)
}

/// Installe au besoin le serveur dans la distribution, puis s'y connecte.
///
/// Les deux premières étapes parlent à `wsl.exe` et peuvent durer plusieurs
/// secondes — une distribution endormie met du temps à s'éveiller, et douze
/// mégaoctets à copier ne sont pas gratuits. **Jamais depuis le thread
/// d'interface** : c'est la fenêtre qui serait figée pendant ce temps.
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
