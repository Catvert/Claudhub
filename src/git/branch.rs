//! Branches: the selector's list, and the closed questions worktree operations
//! ask.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use anyhow::Result;

use super::{git, git_ok, git_opt};

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum BranchKind {
    Local,
    /// Remote-tracking branch (`origin/…`) with no local counterpart.
    Remote,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Upstream {
    pub name: String,
    pub ahead: usize,
    pub behind: usize,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Branch {
    pub name: String,
    pub kind: BranchKind,
    /// Date of the last commit, relative ("3 days ago"), as git phrases it — we
    /// have no need to recompute it.
    pub date: String,
    pub subject: String,
    /// Author of the last commit. In a team repository, this tells two
    /// similarly named branches apart more reliably than their date.
    pub author: String,
    pub upstream: Option<Upstream>,
    /// Worktree that already has this branch checked out. Git refuses two
    /// checkouts of the same branch: saying so beforehand beats an error.
    ///
    /// It is also the only honest answer to "which branch is *here*": the list
    /// is read once per repository, in the **main** worktree, so `%(HEAD)`
    /// marked the main's branch whatever checkout was being looked at — the
    /// picker said "here" on the wrong row from every linked worktree.
    pub checked_out_at: Option<PathBuf>,
}

impl Branch {
    /// Is this the branch checked out in `worktree`?
    pub fn is_head_in(&self, worktree: &Path) -> bool {
        self.checked_out_at.as_deref() == Some(worktree)
    }
}

/// Lists the branches, local first then the remotes with no local twin, from
/// the most recent commit to the oldest.
pub fn list(main: &Path) -> Result<Vec<Branch>> {
    // The separator has to be a character a commit subject does not contain;
    // `%00` is written literally by for-each-ref as a null byte. The author,
    // then the full reference, come last: adding a field at the end keeps
    // outputs written by an earlier version readable, a missing field being the
    // empty string.
    //
    // `%(refname)` is what removed the second `for-each-ref`: it says whether a
    // reference lives under `refs/heads` or `refs/remotes`, which the short name
    // alone cannot tell — a local branch may well be called `origin/x`.
    const FORMAT: &str = "%(refname:short)%00%(HEAD)%00%(committerdate:relative)%00\
                          %(contents:subject)%00%(upstream:short)%00%(upstream:track)%00\
                          %(authorname)%00%(refname)";

    let raw = git(
        main,
        &[
            "for-each-ref",
            "--sort=-committerdate",
            &format!("--format={FORMAT}"),
            "refs/heads",
            "refs/remotes",
        ],
    )?;

    let locals: HashSet<&str> = raw
        .lines()
        .filter_map(|line| line.split('\0').nth(REFNAME_FIELD))
        .filter_map(|refname| refname.strip_prefix("refs/heads/"))
        .collect();

    let mut branches: Vec<Branch> = raw
        .lines()
        .filter_map(|line| parse_ref(line, &locals))
        .collect();
    // for-each-ref sorts by date across the whole set; we want the locals
    // first, each group staying sorted by date (Rust's stable sort guarantees it).
    branches.sort_by_key(|b| match b.kind {
        BranchKind::Local => 0,
        BranchKind::Remote => 1,
    });
    // One `worktree list` for the whole list, and not one per branch: the
    // answer is the same for all of them, and it used to be a fork per local
    // branch — a hundred of them on a repository that has lived a while.
    let holders = checked_out_map(main);
    for b in &mut branches {
        if b.kind == BranchKind::Local {
            b.checked_out_at = holders.get(b.name.as_str()).cloned();
        }
    }
    Ok(branches)
}

/// Where `%(refname)` sits in `FORMAT`, counted in NUL-separated fields.
const REFNAME_FIELD: usize = 7;

fn parse_ref(line: &str, locals: &HashSet<&str>) -> Option<Branch> {
    let mut f = line.split('\0');
    let name = f.next()?.to_string();
    // `%(HEAD)` stays in the format so the field count holds, but it is not
    // read: it marks the HEAD of the checkout the command ran in — the main
    // worktree — and "here" is a per-worktree question, answered by
    // `checked_out_at`.
    let _head = f.next();
    let date = f.next().unwrap_or("").to_string();
    let subject = f.next().unwrap_or("").to_string();
    let upstream_name = f.next().unwrap_or("");
    let track = f.next().unwrap_or("");
    let author = f.next().unwrap_or("").to_string();
    let refname = f.next().unwrap_or("");

    // The reference decides when it is there; without it — a line written by an
    // older format — the short name is all there is to go on.
    let kind = if refname.starts_with("refs/remotes/") {
        BranchKind::Remote
    } else if refname.starts_with("refs/heads/") {
        BranchKind::Local
    } else if name.contains('/') && !locals.contains(name.as_str()) {
        BranchKind::Remote
    } else {
        BranchKind::Local
    };

    if kind == BranchKind::Remote {
        // `refs/remotes/origin/HEAD` is an alias, not a branch.
        if name.ends_with("/HEAD") {
            return None;
        }
        // A remote already present locally adds nothing to the selector.
        if let Some((_, short)) = name.split_once('/') {
            if locals.contains(short) {
                return None;
            }
        }
    }

    Some(Branch {
        name,
        kind,
        date,
        subject,
        author,
        upstream: (!upstream_name.is_empty()).then(|| {
            let (ahead, behind) = parse_track(track);
            Upstream {
                name: upstream_name.to_string(),
                ahead,
                behind,
            }
        }),
        checked_out_at: None,
    })
}

/// `%(upstream:track)` is "[ahead 2, behind 3]", "[gone]" or nothing.
fn parse_track(track: &str) -> (usize, usize) {
    let inner = track.trim().trim_start_matches('[').trim_end_matches(']');
    let mut ahead = 0;
    let mut behind = 0;
    for part in inner.split(',') {
        let part = part.trim();
        if let Some(n) = part.strip_prefix("ahead ") {
            ahead = n.trim().parse().unwrap_or(0);
        } else if let Some(n) = part.strip_prefix("behind ") {
            behind = n.trim().parse().unwrap_or(0);
        }
    }
    (ahead, behind)
}

pub fn current(dir: &Path) -> Option<String> {
    let name = git_opt(dir, &["symbolic-ref", "--short", "-q", "HEAD"])?;
    (!name.is_empty()).then_some(name)
}

pub fn local_exists(main: &Path, branch: &str) -> bool {
    git_ok(
        main,
        &[
            "show-ref",
            "--verify",
            "--quiet",
            &format!("refs/heads/{branch}"),
        ],
    )
}

/// True when `origin/feat` names a remote-tracking ref this repository has.
///
/// Asked before offering `--track`: the answer decides between creating a local
/// branch and handing git a name it will refuse.
pub fn remote_exists(main: &Path, branch: &str) -> bool {
    git_ok(
        main,
        &[
            "show-ref",
            "--verify",
            "--quiet",
            &format!("refs/remotes/{branch}"),
        ],
    )
}

/// The branch a remote-tracking name follows locally: `origin/feat/x` → `feat/x`.
///
/// A pure split on the first slash, which is what git's own DWIM does. A local
/// name is left alone — it may well contain a slash of its own, and the callers
/// only reach this with a name they already know to be remote.
pub fn short_name(branch: &str) -> &str {
    match branch.split_once('/') {
        Some((_, short)) if !short.is_empty() => short,
        _ => branch,
    }
}

/// Splits an upstream into the remote and the branch on it.
///
/// `origin/feat/x` is `origin` and `feat/x`: a remote's name never contains a
/// slash, a branch's often does, so the first one is the boundary. Pure and
/// tested — it decides the refspec a branch update is fetched with, and a wrong
/// split there fetches something that exists under another name.
pub fn split_remote(upstream: &str) -> Option<(&str, &str)> {
    let (remote, branch) = upstream.split_once('/')?;
    (!remote.is_empty() && !branch.is_empty()).then_some((remote, branch))
}

/// The upstream a branch tracks, in `origin/feat` form.
///
/// `None` when it tracks nothing, which is a normal answer and not a failure:
/// a branch Claudhub has just created has no remote counterpart yet.
pub fn upstream_of(main: &Path, branch: &str) -> Option<String> {
    git_opt(
        main,
        &[
            "rev-parse",
            "--abbrev-ref",
            &format!("{branch}@{{upstream}}"),
        ],
    )
    .filter(|name| !name.is_empty())
}

/// The worktree already holding this branch, if there is one.
pub fn checked_out_at(main: &Path, branch: &str) -> Option<PathBuf> {
    checked_out_map(main).remove(branch)
}

/// Every branch a checkout holds, from a single `worktree list`.
///
/// Asking branch by branch meant one fork per branch for an answer that does
/// not change between two of them.
fn checked_out_map(main: &Path) -> HashMap<String, PathBuf> {
    git_opt(main, &["worktree", "list", "--porcelain"])
        .map(|out| parse_checked_out(&out))
        .unwrap_or_default()
}

/// `worktree list --porcelain` is a paragraph per checkout: `worktree <path>`
/// opens it, `branch refs/heads/<name>` names what it holds — and a detached
/// HEAD has no such line at all.
fn parse_checked_out(out: &str) -> HashMap<String, PathBuf> {
    let mut holders = HashMap::new();
    let mut current: Option<&str> = None;
    for line in out.lines() {
        if let Some(path) = line.strip_prefix("worktree ") {
            current = Some(path);
        } else if let Some(refname) = line.strip_prefix("branch refs/heads/") {
            if let Some(path) = current {
                holders.insert(refname.to_string(), PathBuf::from(path));
            }
        }
    }
    holders
}

/// The divergence point between `branch` and its base: it is that commit the
/// "branch diff" view compares against HEAD, and not the base's tip —
/// otherwise the diff includes everything that landed on the base meanwhile,
/// which the branch's author neither wrote nor has to review.
pub fn merge_base(dir: &Path, a: &str, b: &str) -> Option<String> {
    git_opt(dir, &["merge-base", a, b]).filter(|s| !s.is_empty())
}

/// Guesses the repository's integration branch.
///
/// The order follows what is authoritative: what the remote declares as its
/// default branch, then the local configuration, then the two usual names —
/// and only if they exist.
pub fn default_base(main: &Path) -> Option<String> {
    if let Some(head) = git_opt(
        main,
        &["symbolic-ref", "--short", "-q", "refs/remotes/origin/HEAD"],
    ) {
        if let Some((_, short)) = head.split_once('/') {
            return Some(short.to_string());
        }
    }
    if let Some(name) = git_opt(main, &["config", "--get", "init.defaultBranch"]) {
        if local_exists(main, &name) {
            return Some(name);
        }
    }
    ["main", "master", "develop"]
        .into_iter()
        .find(|b| local_exists(main, b))
        .map(str::to_string)
}

/// Attaches a branch with no upstream to `origin/<branch>` so the first `git
/// push` does not need `-u`. The remote reference does not exist yet: that is
/// precisely what the push will create.
pub(crate) fn ensure_upstream(main: &Path, branch: &str) {
    let merge_key = format!("branch.{branch}.merge");
    if git_opt(main, &["config", "--get", &merge_key]).is_some() {
        return;
    }
    if git_opt(main, &["remote", "get-url", "origin"]).is_none() {
        return;
    }
    let _ = git(
        main,
        &["config", &format!("branch.{branch}.remote"), "origin"],
    );
    let _ = git(
        main,
        &["config", &merge_key, &format!("refs/heads/{branch}")],
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_a_local_branch_with_its_upstream() {
        let locals = HashSet::from(["main"]);
        let line =
            "main\0*\x002 hours ago\0Fix the rendering\0origin/main\0[ahead 1, behind 4]\0Zoé";
        let b = parse_ref(line, &locals).unwrap();
        assert_eq!(b.name, "main");
        assert_eq!(b.kind, BranchKind::Local);
        assert_eq!(b.subject, "Fix the rendering");
        assert_eq!(b.author, "Zoé");
        let up = b.upstream.unwrap();
        assert_eq!(up.name, "origin/main");
        assert_eq!((up.ahead, up.behind), (1, 4));
    }

    #[test]
    fn a_branch_without_upstream_has_none() {
        let line = "wt/try\0 \0yesterday\0Draft\0\0";
        let b = parse_ref(line, &HashSet::from(["wt/try"])).unwrap();
        assert_eq!(b.kind, BranchKind::Local, "a local name may contain a /");
        assert_eq!(b.upstream, None);
        // A missing field — output from before the author was added — does not
        // make the read fail.
        assert_eq!(b.author, "");
    }

    #[test]
    fn hides_remote_duplicates_and_head_alias() {
        let locals = HashSet::from(["main"]);
        assert!(parse_ref("origin/main\0 \0yesterday\0x\0\0", &locals).is_none());
        assert!(parse_ref("origin/HEAD\0 \0yesterday\0x\0\0", &locals).is_none());
        let b = parse_ref("origin/feature\0 \0yesterday\0x\0\0", &locals).unwrap();
        assert_eq!(b.kind, BranchKind::Remote);
    }

    #[test]
    fn an_upstream_splits_at_its_first_slash() {
        assert_eq!(split_remote("origin/main"), Some(("origin", "main")));
        // A branch name carries the slashes; a remote's name never does.
        assert_eq!(split_remote("origin/feat/x"), Some(("origin", "feat/x")));
        assert_eq!(split_remote("main"), None);
        assert_eq!(split_remote("origin/"), None);
    }

    #[test]
    fn a_remote_name_gives_up_its_remote() {
        assert_eq!(short_name("origin/feat/x"), "feat/x");
        // Nothing to strip: what comes back is what went in, so a caller that
        // passes a local name by mistake does not lose half of it.
        assert_eq!(short_name("main"), "main");
    }

    /// A detached checkout has no `branch` line, and the paragraph that follows
    /// must not inherit the previous one's.
    #[test]
    fn reads_which_checkout_holds_each_branch() {
        let out = "worktree /repo\nHEAD abc\nbranch refs/heads/main\n\n\
                   worktree /repo/wt/detached\nHEAD def\ndetached\n\n\
                   worktree /repo/wt/feat\nHEAD ghi\nbranch refs/heads/feat/x\n";
        let holders = parse_checked_out(out);
        assert_eq!(holders.get("main"), Some(&PathBuf::from("/repo")));
        assert_eq!(
            holders.get("feat/x"),
            Some(&PathBuf::from("/repo/wt/feat")),
            "a branch name carries slashes of its own"
        );
        assert_eq!(holders.len(), 2);
    }

    /// A local branch really called `origin/x`: the short name alone reads as a
    /// remote one, the full reference does not.
    #[test]
    fn the_reference_decides_local_from_remote() {
        let locals = HashSet::new();
        let line = "origin/x\0 \0yesterday\0Draft\0\0\0Zoé\0refs/heads/origin/x";
        assert_eq!(parse_ref(line, &locals).unwrap().kind, BranchKind::Local);
        let line = "origin/x\0 \0yesterday\0Draft\0\0\0Zoé\0refs/remotes/origin/x";
        assert_eq!(parse_ref(line, &locals).unwrap().kind, BranchKind::Remote);
    }

    #[test]
    fn parses_track_variants() {
        assert_eq!(parse_track("[ahead 3]"), (3, 0));
        assert_eq!(parse_track("[behind 2]"), (0, 2));
        assert_eq!(parse_track("[ahead 1, behind 2]"), (1, 2));
        // "[gone]": the upstream has vanished, neither ahead nor behind measurable.
        assert_eq!(parse_track("[gone]"), (0, 0));
        assert_eq!(parse_track(""), (0, 0));
    }
}
