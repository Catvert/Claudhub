//! The stash: the shelf a checkout puts unfinished work on.
//!
//! It is the one everyday git object Claudhub could see nothing of. Pulling
//! onto a dirty tree, switching to look at a colleague's branch, putting an
//! experiment aside for an hour — each of those is a stash, and each of them
//! sent the user back to a terminal beside a window that shows the changes.
//!
//! **The stash is shared by every worktree of a repository.** `refs/stash`
//! lives in the common `.git`, not in the checkout's own ref space: the list
//! read from a linked worktree is the list of the main one, to the entry. So
//! the list is keyed by the **main repository**, as the tags are — and a
//! `git stash push` still belongs to a checkout, since what it takes off the
//! table is that checkout's working tree.
//!
//! **Nothing here is addressed by index alone.** `stash@{0}` is a position in a
//! stack that any other worktree — or the user's own terminal — can shift under
//! us, and dropping the wrong one loses work git keeps no trace of. Every
//! gesture therefore carries the commit hash the panel was showing, and refuses
//! the moment the name no longer resolves to it. git offers no way around the
//! name itself: `git stash apply <sha>` is accepted, `git stash drop <sha>` is
//! not — "is not a stash reference" — so the name is what travels, and the hash
//! is what makes it safe.

use std::path::Path;

use anyhow::{bail, Result};

use super::{git, git_opt, git_reporting};

/// A stash entry as the panel lists it.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Stash {
    /// `stash@{0}`. The only thing git takes for a drop, and the thing that
    /// moves: it is a position, and it is checked against `hash` before use.
    pub name: String,
    /// Its rank in the stack, for the row's number.
    pub index: usize,
    /// The stash commit, in full. What the diff is asked of, and what says the
    /// name still means what it meant when the list was read.
    pub hash: String,
    pub short: String,
    /// The branch the work was taken from, read out of the reflog subject.
    pub branch: String,
    /// What it says: the message given, or the commit the branch was on when
    /// git wrote the message itself.
    pub subject: String,
    /// The message is git's own (`WIP on …`) rather than one that was typed.
    /// Worth telling apart: a named stash is one somebody meant to come back
    /// to, and the rest are the residue of an afternoon.
    pub wip: bool,
    /// Relative, as git phrases it.
    pub date: String,
}

/// The repository's stashes, most recent first — which is git's own order, the
/// stack's.
pub fn list(main: &Path) -> Result<Vec<Stash>> {
    // `-z` terminates each entry with a null byte, and the fields are separated
    // by one too: a stash message is free text, and the reflog subject it is
    // read from is the one place a newline could not survive anyway. Fixed
    // arity is what makes the split unambiguous.
    const FORMAT: &str = "%gd%x00%H%x00%h%x00%gs%x00%cr";

    let raw = git(
        main,
        &["stash", "list", "-z", &format!("--format={FORMAT}")],
    )?;
    Ok(parse(&raw))
}

fn parse(raw: &str) -> Vec<Stash> {
    let fields: Vec<&str> = raw.split('\0').collect();
    fields
        .chunks_exact(5)
        .enumerate()
        .filter_map(|(index, entry)| {
            let name = entry[0].trim();
            if name.is_empty() {
                return None;
            }
            let (branch, subject, wip) = describe(entry[3]);
            Some(Stash {
                name: name.to_string(),
                index,
                hash: entry[1].to_string(),
                short: entry[2].to_string(),
                branch,
                subject,
                wip,
                date: entry[4].to_string(),
            })
        })
        .collect()
}

/// Splits the reflog subject into the branch and what the entry says.
///
/// git writes `WIP on <branch>: <sha> <subject>` when it phrases the message
/// itself, and `On <branch>: <message>` when one was given. A ref name holds
/// neither a space nor a colon, so the first colon is the seam — and a subject
/// git wrote in some other form is kept whole rather than cut at a guess.
fn describe(reflog: &str) -> (String, String, bool) {
    let (rest, wip) = match reflog.strip_prefix("WIP on ") {
        Some(rest) => (rest, true),
        None => (reflog.strip_prefix("On ").unwrap_or(reflog), false),
    };
    match rest.split_once(':') {
        Some((branch, subject)) if !branch.contains(' ') => {
            (branch.to_string(), subject.trim().to_string(), wip)
        }
        _ => (String::new(), reflog.trim().to_string(), wip),
    }
}

