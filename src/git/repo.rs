//! Repository discovery, worktrees, and the operations that write
//! (stage, commit, fetch/pull/push, checkout).

use std::ffi::OsString;
use std::path::{Path, PathBuf};

use anyhow::{anyhow, bail, Context, Result};

use super::{git, git_ok, git_opt, split_nul};

/// A repository as Claudhub sees it: the main repository and its linked
/// worktrees.
///
/// `main` is always the original repository, even when Claudhub was opened on a
/// worktree: `--git-common-dir` points at the shared `.git` whatever checkout it
/// is asked from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Repo {
    pub main: PathBuf,
}

/// A checkout: the main one or a linked worktree.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Worktree {
    pub path: PathBuf,
    /// Short branch name, or `None` on a detached HEAD.
    pub branch: Option<String>,
    pub head: String,
    /// The main checkout, the one that cannot be removed.
    pub is_main: bool,
    pub locked: bool,
    /// The folder has vanished from disk; `git worktree prune` will clean it up.
    pub prunable: bool,
}

impl Worktree {
    /// Name shown in the sidebar: the path's last segment, which is what the
    /// user typed when creating the worktree.
    pub fn label(&self) -> String {
        self.path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| self.path.display().to_string())
    }
}

impl Repo {
    /// Walks up from `start` to the main repository.
    pub fn discover(start: &Path) -> Result<Self> {
        let common = git(start, &["rev-parse", "--git-common-dir"])
            .with_context(|| format!("{} is not inside a git repository", start.display()))?;
        let common = PathBuf::from(&common);
        let common = if common.is_absolute() {
            common
        } else {
            start.join(common)
        };
        let common = common.canonicalize().unwrap_or(common);
        // `.git/` → the repository; a bare repository has no checkout and has no
        // business here, but its parent is still a usable starting point.
        let main = common
            .parent()
            .ok_or_else(|| anyhow!("repository with no working tree: {}", common.display()))?
            .to_path_buf();
        Ok(Self { main })
    }

