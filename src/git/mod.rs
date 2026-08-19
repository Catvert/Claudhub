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
pub mod repo;
pub mod status;

pub use branch::{Branch, BranchKind, Upstream};
pub use diff::{DiffFile, DiffLine, DiffLineKind, FileDiff, Hunk, Range as DiffRange};
pub use repo::{Repo, Worktree};
pub use status::{FileStatus, Status, StatusCode};

use std::ffi::OsStr;
use std::path::Path;
use std::process::{Command, Stdio};

use anyhow::{bail, Context, Result};

/// Exécute `git -C <dir> <args…>` et rend sa sortie standard, sans le saut de
/// ligne final.
///
/// `stdin` est fermé : sans cela une commande qui décide de demander un mot de
/// passe hérite du terminal depuis lequel Perch a été lancé — au mieux rien ne
/// s'affiche, au pire le worker se bloque pour toujours sur une invite que
/// personne ne voit. `GIT_TERMINAL_PROMPT=0` fait dire non à git plutôt que de
/// le laisser essayer, et l'échec remonte comme un message d'erreur normal.
pub(crate) fn git<S: AsRef<OsStr>>(dir: &Path, args: &[S]) -> Result<String> {
    let out = command(dir, args)
        .output()
        .context("`git` est introuvable dans le PATH")?;
    if !out.status.success() {
        let stderr = String::from_utf8_lossy(&out.stderr);
        bail!("git {}: {}", describe(args), stderr.trim());
    }
    Ok(strip_trailing_newline(
        String::from_utf8_lossy(&out.stdout).into_owned(),
    ))
}

/// Idem, mais l'échec vaut `None` : pour les lectures facultatives (une
/// branche amont qui n'existe pas n'est pas une erreur).
pub(crate) fn git_opt<S: AsRef<OsStr>>(dir: &Path, args: &[S]) -> Option<String> {
    git(dir, args).ok()
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
