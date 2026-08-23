//! Branches: the selector's list, and the closed questions worktree operations
//! ask.

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
    /// True for HEAD's branch in the checkout being queried.
    pub is_head: bool,
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
    pub checked_out_at: Option<PathBuf>,
}

/// Lists the branches, local first then the remotes with no local twin, from
/// the most recent commit to the oldest.
pub fn list(main: &Path) -> Result<Vec<Branch>> {
    // The separator has to be a character a commit subject does not contain;
    // `%00` is written literally by for-each-ref as a null byte. The author
    // comes last: adding a field at the end keeps outputs written by an earlier
    // version readable, a missing field being the empty string.
    const FORMAT: &str = "%(refname:short)%00%(HEAD)%00%(committerdate:relative)%00\
                          %(contents:subject)%00%(upstream:short)%00%(upstream:track)%00\
                          %(authorname)";

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

    let locals: Vec<String> = git_opt(
        main,
        &["for-each-ref", "--format=%(refname:short)", "refs/heads"],
    )
    .unwrap_or_default()
    .lines()
    .map(str::to_string)
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
    for b in &mut branches {
        if b.kind == BranchKind::Local {
            b.checked_out_at = checked_out_at(main, &b.name);
        }
    }
    Ok(branches)
}

fn parse_ref(line: &str, locals: &[String]) -> Option<Branch> {
    let mut f = line.split('\0');
    let name = f.next()?.to_string();
    let head = f.next().unwrap_or("").trim() == "*";
    let date = f.next().unwrap_or("").to_string();
    let subject = f.next().unwrap_or("").to_string();
    let upstream_name = f.next().unwrap_or("");
    let track = f.next().unwrap_or("");
    let author = f.next().unwrap_or("").to_string();

    let kind = if name.contains('/') && !locals.iter().any(|l| l == &name) {
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
            if locals.iter().any(|l| l == short) {
                return None;
            }
        }
    }

    Some(Branch {
        name,
        kind,
        is_head: head,
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
    let out = git_opt(main, &["worktree", "list", "--porcelain"])?;
    let mut current: Option<&str> = None;
    let target = format!("refs/heads/{branch}");
    for line in out.lines() {
        if let Some(p) = line.strip_prefix("worktree ") {
            current = Some(p);
        } else if line.strip_prefix("branch ") == Some(target.as_str()) {
            return current.map(PathBuf::from);
        }
    }
    None
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
        let locals = vec!["main".to_string()];
        let line =
            "main\0*\x002 hours ago\0Fix the rendering\0origin/main\0[ahead 1, behind 4]\0Zoé";
        let b = parse_ref(line, &locals).unwrap();
        assert_eq!(b.name, "main");
        assert_eq!(b.kind, BranchKind::Local);
        assert!(b.is_head);
        assert_eq!(b.subject, "Fix the rendering");
        assert_eq!(b.author, "Zoé");
        let up = b.upstream.unwrap();
        assert_eq!(up.name, "origin/main");
        assert_eq!((up.ahead, up.behind), (1, 4));
    }

    #[test]
    fn a_branch_without_upstream_has_none() {
        let line = "wt/try\0 \0yesterday\0Draft\0\0";
        let b = parse_ref(line, &["wt/try".to_string()]).unwrap();
        assert_eq!(b.kind, BranchKind::Local, "a local name may contain a /");
        assert!(!b.is_head);
        assert_eq!(b.upstream, None);
        // A missing field — output from before the author was added — does not
        // make the read fail.
        assert_eq!(b.author, "");
    }

    #[test]
    fn hides_remote_duplicates_and_head_alias() {
        let locals = vec!["main".to_string()];
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