    /// The repository's name, as shown at the top of the sidebar.
    pub fn name(&self) -> String {
        self.main
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| self.main.display().to_string())
    }

    /// Every checkout, main one first (that is `git worktree list`'s order, and
    /// the one the sidebar expects).
    pub fn worktrees(&self) -> Result<Vec<Worktree>> {
        let out = git(&self.main, &["worktree", "list", "--porcelain", "-z"])?;
        Ok(parse_worktree_list(&out))
    }

    pub fn add_worktree(&self, path: &Path, branch: &str, from: Option<&str>) -> Result<()> {
        let mut args: Vec<OsString> = vec!["worktree".into(), "add".into()];
        if super::branch::local_exists(&self.main, branch) {
            if let Some(holder) = super::branch::checked_out_at(&self.main, branch) {
                bail!(
                    "the branch \"{branch}\" is already checked out in {}",
                    holder.display()
                );
            }
            if let Some(from) = from {
                // git would accept the command while ignoring the start point;
                // creating something other than what was asked deserves a refusal.
                bail!("\"{branch}\" already exists: it cannot start again from \"{from}\"");
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

    /// Removes a worktree. `force` goes as far as discarding uncommitted
    /// changes — the caller is responsible for having asked for confirmation.
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

/// Operations carried out *inside* a given checkout.
///
/// They take the directory as an argument rather than being `Worktree` methods:
/// the review view calls them on the selected worktree, which is data refreshed
/// continuously and not an object we hold.
pub fn stage(dir: &Path, paths: &[PathBuf]) -> Result<()> {
    if paths.is_empty() {
        return Ok(());
    }
    let mut args: Vec<OsString> = vec!["add".into(), "--".into()];
    args.extend(paths.iter().map(OsString::from));
    git(dir, &args).map(|_| ())
}

/// Unstages without touching the file. `restore --staged` is the modern
/// wording of `reset HEAD --`, and it also works on a repository with no
/// commit, where `reset HEAD` fails for want of a HEAD.
pub fn unstage(dir: &Path, paths: &[PathBuf]) -> Result<()> {
    if paths.is_empty() {
        return Ok(());
    }
    let mut args: Vec<OsString> = vec!["restore".into(), "--staged".into(), "--".into()];
    args.extend(paths.iter().map(OsString::from));
    git(dir, &args).map(|_| ())
}

/// Discards the working tree's changes. Destructive and without a net: nothing
/// in git makes it possible to get them back afterwards.
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

/// Deletes files git does not track.
///
/// `git clean` and not `std::fs::remove_file`: it refuses what is tracked,
/// which is the guarantee wanted here — a misrouted click in the view cannot
/// destroy a versioned file. `-d` covers directories, `-f` is required by git
/// for any deletion.
pub fn clean(dir: &Path, paths: &[PathBuf]) -> Result<()> {
    if paths.is_empty() {
        return Ok(());
    }
    let mut args: Vec<OsString> = vec!["clean".into(), "-f".into(), "-d".into(), "--".into()];
    args.extend(paths.iter().map(OsString::from));
    git(dir, &args).map(|_| ())
}

/// Applies (`reverse = false`) or undoes (`reverse = true`) a patch on the
/// index: that is how an isolated hunk is staged, git having no API for "add
/// that piece".
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
        .context("`git apply` could not start")?;
    child
        .stdin
        .take()
        .expect("stdin requested as piped")
        .write_all(patch.as_bytes())
        .context("writing the patch into `git apply`")?;
    let out = child.wait_with_output()?;
    if !out.status.success() {
        bail!("git apply: {}", String::from_utf8_lossy(&out.stderr).trim());
    }
    Ok(())
}

pub struct CommitOptions<'a> {
    pub message: &'a str,
    /// Reuses the previous commit instead of creating one.
    pub amend: bool,
    /// Stages everything tracked before committing (`git commit -a`).
    pub all: bool,
}

pub fn commit(dir: &Path, opts: CommitOptions<'_>) -> Result<String> {
    if opts.message.trim().is_empty() && !opts.amend {
        bail!("the commit message is empty");
    }
    let mut args: Vec<OsString> = vec!["commit".into()];
    if opts.all {
        args.push("--all".into());
    }
    if opts.amend {
        args.push("--amend".into());
    }
    // `-m` even when amending: otherwise git opens the editor, which has no terminal.
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

/// `pull --ff-only`: an automatic merge triggered by a click is the best way to
/// end up with a conflict nobody asked for. On divergence, git refuses and the
/// user chooses for themselves.
pub fn pull(dir: &Path) -> Result<String> {
    git(dir, &["pull", "--ff-only"])
}

/// Pushes the current branch. `--set-upstream` covers the first push of a
/// branch created by Claudhub, whose remote does not exist yet.
pub fn push(dir: &Path, force_with_lease: bool) -> Result<String> {
    let branch = super::branch::current(dir).ok_or_else(|| anyhow!("HEAD is detached"))?;
    let mut args: Vec<OsString> = vec!["push".into(), "--set-upstream".into(), "origin".into()];
    args.push(branch.into());
    if force_with_lease {
        // Never a bare `--force`: `--force-with-lease` refuses to overwrite a
        // commit we have not seen, which is exactly the protection wanted
        // behind a button.
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

/// A git operation in progress, leaving the repository half-finished.
///
/// While it lasts, the index carries conflicts and `HEAD` does not point where
/// you think. The status bar names it: without that, the user ends up in a
/// state Claudhub does not describe, wondering why the file list looks the way
/// it does.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum Pending {
    Merge,
    Rebase,
    CherryPick,
    Revert,
}

impl Pending {
    /// The i18n key of the operation's name.
    pub fn key(self) -> &'static str {
        match self {
            Self::Merge => "pending-merge",
            Self::Rebase => "pending-rebase",
            Self::CherryPick => "pending-cherry-pick",
            Self::Revert => "pending-revert",
        }
    }

    /// The subcommand that continues or aborts it.
    fn command(self) -> &'static str {
        match self {
            Self::Merge => "merge",
            Self::Rebase => "rebase",
            Self::CherryPick => "cherry-pick",
            Self::Revert => "revert",
        }
    }
}

/// The git directory **of this checkout**.
///
/// In a linked worktree, `.git` is a *file* pointing at
/// `<main>/.git/worktrees/<name>`: that is where its `HEAD`, its index and the
/// in-progress operation markers live. Looking for them in `<dir>/.git` amounts
/// to never finding anything.
pub fn git_dir(dir: &Path) -> Option<PathBuf> {
    let path = git_opt(dir, &["rev-parse", "--git-dir"])?;
    Some(absolute(dir, Path::new(&path)))
}

/// The operation in progress, from the markers git leaves in its directory.
///
/// A free function with no subprocess: `status` calls it on every refresh, and
/// one arrives per file write.
pub fn pending_in(git_dir: &Path) -> Option<Pending> {
    // The order matters: a rebase also sets `CHERRY_PICK_HEAD` while replaying
    // its commits, and announcing it as a cherry-pick would offer the wrong
    // command to get out of it.
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

/// The operation in progress in this checkout, if there is one.
pub fn pending(dir: &Path) -> Option<Pending> {
    pending_in(&git_dir(dir)?)
}

/// Integrates `from` into the current branch.
///
/// `--no-edit` because a default message will do: the gesture starts from a
/// button, not from a command line where there would be room to write.
pub fn merge(dir: &Path, from: &str, no_ff: bool) -> Result<String> {
    let mut args: Vec<&str> = vec!["merge", "--no-edit"];
    if no_ff {
        args.push("--no-ff");
    }
    args.push(from);
    git(dir, &args)
}

/// Replays the current branch onto `onto`.
pub fn rebase(dir: &Path, onto: &str) -> Result<String> {
    git(dir, &["rebase", onto])
}

/// Aborts the operation in progress and returns the checkout to its earlier state.
pub fn abort(dir: &Path) -> Result<String> {
    let kind = pending(dir).ok_or_else(|| anyhow!("no operation in progress"))?;
    git(dir, &[kind.command(), "--abort"])
}

/// Resumes the operation in progress, once the conflicts are resolved.
pub fn resume(dir: &Path) -> Result<String> {
    let kind = pending(dir).ok_or_else(|| anyhow!("no operation in progress"))?;
    git(dir, &[kind.command(), "--continue"])
}

/// Resolves a conflict by keeping one of the two versions.
///
/// `git checkout`'s `--ours` and `--theirs` name, during a merge, the current
/// branch and the one being integrated — and **swap over during a rebase**,
/// where git replays our commits on top of theirs. The flag is therefore
/// translated here rather than at the call site: the view speaks of "ours" and
/// "theirs" in the user's sense, not in git's.
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
    // Keeping a version is deciding: the file moves into the index, which takes
    // it out of the conflict list.
    stage(dir, std::slice::from_ref(&path.to_path_buf()))
}

/// Every tracked file and every non-ignored new one, in **a single call**.
///
/// It is already what the file watcher does to decide what to observe, and for
/// the same reason: a Laravel project has forty thousand directories, and a
/// disk walk folder by folder would cost one system call per directory to reach
/// the seven hundred that carry code.
///
/// `ignored` adds what `.gitignore` leaves out — `vendor/`, `node_modules/`,
/// `target/`: an explicit choice, because the list then changes order of
/// magnitude.
///
/// **Two commands in that case, and there is no way round it.** `--ignored` is
/// not "add the ignored ones": it is a *filter*, and git says so — it shows
/// only what an exclude pattern matches, so `--cached --others --ignored`
/// returns the ignored files **alone**, without a single tracked one. It also
/// refuses to run without `--exclude-standard`, which is the error this used to
/// put in the journal: `ls-files --ignored needs some exclude pattern`. The
/// union therefore takes one call for each half.
pub fn list_files(dir: &Path, ignored: bool) -> Result<Vec<PathBuf>> {
    let mut files = list_of(dir, &["--cached", "--others", "--exclude-standard"])?;
    if ignored {
        files.extend(list_of(
            dir,
            &["--others", "--ignored", "--exclude-standard"],
        )?);
        // Two lists, each sorted, are not a sorted list — and the tree is built
        // from the order git gives.
        files.sort_unstable();
    }
    // `--cached --others` may return the same path twice; the list being sorted,
    // a local dedup is enough.
    files.dedup();
    Ok(files)
}

fn list_of(dir: &Path, flags: &[&str]) -> Result<Vec<PathBuf>> {
    let mut args: Vec<&str> = vec!["ls-files", "-z"];
    args.extend_from_slice(flags);
    let out = git(dir, &args)?;
    Ok(split_nul(&out).map(PathBuf::from).collect())
}

/// True if the checkout has uncommitted changes, tracked or not.
pub fn is_dirty(dir: &Path) -> bool {
    git_opt(dir, &["status", "--porcelain"])
        .map(|s| !s.trim().is_empty())
        .unwrap_or(false)
}

pub fn is_repo(dir: &Path) -> bool {
    git_ok(dir, &["rev-parse", "--git-dir"])
}

/// Parses `git worktree list --porcelain -z`.
///
/// The format is a sequence of `key value` records separated by null bytes, one
/// block per worktree, blocks separated by an empty record — which `split_nul`
/// removes, hence the split on `worktree `.
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
                // The first block git returns is always the main one.
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
            // "detached", "bare": nothing to keep, `branch` stays None.
            _ => {}
        }
    }
    trees
}

