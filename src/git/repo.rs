//! Découverte du dépôt, worktrees, et les opérations qui écrivent
//! (stage, commit, fetch/pull/push, checkout).

use std::ffi::OsString;
use std::path::{Path, PathBuf};

use anyhow::{anyhow, bail, Context, Result};

use super::{git, git_ok, git_opt, split_nul};

/// Un dépôt tel que Claudhub le voit : le dépôt principal et ses worktrees liés.
///
/// `main` est toujours le dépôt d'origine, même si Claudhub a été ouvert sur un
/// worktree : `--git-common-dir` pointe sur le `.git` partagé quel que soit le
/// checkout d'où on l'interroge.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Repo {
    pub main: PathBuf,
}

/// Un checkout : le principal ou un worktree lié.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Worktree {
    pub path: PathBuf,
    /// Nom court de la branche, ou `None` en HEAD détachée.
    pub branch: Option<String>,
    pub head: String,
    /// Le checkout principal, celui qu'on ne peut pas retirer.
    pub is_main: bool,
    pub locked: bool,
    /// Le dossier a disparu du disque ; `git worktree prune` le nettoiera.
    pub prunable: bool,
}

impl Worktree {
    /// Nom affiché dans la barre latérale : le dernier segment du chemin, qui
    /// est ce que l'utilisateur a tapé en créant le worktree.
    pub fn label(&self) -> String {
        self.path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| self.path.display().to_string())
    }
}

impl Repo {
    /// Remonte de `start` au dépôt principal.
    pub fn discover(start: &Path) -> Result<Self> {
        let common = git(start, &["rev-parse", "--git-common-dir"])
            .with_context(|| format!("{} n'est pas dans un dépôt git", start.display()))?;
        let common = PathBuf::from(&common);
        let common = if common.is_absolute() {
            common
        } else {
            start.join(common)
        };
        let common = common.canonicalize().unwrap_or(common);
        // `.git/` → le dépôt ; un dépôt nu n'a pas de checkout et n'a rien à
        // faire ici, mais son parent reste un point de départ utilisable.
        let main = common
            .parent()
            .ok_or_else(|| anyhow!("dépôt sans répertoire de travail : {}", common.display()))?
            .to_path_buf();
        Ok(Self { main })
    }

    /// Nom du dépôt, tel qu'affiché en tête de la barre latérale.
    pub fn name(&self) -> String {
        self.main
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| self.main.display().to_string())
    }

    /// Tous les checkouts, principal en tête (c'est l'ordre de `git worktree
    /// list`, et celui qu'attend la barre latérale).
    pub fn worktrees(&self) -> Result<Vec<Worktree>> {
        let out = git(&self.main, &["worktree", "list", "--porcelain", "-z"])?;
        Ok(parse_worktree_list(&out))
    }

    pub fn add_worktree(&self, path: &Path, branch: &str, from: Option<&str>) -> Result<()> {
        let mut args: Vec<OsString> = vec!["worktree".into(), "add".into()];
        if super::branch::local_exists(&self.main, branch) {
            if let Some(holder) = super::branch::checked_out_at(&self.main, branch) {
                bail!(
                    "la branche « {branch} » est déjà déployée dans {}",
                    holder.display()
                );
            }
            if let Some(from) = from {
                // git accepterait la commande en ignorant le point de départ ;
                // créer autre chose que ce qui est demandé vaut un refus.
                bail!("« {branch} » existe déjà : elle ne peut pas repartir de « {from} »");
            }
            args.push(path.into());
            args.push(branch.into());
        } else {
            args.push(path.into());
            args.push("-b".into());
            args.push(branch.into());
            if let Some(from) = from {
                args.push(from.into());
            }
        }
        git(&self.main, &args)?;
        super::branch::ensure_upstream(&self.main, branch);
        Ok(())
    }

    /// Retire un worktree. `force` va jusqu'à jeter des modifications non
    /// validées — l'appelant est responsable d'avoir demandé confirmation.
    pub fn remove_worktree(&self, path: &Path, force: bool) -> Result<()> {
        let mut args: Vec<OsString> = vec!["worktree".into(), "remove".into()];
        if force {
            args.push("--force".into());
        }
        args.push(path.into());
        git(&self.main, &args)?;
        git(&self.main, &["worktree", "prune"])?;
        Ok(())
    }
}

