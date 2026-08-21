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

/// Où trouver le serveur.
#[derive(Debug, Clone)]
pub enum Target {
    /// Une ligne de commande explicite — le chemin de test : sous Linux,
    /// `CLAUDHUB_SERVER_CMD=target/debug/claudhub-server` exerce tout le fil
    /// sans Windows ni WSL.
    Command(Vec<String>),
}

/// La cible dictée par l'environnement, s'il y en a une.
///
/// C'est la vue qui décide du mode (elle seule connaît les réglages) ; ceci
/// n'est que le levier de test, lu partout pareil.
pub fn target_from_env() -> Option<Target> {
    let line = std::env::var("CLAUDHUB_SERVER_CMD").ok()?;
    let parts = crate::cmdline::split_command(&line);
    (!parts.is_empty()).then_some(Target::Command(parts))
}

/// Lance le serveur et rend de quoi lui parler et l'écouter.
///
/// L'échec ici est celui du **lancement** (programme introuvable) ; tout ce
/// qui arrive après — poignée de main, mort du serveur — remonte par le canal
/// d'événements, la fenêtre étant déjà ouverte à ce moment-là.
pub fn connect(target: &Target) -> anyhow::Result<(Handle, async_channel::Receiver<Evt>)> {
    let Target::Command(parts) = target;
    let (program, args) = parts
        .split_first()
        .ok_or_else(|| anyhow::anyhow!("cible de serveur vide"))?;

    let mut child = Command::new(program)
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| anyhow::anyhow!("{program} n'a pas pu être lancé : {e}"))?;

    let mut stdin = child.stdin.take().expect("stdin demandé");
    let mut stdout = child.stdout.take().expect("stdout demandé");
    let stderr = child.stderr.take().expect("stderr demandé");

    // L'écrivain. Notre poignée de main part d'abord, hors de la file : elle
    // doit précéder tout `Cmd`, et le serveur l'attend avant de servir.
    let (cmd_tx, cmd_rx) = async_channel::unbounded::<Cmd>();
    std::thread::Builder::new()
        .name("claudhub-remote-out".into())
        .spawn(move || {
            if wire::write_frame(&mut stdin, &Hello::current()).is_err() {
                return;
            }
            while let Ok(cmd) = cmd_rx.recv_blocking() {
                if wire::write_frame(&mut stdin, &cmd).is_err() {
                    return; // le lecteur dira la mort du serveur
                }
            }
            // Plus de manche : stdin se ferme en sortant, et le serveur lit
            // cette fin de flux comme l'ordre de s'éteindre.
        })?;

    // Le lecteur : la poignée de main, puis les événements.
    let (evt_tx, evt_rx) = async_channel::unbounded::<Evt>();
    std::thread::Builder::new()
        .name("claudhub-remote-in".into())
        .spawn(move || {
            let lost = |message: String| {
                let _ = evt_tx.send_blocking(Evt::ServerLost { message });
            };
            let hello: Hello = match wire::read_frame(&mut stdout) {
                Ok(Some(hello)) => hello,
                Ok(None) => return lost("le serveur s'est éteint avant la poignée de main".into()),
                Err(e) => return lost(format!("poignée de main illisible : {e}")),
            };
            if hello.protocol != wire::PROTOCOL_VERSION {
                return lost(format!(
                    "versions désaccordées : serveur {} ({}), interface {}",
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
                            return; // la fenêtre est partie
                        }
                    }
                    Ok(None) => return lost("le serveur s'est éteint".into()),
                    Err(e) => return lost(format!("flux du serveur illisible : {e}")),
                }
            }
        })?;

    // Le pompier : stderr de l'enfant dans nos traces, et le `wait` qui évite
    // le zombie — stderr ne se ferme qu'à la mort du processus.
    std::thread::Builder::new()
        .name("claudhub-remote-err".into())
        .spawn(move || {
            for line in std::io::BufReader::new(stderr).lines() {
                let Ok(line) = line else { break };
                log::info!(target: "claudhub_server", "{line}");
            }
            match child.wait() {
                Ok(status) => log::info!(target: "claudhub_server", "serveur terminé : {status}"),
                Err(e) => log::warn!("attente du serveur : {e}"),
            }
        })?;

    Ok((Handle::remote(cmd_tx), evt_rx))
}
