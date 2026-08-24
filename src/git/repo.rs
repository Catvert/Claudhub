//! Repository discovery, worktrees, and the operations that write
//! (stage, commit, fetch/pull/push, checkout).

use std::ffi::OsString;
use std::path::{Path, PathBuf};

use anyhow::{anyhow, bail, Context, Result};

use super::{git, git_blob, git_ok, git_opt, git_reporting, split_nul};

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

/// What `HEAD` holds for a path, or `None` when it holds nothing.
///
/// `None` covers the two cases the editor's gutter treats alike — a file git
/// does not track, and one added since the last commit — and it is a normal
/// answer, not a failure, hence `git_opt`. The path is given relative to the
/// worktree because that is how the revision syntax names it: `HEAD:` walks the
/// tree, and an absolute path is not in it.
///
/// `--textconv` is deliberately absent: a smudge filter would hand back
/// something that is not what the file's bytes were, and the gutter compares
/// bytes.
pub fn head_blob(dir: &Path, path: &Path) -> Option<String> {
    let path = path.to_str()?;
    git_opt(dir, &["show", &format!("HEAD:./{path}")])
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
    // `git_reporting`: a fetch says what it brought back on stderr, and its
    // stdout is empty.
    git_reporting(dir, &args)
}

/// `pull --ff-only`: an automatic merge triggered by a click is the best way to
/// end up with a conflict nobody asked for. On divergence, git refuses and the
/// user chooses for themselves.
pub fn pull(dir: &Path) -> Result<String> {
    git_reporting(dir, &["pull", "--ff-only"])
}

/// Brings the diverged branch back in line with its upstream — the choice the
/// user made in the divergence dialog — then pushes again when a push is what
/// started it all.
///
/// One function and not three commands, for the reason `Commit { push }` is one:
/// the halves would go into different queues, and nothing orders those. `git
/// pull` does the fetch itself, which matters after a rejected push: the
/// remote-tracking ref is stale — a rejection updates nothing — and merging it
/// as it stands would resolve nothing.
pub fn reconcile(dir: &Path, rebase: bool, then_push: bool) -> Result<String> {
    // `--no-rebase` spelled out: `pull.rebase` in the user's config would
    // otherwise decide, and the button pressed said "merge".
    let mode = if rebase { "--rebase" } else { "--no-rebase" };
    let pulled = git_reporting(dir, &["pull", mode])?;
    if !then_push {
        return Ok(pulled);
    }
    // The push's report over the pull's, the tags' precedent: the round trip is
    // the half one waited for, and the merge shows in the history that follows.
    let pushed = push(dir, false)?;
    Ok(match pushed.trim().is_empty() {
        true => pulled,
        false => pushed,
    })
}

/// Does this failure say the branch and its upstream have diverged?
///
/// Read off git's message — `LC_ALL=C` makes it stable — because the exit code
/// says nothing: a rejected push and a wrong URL both exit with 1. Three
/// phrases, one per road here: a push rejected because the branch is behind
/// (`non-fast-forward`), one rejected because the remote holds commits never
/// fetched (`fetch first`), and a `pull --ff-only` that refuses.
pub fn diverged(message: &str) -> bool {
    [
        "(non-fast-forward)",
        "(fetch first)",
        "Not possible to fast-forward",
    ]
    .iter()
    .any(|phrase| message.contains(phrase))
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
    git_reporting(dir, &args)
}