/// Opérations menées *dans* un checkout donné.
///
/// Elles prennent le répertoire en argument plutôt que d'être des méthodes de
/// `Worktree` : la vue de revue les appelle sur le worktree sélectionné, qui
/// est une donnée rafraîchie en permanence et pas un objet qu'on garde.
pub fn stage(dir: &Path, paths: &[PathBuf]) -> Result<()> {
    if paths.is_empty() {
        return Ok(());
    }
    let mut args: Vec<OsString> = vec!["add".into(), "--".into()];
    args.extend(paths.iter().map(OsString::from));
    git(dir, &args).map(|_| ())
}

/// Dé-indexe sans toucher au fichier. `restore --staged` est la formulation
/// moderne de `reset HEAD --`, et elle marche aussi sur un dépôt sans commit,
/// là où `reset HEAD` échoue faute de HEAD.
pub fn unstage(dir: &Path, paths: &[PathBuf]) -> Result<()> {
    if paths.is_empty() {
        return Ok(());
    }
    let mut args: Vec<OsString> = vec!["restore".into(), "--staged".into(), "--".into()];
    args.extend(paths.iter().map(OsString::from));
    git(dir, &args).map(|_| ())
}

/// Jette les modifications du répertoire de travail. Destructif et sans
/// filet : rien dans git ne permet de les retrouver ensuite.
pub fn discard(dir: &Path, paths: &[PathBuf]) -> Result<()> {
    if paths.is_empty() {
        return Ok(());
    }
    let mut args: Vec<OsString> = vec![
        "restore".into(),
        "--worktree".into(),
        "--source=HEAD".into(),
        "--".into(),
    ];
    args.extend(paths.iter().map(OsString::from));
    git(dir, &args).map(|_| ())
}

/// Supprime des fichiers que git ne suit pas.
///
/// `git clean` et non `std::fs::remove_file` : il refuse ce qui est suivi, ce
/// qui est la garantie qu'on veut ici — une erreur d'aiguillage dans la vue ne
/// peut pas détruire un fichier versionné. `-d` couvre les dossiers, `-f`
/// est exigé par git pour toute suppression.
pub fn clean(dir: &Path, paths: &[PathBuf]) -> Result<()> {
    if paths.is_empty() {
        return Ok(());
    }
    let mut args: Vec<OsString> = vec!["clean".into(), "-f".into(), "-d".into(), "--".into()];
    args.extend(paths.iter().map(OsString::from));
    git(dir, &args).map(|_| ())
}

