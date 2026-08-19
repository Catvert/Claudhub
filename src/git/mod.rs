//! Couche git de Perch.
//!
//! Tout passe par le binaire `git` en sous-processus, jamais par libgit2. Un
//! utilisateur qui lance Perch attend que ses `credential.helper`,
//! `includeIf`, hooks, `commit.gpgsign` et alias s'appliquent — c'est-à-dire
//! sa configuration, pas une réimplémentation qui en couvrirait la moitié.
//! Le coût est un `fork` par commande ; à l'échelle d'un panneau de revue qui
//! rafraîchit sur événement de fichier, il est invisible.
//!
//! Aucune fonction de ce module ne doit être appelée depuis le thread UI :
//! elles bloquent. Elles sont conçues pour tourner dans le worker
//! (`crate::app::worker`), qui renvoie ses résultats par événements.

pub mod branch;
pub mod diff;
// Nommé `history` et non `log` : un module `log` dans ce crate masquerait la
// bibliothèque de journalisation du même nom pour tout le fichier.
pub mod history;
pub mod repo;
pub mod status;

pub use branch::{Branch, BranchKind, Upstream};
pub use diff::{DiffFile, DiffLine, DiffLineKind, FileDiff, Hunk, Range as DiffRange};
pub use history::{Commit, GraphRow, LogRange};
pub use repo::{Repo, Worktree};
pub use status::{FileStatus, Status, StatusCode, Summary};

use std::ffi::OsStr;
use std::io::Read;
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use anyhow::{bail, Context, Result};

/// Au-delà, la commande est tuée et l'échec remonte comme un message.
///
/// Aucune lecture git ne prend trente secondes : un `status` coûte dix
/// millisecondes sur un dépôt de quarante mille fichiers. Ce délai n'existe
/// donc pas pour les commandes lentes mais pour celles qui **n'aboutissent
/// jamais** — une invite d'authentification qu'on ne voit pas, un dépôt sur un
/// montage réseau qui a disparu, un verrou tenu par un autre outil. Sans lui,
/// une seule commande de ce genre emporte un worker définitivement, et trois
/// figent toute l'application sans le moindre message.
const TIMEOUT: Duration = Duration::from_secs(30);

// Le délai est réglable pour les tests : trente secondes d'attente y seraient
// insupportables, et vérifier qu'une commande bloquée est bien interrompue vaut
// mieux que de faire confiance au code.
#[cfg(test)]
thread_local! {
    static TEST_TIMEOUT: std::cell::Cell<Option<Duration>> = const { std::cell::Cell::new(None) };
}

fn timeout() -> Duration {
    #[cfg(test)]
    if let Some(d) = TEST_TIMEOUT.with(|t| t.get()) {
        return d;
    }
    TIMEOUT
}

/// Exécute `git -C <dir> <args…>` et rend sa sortie standard, sans le saut de
/// ligne final.
///
/// `stdin` est fermé : sans cela une commande qui décide de demander un mot de
/// passe hérite du terminal depuis lequel Perch a été lancé — au mieux rien ne
/// s'affiche, au pire le worker se bloque pour toujours sur une invite que
/// personne ne voit. `GIT_TERMINAL_PROMPT=0` fait dire non à git plutôt que de
/// le laisser essayer, et l'échec remonte comme un message d'erreur normal.
pub(crate) fn git<S: AsRef<OsStr>>(dir: &Path, args: &[S]) -> Result<String> {
    let started = Instant::now();
    let out = run(dir, args)?;
    let elapsed = started.elapsed();
    // Une commande qui dépasse la demi-seconde mérite qu'on sache laquelle :
    // c'est la première trace à regarder quand l'interface traîne.
    if elapsed > Duration::from_millis(500) {
        log::debug!("git {} : {elapsed:?}", describe(args));
    }
    if !out.status.success() {
        let stderr = String::from_utf8_lossy(&out.stderr);
        bail!("git {}: {}", describe(args), stderr.trim());
    }
    Ok(strip_trailing_newline(
        String::from_utf8_lossy(&out.stdout).into_owned(),
    ))
}

/// Lance la commande et attend sa fin, sans dépasser `TIMEOUT`.
///
/// Les deux sorties sont lues par des threads : un tube plein bloque
/// l'écrivain, et `git diff` d'un gros fichier remplit les soixante-quatre
/// kilo-octets du tube bien avant de se terminer. Les lire après l'attente
/// donnerait un interblocage — le processus attend qu'on vide le tube, nous
/// attendons qu'il se termine.
fn run<S: AsRef<OsStr>>(dir: &Path, args: &[S]) -> Result<std::process::Output> {
    let mut cmd = command(dir, args);
    cmd.stdout(Stdio::piped()).stderr(Stdio::piped());
    wait_with_timeout(cmd, || format!("git {}", describe(args)))
}