/// Absolute path of a checkout file, to open it in an editor.
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

    /// A small real repository: tracked code, a new file, an ignored folder.
    fn scratch_repo(name: &str) -> PathBuf {
        let root = std::env::temp_dir().join(format!("claudhub-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("src")).unwrap();
        std::fs::create_dir_all(root.join("vendor/pkg")).unwrap();
        std::fs::write(root.join("src/code.rs"), "fn main() {}").unwrap();
        std::fs::write(root.join("src/new.rs"), "// not added yet").unwrap();
        std::fs::write(root.join("vendor/pkg/big.php"), "<?php").unwrap();
        std::fs::write(root.join(".gitignore"), "vendor/\n").unwrap();
        for args in [
            vec!["init", "-q"],
            vec!["config", "user.email", "t@example.com"],
            vec!["config", "user.name", "T"],
            vec!["add", "src/code.rs", ".gitignore"],
        ] {
            std::process::Command::new("git")
                .arg("-C")
                .arg(&root)
                .args(&args)
                .output()
                .unwrap();
        }
        root
    }

    #[test]
    fn showing_the_ignored_files_adds_them_instead_of_replacing_everything() {
        // `--ignored` is a filter and not an addition: asking for it in the
        // same call returned the ignored files alone — and git refused to run
        // at all without `--exclude-standard`, which is what the journal was
        // saying.
        let root = scratch_repo("ls-files");

        let plain = list_files(&root, false).unwrap();
        assert!(plain.contains(&PathBuf::from("src/code.rs")), "{plain:?}");
        assert!(
            plain.contains(&PathBuf::from("src/new.rs")),
            "a new file too"
        );
        assert!(
            !plain.iter().any(|p| p.starts_with("vendor")),
            "nothing ignored: {plain:?}"
        );

        let all = list_files(&root, true).unwrap();
        assert!(
            all.contains(&PathBuf::from("vendor/pkg/big.php")),
            "the ignored files: {all:?}"
        );
        // And everything the plain listing had is still there.
        for path in &plain {
            assert!(
                all.contains(path),
                "{} is missing from {all:?}",
                path.display()
            );
        }
        // Sorted and without duplicates: the tree is built from this order.
        let mut sorted = all.clone();
        sorted.sort_unstable();
        sorted.dedup();
        assert_eq!(all, sorted);

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn parses_a_main_repo_and_two_worktrees() {
        // As git writes it: NUL records, empty block between worktrees.
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

        // Detached HEAD: no branch, and git tells us it is prunable.
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