/// Applique (`reverse = false`) ou annule (`reverse = true`) un patch sur
/// l'index : c'est ainsi que se fait l'indexation d'un hunk isolé, git n'ayant
/// pas d'API pour « ajoute ce morceau-là ».
pub fn apply_patch(dir: &Path, patch: &str, reverse: bool) -> Result<()> {
    use std::io::Write;
    use std::process::{Command, Stdio};

    let mut cmd = Command::new("git");
    cmd.arg("-C")
        .arg(dir)
        .args(["apply", "--cached", "--unidiff-zero"]);
    if reverse {
        cmd.arg("--reverse");
    }
    let mut child = cmd
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .env("LC_ALL", "C")
        .spawn()
        .context("`git apply` n'a pas pu démarrer")?;
    child
        .stdin
        .take()
        .expect("stdin demandé en piped")
        .write_all(patch.as_bytes())
        .context("écriture du patch dans `git apply`")?;
    let out = child.wait_with_output()?;
    if !out.status.success() {
        bail!(
            "git apply : {}",
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    Ok(())
}

pub struct CommitOptions<'a> {
    pub message: &'a str,
    /// Reprend le commit précédent au lieu d'en créer un.
    pub amend: bool,
    /// Indexe tout le suivi avant de valider (`git commit -a`).
    pub all: bool,
}

pub fn commit(dir: &Path, opts: CommitOptions<'_>) -> Result<String> {
    if opts.message.trim().is_empty() && !opts.amend {
        bail!("le message de commit est vide");
    }
    let mut args: Vec<OsString> = vec!["commit".into()];
    if opts.all {
        args.push("--all".into());
    }
    if opts.amend {
        args.push("--amend".into());
    }
    // `-m` même en amend : sinon git ouvre l'éditeur, qui n'a pas de terminal.
    args.push("-m".into());
    args.push(opts.message.into());
    git(dir, &args)
}

pub fn fetch(dir: &Path, prune: bool) -> Result<String> {
    let mut args: Vec<&str> = vec!["fetch", "--all"];
    if prune {
        args.push("--prune");
    }
    git(dir, &args)
}

/// `pull --ff-only` : un merge automatique déclenché par un clic est la
/// meilleure façon de se retrouver avec un conflit qu'on n'a pas demandé. En
/// cas de divergence, git refuse et l'utilisateur choisit lui-même.
pub fn pull(dir: &Path) -> Result<String> {
    git(dir, &["pull", "--ff-only"])
}

/// Pousse la branche courante. `--set-upstream` couvre le premier envoi d'une
/// branche créée par Claudhub, dont la remote n'existe pas encore.
pub fn push(dir: &Path, force_with_lease: bool) -> Result<String> {
    let branch = super::branch::current(dir).ok_or_else(|| anyhow!("HEAD est détachée"))?;
    let mut args: Vec<OsString> = vec!["push".into(), "--set-upstream".into(), "origin".into()];
    args.push(branch.into());
    if force_with_lease {
        // Jamais `--force` nu : `--force-with-lease` refuse d'écraser un
        // commit qu'on n'a pas vu, ce qui est exactement la protection qu'on
        // veut derrière un bouton.
        args.push("--force-with-lease".into());
    }
    git(dir, &args)
}

pub fn checkout(dir: &Path, branch: &str) -> Result<()> {
    git(dir, &["switch", branch]).map(|_| ())
}

pub fn create_branch(dir: &Path, name: &str, from: Option<&str>) -> Result<()> {
    let mut args: Vec<&str> = vec!["switch", "-c", name];
    args.extend(from);
    git(dir, &args).map(|_| ())
}

pub fn delete_branch(main: &Path, name: &str, force: bool) -> Result<()> {
    let flag = if force { "-D" } else { "-d" };
    git(main, &["branch", flag, name]).map(|_| ())
}

/// Une opération git en cours, qui laisse le dépôt à mi-chemin.
///
/// Tant qu'elle dure, l'index porte des conflits et `HEAD` ne désigne pas ce
/// qu'on croit. La barre d'état la nomme : sans cela, l'utilisateur se
/// retrouve dans un état que Claudhub ne dit pas, à se demander pourquoi la
/// liste des fichiers ressemble à ça.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum Pending {
    Merge,
    Rebase,
    CherryPick,
    Revert,
}

impl Pending {
    /// Clé i18n du nom de l'opération.
    pub fn key(self) -> &'static str {
        match self {
            Self::Merge => "pending-merge",
            Self::Rebase => "pending-rebase",
            Self::CherryPick => "pending-cherry-pick",
            Self::Revert => "pending-revert",
        }
    }

    /// Le sous-commande qui la continue ou l'abandonne.
    fn command(self) -> &'static str {
        match self {
            Self::Merge => "merge",
            Self::Rebase => "rebase",
            Self::CherryPick => "cherry-pick",
            Self::Revert => "revert",
        }
    }
}

/// Le répertoire git **de ce checkout**.
///
/// Dans un worktree lié, `.git` est un *fichier* qui pointe vers
/// `<principal>/.git/worktrees/<nom>` : c'est là que vivent son `HEAD`, son
/// index et les marqueurs d'opération en cours. Les chercher dans `<dir>/.git`
/// revient à ne jamais rien trouver.
pub fn git_dir(dir: &Path) -> Option<PathBuf> {
    let path = git_opt(dir, &["rev-parse", "--git-dir"])?;
    Some(absolute(dir, Path::new(&path)))
}