/// Attend la fin d'un processus, ou l'interrompt passé le délai.
///
/// Séparé de `run` pour être vérifiable : le tester avec `git` demanderait une
/// commande git qui pend de façon reproductible, ce qui n'existe pas.
fn wait_with_timeout(
    mut cmd: Command,
    describe: impl Fn() -> String,
) -> Result<std::process::Output> {
    let mut child = cmd
        .spawn()
        .with_context(|| format!("{} : programme introuvable", describe()))?;

    let mut stdout = child.stdout.take().expect("stdout demandé en piped");
    let mut stderr = child.stderr.take().expect("stderr demandé en piped");
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

    let limit = timeout();
    let deadline = Instant::now() + limit;
    let status = loop {
        match child.try_wait()? {
            Some(status) => break status,
            None if Instant::now() >= deadline => {
                let _ = child.kill();
                let _ = child.wait();
                bail!(
                    "{} n'a pas répondu en {:?} et a été interrompue",
                    describe(),
                    limit
                );
            }
            // Assez court pour qu'une commande de dix millisecondes n'en
            // paraisse pas cinquante, assez long pour ne pas tourner à vide.
            None => std::thread::sleep(Duration::from_millis(5)),
        }
    };

    Ok(std::process::Output {
        status,
        stdout: out_reader.join().unwrap_or_default(),
        stderr: err_reader.join().unwrap_or_default(),
    })
}

/// Idem, mais l'échec vaut `None` : pour les lectures facultatives (une
/// branche amont qui n'existe pas n'est pas une erreur).
pub(crate) fn git_opt<S: AsRef<OsStr>>(dir: &Path, args: &[S]) -> Option<String> {
    git(dir, args).ok()
}

/// Lance git et rend sa sortie **même si le code de retour n'est pas nul**.
///
/// Pour la poignée de commandes dont un code non nul est le cas normal :
/// `diff --no-index` sort avec 1 dès qu'il y a une différence, ce qui est
/// exactement ce qu'on lui demande de trouver. Passer par `git` ferait jeter
/// la sortie avec l'« erreur ».
///
/// Au-delà de `max_code`, c'est un vrai échec : `--no-index` sort avec 2 quand
/// le fichier n'existe pas ou n'est pas lisible.
pub(crate) fn git_tolerant<S: AsRef<OsStr>>(
    dir: &Path,
    args: &[S],
    max_code: i32,
) -> Result<String> {
    let out = run(dir, args)?;
    let code = out.status.code().unwrap_or(-1);
    if code < 0 || code > max_code {
        let stderr = String::from_utf8_lossy(&out.stderr);
        bail!("git {}: {}", describe(args), stderr.trim());
    }
    Ok(strip_trailing_newline(
        String::from_utf8_lossy(&out.stdout).into_owned(),
    ))
}

/// Vrai si la commande sort avec le code 0. Pour les questions fermées
/// (`show-ref --verify --quiet`) dont la sortie n'intéresse personne.
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
        // Un pager laisserait la commande attendre un lecteur qui n'existe pas.
        .env("GIT_PAGER", "cat")
        .env("GIT_TERMINAL_PROMPT", "0")
        // Les sorties porcelain sont stables, mais les messages d'erreur que
        // nous affichons tels quels ne le sont pas : les lire en anglais évite
        // de dépendre de la locale de la machine pour les reconnaître.
        .env("LC_ALL", "C");
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

/// Découpe une sortie `-z` (enregistrements séparés par des octets nuls).
///
/// Les formats `--porcelain=v1 -z`, `diff --name-status -z` et consorts
/// existent précisément parce qu'un chemin peut contenir un saut de ligne ou
/// une apostrophe ; découper sur `\n` marche jusqu'au jour où un fichier
/// s'appelle mal.
pub(crate) fn split_nul(s: &str) -> impl Iterator<Item = &str> {
    s.split('\0').filter(|r| !r.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Une commande qui n'aboutit jamais ne doit pas emporter son worker : sans
    /// cette interruption, trois d'entre elles figent l'application entière —
    /// plus de statut, plus de diff — sans le moindre message.
    #[test]
    fn a_command_that_never_returns_is_interrupted() {
        TEST_TIMEOUT.with(|t| t.set(Some(Duration::from_millis(300))));

        let mut cmd = Command::new("sleep");
        cmd.arg("30").stdout(Stdio::piped()).stderr(Stdio::piped());

        let started = Instant::now();
        let result = wait_with_timeout(cmd, || "sleep 30".into());
        let elapsed = started.elapsed();

        TEST_TIMEOUT.with(|t| t.set(None));

        let message = result.expect_err("la commande devait être interrompue");
        assert!(
            message.to_string().contains("interrompue"),
            "message inattendu : {message}"
        );
        assert!(
            elapsed < Duration::from_secs(3),
            "l'interruption a pris {elapsed:?}"
        );
    }

    #[test]
    fn a_large_output_is_read_while_waiting() {
        // Bien plus que la taille d'un tube : si les sorties n'étaient pas
        // lues pendant l'attente, le processus resterait bloqué à écrire et
        // nous à l'attendre — l'interblocage classique de `spawn` + `wait`.
        let mut cmd = Command::new("sh");
        cmd.args(["-c", "head -c 2000000 /dev/zero | tr '\\0' 'x'"])
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());

        let out = wait_with_timeout(cmd, || "sortie volumineuse".into())
            .expect("la commande doit se terminer");
        assert_eq!(out.stdout.len(), 2_000_000);
    }
}