/// Puts the checkout's changes aside.
///
/// `untracked` takes the files git does not know yet — without it they stay on
/// the disk, which is the surprise everyone has had once: the tree looks clean,
/// and the new file is still there. `keep_index` leaves what was staged staged,
/// for stashing only what is not going into the commit being written.
///
/// A repository with nothing to stash is **not** a failure: git says "No local
/// changes to save" and exits zero, and that sentence is what the balloon
/// shows.
pub fn push(
    dir: &Path,
    message: Option<&str>,
    untracked: bool,
    keep_index: bool,
) -> Result<String> {
    let mut args: Vec<String> = vec!["stash".into(), "push".into()];
    if untracked {
        args.push("--include-untracked".into());
    }
    if keep_index {
        args.push("--keep-index".into());
    }
    if let Some(message) = message.map(str::trim).filter(|m| !m.is_empty()) {
        args.push("--message".into());
        args.push(message.to_string());
    }
    git_reporting(dir, &args)
}

/// Restores a stash into the checkout, keeping it (`apply`) or taking it off
/// the stack (`pop`).
///
/// `index` restores what was staged as staged; without it everything comes back
/// as working-tree changes, which is git's default and what one wants nine
/// times in ten.
///
/// **A conflict is a failure here and a normal outcome there**: git applies
/// what it can, leaves the conflict markers in the files and exits non-zero —
/// and, for a `pop`, keeps the stash rather than dropping it. The caller
/// re-reads the status either way; see `runtime::stash_written`.
pub fn restore(dir: &Path, name: &str, hash: &str, pop: bool, index: bool) -> Result<String> {
    check(dir, name, hash)?;
    let mut args: Vec<String> = vec![
        "stash".into(),
        if pop { "pop".into() } else { "apply".into() },
    ];
    if index {
        args.push("--index".into());
    }
    args.push(name.to_string());
    git_reporting(dir, &args)
}

/// Throws a stash away. There is no undo: git keeps the commit in the object
/// database for a while, and nothing on screen can point at it any more.
pub fn drop(dir: &Path, name: &str, hash: &str) -> Result<String> {
    check(dir, name, hash)?;
    git_reporting(dir, &["stash", "drop", name])
}

/// Empties the stack.
pub fn clear(main: &Path) -> Result<()> {
    git(main, &["stash", "clear"]).map(|_| ())
}

/// Creates a branch at the commit the stash was made on and restores the stash
/// onto it.
///
/// git's own `stash branch`, and the way out of the one case where a plain
/// apply cannot work: the tree has moved since, and the stash no longer fits.
/// It drops the stash when it succeeds — that is git's behaviour, not ours.
pub fn branch(dir: &Path, name: &str, hash: &str, new_branch: &str) -> Result<String> {
    check(dir, name, hash)?;
    let new_branch = new_branch.trim();
    // The tags' check, and not a second one of its own: `check-ref-format`'s
    // rules are the same for both, and a branch name is where they were
    // written down first.
    if !super::tags::is_valid_name(new_branch) {
        bail!("invalid branch name: {new_branch}");
    }
    git_reporting(dir, &["stash", "branch", new_branch, name])
}

/// Does this name still designate the stash the panel was showing?
///
/// The stack is shared by every worktree and by the user's own terminal, and
/// `stash@{1}` becomes `stash@{0}` the moment anything drops the entry above
/// it. Checked before every gesture, because the one this protects against —
/// dropping the wrong stash — destroys work git never wrote down.
fn check(dir: &Path, name: &str, hash: &str) -> Result<()> {
    let name = check_name(name)?;
    match git_opt(
        dir,
        &["rev-parse", "--verify", &format!("{name}^{{commit}}")],
    ) {
        Some(at) if at.trim() == hash => Ok(()),
        Some(_) => bail!("the stash list has moved: {name} is no longer the entry shown"),
        None => bail!("no such stash: {name}"),
    }
}