/// L'opération en cours, d'après les marqueurs que git laisse dans son
/// répertoire.
///
/// Fonction libre et sans sous-processus : `status` la rappelle à chaque
/// rafraîchissement, et il en arrive un par écriture de fichier.
pub fn pending_in(git_dir: &Path) -> Option<Pending> {
    // L'ordre compte : un rebase pose aussi `CHERRY_PICK_HEAD` en rejouant ses
    // commits, et l'annoncer comme un picorage ferait proposer la mauvaise
    // commande pour en sortir.
    const MARKERS: [(&str, Pending); 5] = [
        ("rebase-merge", Pending::Rebase),
        ("rebase-apply", Pending::Rebase),
        ("MERGE_HEAD", Pending::Merge),
        ("CHERRY_PICK_HEAD", Pending::CherryPick),
        ("REVERT_HEAD", Pending::Revert),
    ];
    MARKERS
        .into_iter()
        .find(|(marker, _)| git_dir.join(marker).exists())
        .map(|(_, kind)| kind)
}

/// L'opération en cours dans ce checkout, s'il y en a une.
pub fn pending(dir: &Path) -> Option<Pending> {
    pending_in(&git_dir(dir)?)
}

/// Intègre `from` dans la branche courante.
///
/// `--no-edit` parce qu'un message par défaut convient : le geste part d'un
/// bouton, pas d'une ligne de commande où l'on aurait de quoi écrire.
pub fn merge(dir: &Path, from: &str, no_ff: bool) -> Result<String> {
    let mut args: Vec<&str> = vec!["merge", "--no-edit"];
    if no_ff {
        args.push("--no-ff");
    }
    args.push(from);
    git(dir, &args)
}

/// Rejoue la branche courante sur `onto`.
pub fn rebase(dir: &Path, onto: &str) -> Result<String> {
    git(dir, &["rebase", onto])
}

/// Abandonne l'opération en cours et rend le checkout à son état d'avant.
pub fn abort(dir: &Path) -> Result<String> {
    let kind = pending(dir).ok_or_else(|| anyhow!("aucune opération en cours"))?;
    git(dir, &[kind.command(), "--abort"])
}

/// Reprend l'opération en cours, une fois les conflits résolus.
pub fn resume(dir: &Path) -> Result<String> {
    let kind = pending(dir).ok_or_else(|| anyhow!("aucune opération en cours"))?;
    git(dir, &[kind.command(), "--continue"])
}

/// Résout un conflit en gardant une des deux versions.
///
/// `--ours` et `--theirs` de `git checkout` désignent, pendant un merge, la
/// branche courante et celle qu'on intègre — et **s'inversent pendant un
/// rebase**, où git rejoue nos commits par-dessus les leurs. Le drapeau est
/// donc traduit ici plutôt qu'à l'appel : la vue parle de « la nôtre » et de
/// « la leur » au sens de l'utilisateur, pas au sens de git.
pub fn resolve(dir: &Path, path: &Path, ours: bool) -> Result<()> {
    let swapped = matches!(pending(dir), Some(Pending::Rebase));
    let flag = if ours != swapped {
        "--ours"
    } else {
        "--theirs"
    };
    let mut args: Vec<OsString> = vec!["checkout".into(), flag.into(), "--".into()];
    args.push(path.as_os_str().to_os_string());
    git(dir, &args)?;
    // Garder une version, c'est décider : le fichier passe à l'index, ce qui
    // le fait sortir de la liste des conflits.
    stage(dir, std::slice::from_ref(&path.to_path_buf()))
}

/// Tous les fichiers suivis et les nouveaux non ignorés, en **un seul appel**.
///
/// C'est déjà ce que fait la surveillance de fichiers pour décider quoi
/// observer, et pour la même raison : un projet Laravel a quarante mille
/// répertoires, et un parcours de disque dossier par dossier coûterait un
/// appel système par répertoire pour arriver aux sept cents qui portent du
/// code.
///
/// `ignored` ajoute ce que `.gitignore` écarte — `vendor/`, `node_modules/`,
/// `target/` : c'est un choix explicite, parce que la liste change alors
/// d'ordre de grandeur.
pub fn list_files(dir: &Path, ignored: bool) -> Result<Vec<PathBuf>> {
    let mut args: Vec<&str> = vec!["ls-files", "-z", "--cached", "--others"];
    if ignored {
        args.push("--ignored");
    } else {
        args.push("--exclude-standard");
    }
    let out = git(dir, &args)?;
    let mut files: Vec<PathBuf> = split_nul(&out).map(PathBuf::from).collect();
    // `--cached --others` peut rendre deux fois le même chemin ; la liste est
    // déjà triée par git, donc un dédoublonnage local suffit.
    files.dedup();
    Ok(files)
}