/// Moves HEAD onto a branch, whatever form the picker showed it in.
///
/// **A remote-tracking name is not a branch**, and that is the whole of this
/// function: `git switch origin/feat` fails outright — the ref is not something
/// HEAD may point at. What clicking `origin/feat` means is the local branch that
/// follows it, which `--track` creates and which may already exist under the
/// short name. Only what is left over goes to git as it was typed.
pub fn checkout(dir: &Path, branch: &str) -> Result<()> {
    if super::branch::local_exists(dir, branch) {
        return git(dir, &["switch", branch]).map(|_| ());
    }
    // A local name may itself contain a slash (`wt/essai`), which is why the
    // local is looked for first: the split below is only reached by a name no
    // local carries.
    if let Some((_, short)) = branch.split_once('/') {
        if super::branch::local_exists(dir, short) {
            return git(dir, &["switch", short]).map(|_| ());
        }
        if super::branch::remote_exists(dir, branch) {
            return git(dir, &["switch", "--track", branch]).map(|_| ());
        }
    }
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

/// Renames a branch, checked out or not.
///
/// `-m` and not `-M`: git refuses a name already taken, and overwriting someone
/// else's branch is not what a rename dialog is asking to do.
pub fn rename_branch(main: &Path, from: &str, to: &str) -> Result<()> {
    git(main, &["branch", "-m", from, to]).map(|_| ())
}

/// Removes a branch from `origin`, leaving the local one alone.
///
/// The full `refs/heads/` form: a tag and a branch may bear the same name, and
/// a bare name leaves git to guess which of the two is meant.
pub fn delete_remote_branch(main: &Path, name: &str) -> Result<String> {
    let short = super::branch::short_name(name);
    git_reporting(
        main,
        &["push", "origin", "--delete", &format!("refs/heads/{short}")],
    )
}

/// Publishes one branch, which need not be the one HEAD is on.
///
/// The refspec is written out (`<b>:<b>`) rather than left to `push origin <b>`:
/// the short form pushes *the branch called `b` on the remote*, which is the
/// same thing here and not the same thing under a `push.default` of someone
/// else's choosing. `--set-upstream` covers the first push of a branch Claudhub
/// created, whose remote counterpart does not exist yet.
pub fn push_branch(main: &Path, branch: &str, force_with_lease: bool) -> Result<String> {
    let mut args: Vec<String> = vec![
        "push".into(),
        "--set-upstream".into(),
        "origin".into(),
        format!("{branch}:{branch}"),
    ];
    if force_with_lease {
        args.push("--force-with-lease".into());
    }
    git_reporting(main, &args)
}

/// Brings a branch up to date with its upstream, without leaving the branch one
/// is on.
///
/// Two commands and not one, and which of the two applies is not a preference:
/// **git refuses to fetch into a branch that is checked out somewhere**, so a
/// branch held by a worktree is updated from inside it — `pull --ff-only`, the
/// same fast-forward-or-refuse rule as the button — and every other branch by
/// the refspec form, which moves the ref without a working tree at all.
///
/// Fast-forward in both halves: a branch nobody is standing on cannot be given
/// a merge commit by a menu entry, and a divergence is a decision.
pub fn update_branch(main: &Path, branch: &str) -> Result<String> {
    let upstream = super::branch::upstream_of(main, branch)
        .ok_or_else(|| anyhow!("{branch} has no upstream to update from"))?;
    if let Some(dir) = super::branch::checked_out_at(main, branch) {
        return pull(&dir);
    }
    let (remote, remote_branch) = super::branch::split_remote(&upstream)
        .ok_or_else(|| anyhow!("{upstream} does not name a remote"))?;
    git_reporting(
        main,
        &["fetch", remote, &format!("{remote_branch}:{branch}")],
    )
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
///
/// **Read from disk first, `rev-parse` only as a fallback.** `status` asks for
/// it on every refresh — and one refresh arrives per file write — where the
/// answer is a `stat` on `<dir>/.git`. The subprocess stays for the case the
/// disk cannot answer: a subdirectory of the checkout, a `.git` file written in
/// a form we do not read.
pub fn git_dir(dir: &Path) -> Option<PathBuf> {
    if let Some(path) = git_dir_on_disk(dir) {
        return Some(path);
    }
    let path = git_opt(dir, &["rev-parse", "--git-dir"])?;
    Some(absolute(dir, Path::new(&path)))
}

/// The same answer without a subprocess, when `dir` is a checkout's root.
///
/// `None` means "ask git": either there is no `.git` here, or it points
/// somewhere that is not a directory.
pub(crate) fn git_dir_on_disk(dir: &Path) -> Option<PathBuf> {
    let entry = dir.join(".git");
    if entry.is_dir() {
        return Some(entry);
    }
    let text = std::fs::read_to_string(&entry).ok()?;
    let target = text.strip_prefix("gitdir:")?.trim();
    let path = absolute(dir, Path::new(target));
    path.is_dir().then_some(path)
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

/// The three versions of a conflicted file, as the index holds them.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Stages {
    /// The common ancestor. Empty when there is none — a file both sides
    /// created, which conflicts as a whole and is none the worse for being
    /// compared against nothing.
    pub base: String,
    /// Ours, in the user's sense.
    pub ours: String,
    pub theirs: String,
}

/// Reads the three stages an unmerged file has in the index.
///
/// `:1:`, `:2:` and `:3:` are the ancestor, ours and theirs — and stages 2 and
/// 3 **swap over during a rebase** for the same reason `--ours` does: git
/// replays our commits on top of theirs, so the checkout is theirs and the
/// commit being applied is ours. The swap happens here, once, and the view
/// speaks of "ours" in the user's sense throughout.
///
/// A missing stage 2 or 3 is a conflict of a different kind — one side deleted
/// the file — and refusing it is the honest answer: there is no third column to
/// paint, and the two buttons of the conflicts panel already settle it.
pub fn stages(dir: &Path, path: &Path) -> Result<Stages> {
    let path = path
        .to_str()
        .ok_or_else(|| anyhow!("this path is not valid UTF-8"))?;
    let read = |stage: u8| git_blob(dir, &["show", &format!(":{stage}:./{path}")]);
    let swapped = matches!(pending(dir), Some(Pending::Rebase));
    let (mine, yours) = if swapped { (3, 2) } else { (2, 3) };
    Ok(Stages {
        // No ancestor is not an error: two branches that each created the file
        // have nothing in common, and the merge reads that as a conflict over
        // the whole file, which is what it is.
        base: read(1).unwrap_or_default(),
        ours: read(mine).context("this side of the conflict has no content")?,
        theirs: read(yours).context("this side of the conflict has no content")?,
    })
}

/// Writes a merged file and stages it, which is what marks it resolved.
///
/// Unconditional, where every other write of this program carries the digest of
/// what was read: what is on disk is the version git wrote with markers through
/// it, and overwriting it is the whole gesture.
pub fn resolve_with(dir: &Path, path: &Path, content: &str) -> Result<()> {
    let full = dir.join(path);
    if let Some(parent) = full.parent() {
        std::fs::create_dir_all(parent).ok();
    }
    std::fs::write(&full, content).with_context(|| format!("writing {}", full.display()))?;
    stage(dir, std::slice::from_ref(&path.to_path_buf()))
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
pub fn list_files(dir: &Path, ignored: bool) -> Result<Files> {
    let mut files: Vec<PathBuf> = list_of(dir, &["--cached", "--others", "--exclude-standard"])?
        .into_iter()
        .map(|entry| entry.path)
        .collect();
    let (excluded, dirs) = if ignored {
        // `--directory` is what makes this affordable: git stops at a folder it
        // excludes whole rather than walking it. On a Laravel checkout that is
        // five hundred and twenty entries instead of a hundred and fifty-four
        // thousand, ten milliseconds instead of a second — and the tree that
        // comes out of it has nine thousand nodes instead of a hundred and
        // sixty. What it costs is that a directory's contents are unknown until
        // someone opens it; see `Files::dirs`.
        let excluded = list_of(
            dir,
            &[
                "--others",
                "--ignored",
                "--exclude-standard",
                "--directory",
                // An excluded directory with nothing in it has nothing to show.
                "--no-empty-directory",
            ],
        )?;
        let dirs = excluded
            .iter()
            .filter(|entry| entry.dir)
            .map(|entry| entry.path.clone())
            .collect();
        let excluded: Vec<PathBuf> = excluded.into_iter().map(|entry| entry.path).collect();
        files.extend(excluded.iter().cloned());
        // Two lists, each sorted, are not a sorted list — and the tree is built
        // from the order git gives.
        files.sort_unstable();
        (excluded, dirs)
    } else {
        (Vec::new(), Vec::new())
    };
    // `--cached --others` may return the same path twice; the list being sorted,
    // a local dedup is enough.
    files.dedup();
    Ok(Files {
        all: files,
        ignored: excluded,
        dirs,
    })
}

/// A worktree's files, and which of them `.gitignore` leaves out.
///
/// The excluded ones are named separately rather than flagged one by one: the
/// explorer greys them, and a second sorted list it can binary-search costs
/// nothing on a project that ignores little, where a `Vec<(PathBuf, bool)>`
/// would pay a byte and an alignment hole per file either way. It is empty
/// whenever they were not asked for, which is also what says "nothing to grey".
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct Files {
    pub all: Vec<PathBuf>,
    pub ignored: Vec<PathBuf>,
    /// Those of `ignored` that are **directories git stopped at**, and whose
    /// contents are therefore unknown. Sorted, and a subset of `ignored`.
    ///
    /// They are named rather than left to be guessed: a path with nothing under
    /// it is a file as far as a tree built from paths can tell, and `vendor/`
    /// would draw itself as one.
    pub dirs: Vec<PathBuf>,
}

/// One entry of `ls-files`, and whether git wrote it as a directory.
struct Listed {
    path: PathBuf,
    dir: bool,
}

fn list_of(dir: &Path, flags: &[&str]) -> Result<Vec<Listed>> {
    let mut args: Vec<&str> = vec!["ls-files", "-z"];
    args.extend_from_slice(flags);
    let out = git(dir, &args)?;
    // The trailing slash is how `--directory` says "and I did not look inside".
    // It has to be read off the text: `PathBuf` drops it, and `vendor/` and
    // `vendor` are the same path once parsed.
    Ok(split_nul(&out)
        .map(|text| Listed {
            dir: text.ends_with('/'),
            path: PathBuf::from(text.trim_end_matches('/')),
        })
        .collect())
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

    /// The three messages git writes for a divergence, verbatim, and the
    /// failures that must not pass for one: offering a merge over a typoed URL
    /// or a refused lease would run `git pull` on a repository whose problem is
    /// elsewhere.
    #[test]
    fn a_divergence_is_read_off_gits_own_words() {
        // `push` while the branch is behind its upstream.
        assert!(diverged(
            " ! [rejected]        feat -> feat (non-fast-forward)\n\
             error: failed to push some refs to 'origin'\n\
             hint: Updates were rejected because the tip of your current branch is behind"
        ));
        // `push` while the remote holds commits never fetched.
        assert!(diverged(
            " ! [rejected]        feat -> feat (fetch first)\n\
             error: failed to push some refs to 'origin'"
        ));
        // `pull --ff-only` on a branch that has its own commits.
        assert!(diverged(
            "hint: Diverging branches can't be fast-forwarded, you need to either:\n\
             fatal: Not possible to fast-forward, aborting."
        ));

        assert!(!diverged("fatal: could not read from remote repository"));
        // A refused lease is the protection doing its work, not a divergence
        // to smooth over.
        assert!(!diverged(
            " ! [rejected]        feat -> feat (stale info)\n\
             error: failed to push some refs to 'origin'"
        ));
        assert!(!diverged(
            "fatal: 'origin' does not appear to be a git repository"
        ));
    }

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
        assert!(
            plain.all.contains(&PathBuf::from("src/code.rs")),
            "{plain:?}"
        );
        assert!(
            plain.all.contains(&PathBuf::from("src/new.rs")),
            "a new file too"
        );
        assert!(
            !plain.all.iter().any(|p| p.starts_with("vendor")),
            "nothing ignored: {plain:?}"
        );
        assert!(
            plain.ignored.is_empty(),
            "nothing to grey when nothing was asked for"
        );

        let all = list_files(&root, true).unwrap();
        // git stops at the directory it excludes whole, and says so by the
        // trailing slash — a hundred and fifty thousand entries become five
        // hundred on a real checkout. What is inside is nobody's business
        // until a chevron asks; see `files::read_dir`.
        assert!(
            all.all.contains(&PathBuf::from("vendor")),
            "the ignored directory, not what is under it: {all:?}"
        );
        assert!(
            // `starts_with` compares components, so `vendor` starts with
            // `vendor/`: what is asked here is that nothing goes *deeper*.
            !all.all
                .iter()
                .any(|p| p.starts_with("vendor") && p != Path::new("vendor")),
            "and nothing under it: {all:?}"
        );
        assert_eq!(all.dirs, vec![PathBuf::from("vendor")]);
        // And everything the plain listing had is still there.
        for path in &plain.all {
            assert!(
                all.all.contains(path),
                "{} is missing from {all:?}",
                path.display()
            );
        }
        // The excluded ones are named apart, and are a subset of the whole: it
        // is that list the explorer binary-searches to know what to grey.
        assert!(all.ignored.contains(&PathBuf::from("vendor")));
        for path in &all.ignored {
            assert!(
                all.all.contains(path),
                "{} is not in the list",
                path.display()
            );
            assert!(
                !plain.all.contains(path),
                "{} is not ignored",
                path.display()
            );
        }
        // Sorted and without duplicates: the tree is built from this order, and
        // the greying searches the second list the same way.
        for list in [&all.all, &all.ignored] {
            let mut sorted = list.clone();
            sorted.sort_unstable();
            sorted.dedup();
            assert_eq!(list, &sorted);
        }

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