/// Is this one of the names `git stash list` writes?
///
/// Names come from our own listing, so this is not an escaping measure — the
/// arguments never see a shell. It is what keeps a gesture from being aimed at
/// some other revision entirely: `git stash drop HEAD~3` is a sentence git is
/// perfectly willing to refuse in its own words, hours later.
pub fn is_stash_ref(name: &str) -> bool {
    let Some(rest) = name.strip_prefix("stash@{") else {
        return false;
    };
    let Some(number) = rest.strip_suffix('}') else {
        return false;
    };
    !number.is_empty() && number.chars().all(|c| c.is_ascii_digit())
}

fn check_name(name: &str) -> Result<&str> {
    let trimmed = name.trim();
    if !is_stash_ref(trimmed) {
        bail!("not a stash reference: {name}");
    }
    Ok(trimmed)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(reflog: &str) -> String {
        [
            "stash@{0}",
            "a".repeat(40).as_str(),
            "aaaaaaa",
            reflog,
            "now",
        ]
        .join("\0")
            + "\0"
    }

    /// The everyday case: a stash nobody named, whose message git wrote from
    /// the commit the branch was on.
    #[test]
    fn a_message_git_wrote_is_told_from_one_that_was_typed() {
        let stashes = parse(&entry("WIP on master: 5f8a1c2 Fix the thing"));
        let stash = &stashes[0];
        assert!(stash.wip);
        assert_eq!(stash.branch, "master");
        assert_eq!(stash.subject, "5f8a1c2 Fix the thing");

        let stashes = parse(&entry("On feature/x: put the migration aside"));
        let stash = &stashes[0];
        assert!(!stash.wip);
        assert_eq!(stash.branch, "feature/x");
        assert_eq!(stash.subject, "put the migration aside");
    }

    /// A message may hold anything the reflog does — colons included, and the
    /// branch is only the part before the **first** one.
    #[test]
    fn a_message_may_contain_a_colon() {
        let stashes = parse(&entry("On main: refs: split the loader in two"));
        assert_eq!(stashes[0].branch, "main");
        assert_eq!(stashes[0].subject, "refs: split the loader in two");
    }

    /// A subject in no form we know is kept whole: half a sentence read as a
    /// branch name would be worse than no branch at all.
    #[test]
    fn an_unknown_shape_is_kept_whole() {
        let stashes = parse(&entry("something else entirely"));
        assert!(stashes[0].branch.is_empty());
        assert_eq!(stashes[0].subject, "something else entirely");
    }

    /// The stack's order is git's, and the rank is the row's number.
    #[test]
    fn entries_keep_their_rank() {
        let raw = [
            "stash@{0}",
            "aaa",
            "aaa",
            "On main: last",
            "now", //
            "stash@{1}",
            "bbb",
            "bbb",
            "On main: first",
            "an hour ago",
            "",
        ]
        .join("\0");
        let stashes = parse(&raw);
        assert_eq!(stashes.len(), 2);
        assert_eq!(stashes[0].index, 0);
        assert_eq!(stashes[1].name, "stash@{1}");
        assert_eq!(stashes[1].date, "an hour ago");
    }

    /// An empty stack lists nothing rather than one empty row.
    #[test]
    fn an_empty_stack_lists_nothing() {
        assert!(parse("").is_empty());
        assert!(parse("\0").is_empty());
    }

    /// Every gesture is aimed by name, and a name that is not a stash's would
    /// let one be aimed at any revision at all.
    #[test]
    fn only_a_stash_name_is_taken() {
        assert!(is_stash_ref("stash@{0}"));
        assert!(is_stash_ref("stash@{12}"));
        assert!(!is_stash_ref("stash@{}"));
        assert!(!is_stash_ref("stash@{a}"));
        assert!(!is_stash_ref("HEAD~3"));
        assert!(!is_stash_ref(""));
        assert!(!is_stash_ref("refs/stash"));
    }
}