/// Vrai si le checkout a des modifications non validées, suivies ou non.
pub fn is_dirty(dir: &Path) -> bool {
    git_opt(dir, &["status", "--porcelain"])
        .map(|s| !s.trim().is_empty())
        .unwrap_or(false)
}

pub fn is_repo(dir: &Path) -> bool {
    git_ok(dir, &["rev-parse", "--git-dir"])
}

/// Parse `git worktree list --porcelain -z`.
///
/// Le format est une suite d'enregistrements `clé valeur` séparés par des
/// octets nuls, un bloc par worktree, les blocs séparés par un enregistrement
/// vide — que `split_nul` élimine, d'où le découpage sur `worktree `.
fn parse_worktree_list(out: &str) -> Vec<Worktree> {
    let mut trees: Vec<Worktree> = Vec::new();
    for rec in split_nul(out) {
        let (key, value) = match rec.split_once(' ') {
            Some((k, v)) => (k, v),
            None => (rec, ""),
        };
        match key {
            "worktree" => trees.push(Worktree {
                path: PathBuf::from(value),
                branch: None,
                head: String::new(),
                // Le premier bloc rendu par git est toujours le principal.
                is_main: trees.is_empty(),
                locked: false,
                prunable: false,
            }),
            "HEAD" => {
                if let Some(w) = trees.last_mut() {
                    w.head = value.to_string();
                }
            }
            "branch" => {
                if let Some(w) = trees.last_mut() {
                    w.branch = Some(value.trim_start_matches("refs/heads/").to_string());
                }
            }
            "locked" => {
                if let Some(w) = trees.last_mut() {
                    w.locked = true;
                }
            }
            "prunable" => {
                if let Some(w) = trees.last_mut() {
                    w.prunable = true;
                }
            }
            // "detached", "bare" : rien à retenir, `branch` reste None.
            _ => {}
        }
    }
    trees
}

/// Chemin absolu d'un fichier du checkout, pour l'ouvrir dans un éditeur.
pub fn absolute(dir: &Path, rel: &Path) -> PathBuf {
    if rel.is_absolute() {
        rel.to_path_buf()
    } else {
        dir.join(rel)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_a_main_repo_and_two_worktrees() {
        // Tel que git l'écrit : enregistrements NUL, bloc vide entre worktrees.
        let out = "worktree /repo\0HEAD abc123\0branch refs/heads/main\0\0\
                   worktree /repo-wt/feat\0HEAD def456\0branch refs/heads/wt/feat\0\0\
                   worktree /repo-wt/gone\0HEAD 000000\0detached\0prunable gitdir file points to non-existent location\0\0";
        let trees = parse_worktree_list(out);
        assert_eq!(trees.len(), 3);

        assert_eq!(trees[0].path, PathBuf::from("/repo"));
        assert_eq!(trees[0].branch.as_deref(), Some("main"));
        assert!(trees[0].is_main);

        assert_eq!(trees[1].branch.as_deref(), Some("wt/feat"));
        assert!(!trees[1].is_main);
        assert!(!trees[1].prunable);

        // HEAD détachée : pas de branche, et git nous dit qu'il est élagable.
        assert_eq!(trees[2].branch, None);
        assert!(trees[2].prunable);
        assert_eq!(trees[2].label(), "gone");
    }

    #[test]
    fn parses_a_locked_worktree() {
        let out = "worktree /repo\0HEAD abc\0branch refs/heads/main\0\0\
                   worktree /mnt/usb/wt\0HEAD abc\0branch refs/heads/x\0locked\0\0";
        let trees = parse_worktree_list(out);
        assert!(trees[1].locked);
        assert!(!trees[0].locked);
    }
}
