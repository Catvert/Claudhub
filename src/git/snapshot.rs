//! The review point: the state of a worktree as it was read, kept as a commit.
//!
//! "Since my last review" compares that state with the one on disk now. It has
//! to hold **what is not committed**: an agent iterating on a worktree mostly
//! leaves its work in the tree, and a point that only knew HEAD would say
//! "nothing changed" of a round that rewrote five files.
//!
//! **A scratch index, and not `git stash create`.** The stash builds its
//! commit without touching the user's index either, but it leaves the
//! untracked files out — the new file an agent has just written, which is
//! precisely what one reads first. `stash create -u` does not exist. So the
//! tree is built the way `git add -A` would build it, into an index of our own
//! named by `GIT_INDEX_FILE`: a copy of the user's, so that `add` finds the
//! stat information already there and only hashes what has moved — seeding it
//! empty would rehash every file of the checkout. The user's index is never
//! opened for writing, and what is staged stays staged exactly as it was.
//!
//! The scratch file lives in the checkout's own git directory, **on the
//! worker's side**: under Windows the workers run in WSL, and a temporary file
//! is never a path that has to cross from one world to the other. It is not
//! named `index`, which is what the watcher reacts to.
//!
//! The tree becomes a **commit** (`commit-tree`) — it carries the moment it was
//! taken, which the base selector shows, and the HEAD of that moment as its
//! parent — and the commit is held by a ref under `refs/claudhub/review/`, or
//! the next `gc` would take it: nothing else reaches it. One ref per worktree,
//! overwritten by the next point; the refs whose worktree is gone are dropped
//! whenever a point is set (`prune`). `git log --all` would show them in the
//! branches' graph, which is why `history` leaves that namespace out.

use std::ffi::OsStr;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use anyhow::{Context, Result};

use super::{git, git_env, git_opt};

/// Where the points live. Outside `refs/heads` and `refs/tags`: they are no
/// branch anybody checks out, and no tag anybody pushes.
pub const REF_ROOT: &str = "refs/claudhub/review";

/// The line of the commit's message that names its worktree — what `prune`
/// reads back. The ref's own name is a digest, which cannot be read back.
const WORKTREE_LINE: &str = "Worktree: ";

/// A review point as the view keeps it: the commit, and when it was set.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Point {
    pub commit: String,
    /// Unix seconds. Written into the commit as its date, so the two agree.
    pub at: i64,
}

/// The tree of the working state: tracked files as they are on disk, the
/// untracked ones `.gitignore` lets through, deletions applied — what
/// `git add -A` then `git write-tree` would give, without the user's index
/// knowing anything of it.
///
/// A submodule is recorded at its HEAD, as `add` records any gitlink: what is
/// uncommitted inside it is not part of the point.
pub fn working_tree(dir: &Path) -> Result<String> {
    let index = ScratchIndex::new(dir)?;
    let env = [("GIT_INDEX_FILE", index.path.as_os_str())];
    git_env(dir, &["add", "-A"], &env)?;
    git_env(dir, &["write-tree"], &env)
}

/// Sets the worktree's review point to what is on disk now.
pub fn mark(dir: &Path) -> Result<Point> {
    let tree = working_tree(dir)?;
    let at = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| elapsed.as_secs() as i64)
        .unwrap_or(0);
    let date = format!("@{at} +0000");
    let worktree_line = format!("{WORKTREE_LINE}{}", dir.display());
    let mut args: Vec<&str> = vec![
        "commit-tree",
        // `commit-tree` reads `commit.gpgSign`: a point is no commit anybody
        // publishes, and a signature would be a pinentry prompt nobody sees.
        "--no-gpg-sign",
        &tree,
        "-m",
        "Claudhub review point",
        "-m",
        &worktree_line,
    ];
    let head = git_opt(dir, &["rev-parse", "--verify", "-q", "HEAD^{commit}"]);
    if let Some(head) = head.as_deref() {
        args.extend(["-p", head]);
    }
    // An identity of our own: the user's may not be configured, and the
    // point is not their commit anyway.
    let env: [(&str, &OsStr); 6] = [
        ("GIT_AUTHOR_NAME", OsStr::new("Claudhub")),
        ("GIT_AUTHOR_EMAIL", OsStr::new("claudhub@localhost")),
        ("GIT_COMMITTER_NAME", OsStr::new("Claudhub")),
        ("GIT_COMMITTER_EMAIL", OsStr::new("claudhub@localhost")),
        ("GIT_AUTHOR_DATE", OsStr::new(&date)),
        ("GIT_COMMITTER_DATE", OsStr::new(&date)),
    ];
    let commit = git_env(dir, &args, &env)?;
    git(
        dir,
        &[
            "update-ref",
            "-m",
            "claudhub: review point",
            &ref_name(dir),
            &commit,
        ],
    )?;
    // Best effort: a ref left behind costs a few objects, a point refused
    // because of a neighbour's would cost the gesture.
    if let Err(e) = prune(dir) {
        log::debug!("review points not pruned: {e:#}");
    }
    Ok(Point { commit, at })
}

/// The ref holding a worktree's point.
///
/// A digest of the path and not the path: a ref name refuses spaces, `..`,
/// `~`, `:` and a dozen other things a folder name accepts. FNV-1a, written
/// here because `DefaultHasher` promises nothing from one Rust to the next,
/// and a name that changes with the compiler would orphan every point.
pub fn ref_name(worktree: &Path) -> String {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in worktree.as_os_str().as_encoded_bytes() {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    format!("{REF_ROOT}/{hash:016x}")
}

/// Drops the points whose worktree is no longer on disk — a `wt rm`, a
/// `git worktree remove`: nothing else would ever delete their ref, and their
/// objects would outlive every `gc`.
fn prune(dir: &Path) -> Result<()> {
    let out = git(
        dir,
        &[
            "for-each-ref",
            "--format=%(refname)%00%(contents:body)%00",
            REF_ROOT,
        ],
    )?;
    for (name, worktree) in parse_points(&out) {
        if !worktree.join(".git").exists() {
            git(dir, &["update-ref", "-d", &name])?;
        }
    }
    Ok(())
}

/// `for-each-ref`'s answer: each ref's name and the worktree its message
/// names. A ref whose message names none is not ours to judge, and is left
/// out.
fn parse_points(out: &str) -> Vec<(String, PathBuf)> {
    let fields: Vec<&str> = out.split('\0').collect();
    fields
        .chunks(2)
        .filter_map(|pair| {
            let [name, body] = pair else { return None };
            let name = name.trim_matches('\n');
            let worktree = body
                .lines()
                .find_map(|line| line.strip_prefix(WORKTREE_LINE))?;
            (!name.is_empty()).then(|| (name.to_string(), PathBuf::from(worktree)))
        })
        .collect()
}

/// A copy of the user's index, removed when dropped.
struct ScratchIndex {
    path: PathBuf,
}

impl ScratchIndex {
    fn new(dir: &Path) -> Result<Self> {
        // `--git-path` and not `<git-dir>/index`: a linked worktree's index is
        // in its own directory, not in the common one. The answer is relative
        // to `dir` in the main checkout.
        let real = PathBuf::from(git(dir, &["rev-parse", "--git-path", "index"])?);
        let real = if real.is_absolute() {
            real
        } else {
            dir.join(real)
        };
        // Unique per call: three read workers may build a tree at once.
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let path = real.with_file_name(format!(
            "claudhub-snapshot-{}-{}.index",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        match std::fs::copy(&real, &path) {
            Ok(_) => {}
            // A repository where nothing was ever added has no index: `add`
            // starts from nothing, which is what the real one would do.
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => {
                return Err(e).with_context(|| format!("copying {}", real.display()));
            }
        }
        Ok(Self { path })
    }
}

impl Drop for ScratchIndex {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
        // A command killed by the timeout leaves its lock behind.
        let mut lock = self.path.clone().into_os_string();
        lock.push(".lock");
        let _ = std::fs::remove_file(lock);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::git::diff::{self, Range};

    fn scratch(name: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("claudhub-snapshot-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("test directory");
        let dir = dir.canonicalize().unwrap();
        for args in [
            &["init", "-q", "."][..],
            &["config", "user.email", "t@example.com"],
            &["config", "user.name", "T"],
            &["config", "core.autocrlf", "false"],
        ] {
            sh(&dir, args);
        }
        dir
    }

    fn sh(dir: &Path, args: &[&str]) -> String {
        let out = std::process::Command::new("git")
            .arg("-C")
            .arg(dir)
            .args(args)
            .output()
            .expect("git");
        assert!(
            out.status.success(),
            "git {args:?}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8_lossy(&out.stdout).into_owned()
    }

    fn ok(dir: &Path, args: &[&str]) -> bool {
        std::process::Command::new("git")
            .arg("-C")
            .arg(dir)
            .args(args)
            .output()
            .expect("git")
            .status
            .success()
    }

    /// Three files committed, then a round of work: one change staged, one
    /// left in the tree, one file deleted, one new and untracked.
    fn worked_on(name: &str) -> PathBuf {
        let dir = scratch(name);
        std::fs::write(dir.join("staged.txt"), "one\n").unwrap();
        std::fs::write(dir.join("tree.txt"), "one\n").unwrap();
        std::fs::write(dir.join("gone.txt"), "one\n").unwrap();
        std::fs::write(dir.join(".gitignore"), "*.log\n").unwrap();
        sh(&dir, &["add", "."]);
        sh(&dir, &["commit", "-q", "-m", "first"]);
        std::fs::write(dir.join("staged.txt"), "one\ntwo\n").unwrap();
        sh(&dir, &["add", "staged.txt"]);
        std::fs::write(dir.join("tree.txt"), "one\nTWO\n").unwrap();
        std::fs::remove_file(dir.join("gone.txt")).unwrap();
        std::fs::write(dir.join("new.txt"), "fresh\n").unwrap();
        std::fs::write(dir.join("noise.log"), "ignored\n").unwrap();
        dir
    }

    #[test]
    fn a_point_holds_the_uncommitted_work_and_leaves_the_index_alone() {
        let dir = worked_on("mark");
        let index_before = sh(&dir, &["ls-files", "--stage"]);
        let status_before = sh(&dir, &["status", "--porcelain"]);
        let index_bytes = std::fs::read(dir.join(".git/index")).unwrap();

        let point = mark(&dir).unwrap();

        // The user's index: not one byte, and git reads the same state.
        assert_eq!(std::fs::read(dir.join(".git/index")).unwrap(), index_bytes);
        assert_eq!(sh(&dir, &["ls-files", "--stage"]), index_before);
        assert_eq!(sh(&dir, &["status", "--porcelain"]), status_before);

        // The point: every file as it is on disk, staged or not, tracked or
        // not — and neither the deleted one nor the ignored one.
        let at = |path: &str| sh(&dir, &["show", &format!("{}:{path}", point.commit)]);
        assert_eq!(at("staged.txt"), "one\ntwo\n");
        assert_eq!(at("tree.txt"), "one\nTWO\n");
        assert_eq!(at("new.txt"), "fresh\n");
        assert!(!ok(
            &dir,
            &["cat-file", "-e", &format!("{}:gone.txt", point.commit)]
        ));
        assert!(!ok(
            &dir,
            &["cat-file", "-e", &format!("{}:noise.log", point.commit)]
        ));

        // Held by a ref, dated by the commit itself, child of HEAD.
        assert_eq!(
            sh(&dir, &["rev-parse", &ref_name(&dir)]).trim(),
            point.commit
        );
        assert_eq!(
            sh(&dir, &["show", "-s", "--format=%ct", &point.commit]).trim(),
            point.at.to_string()
        );
        assert_eq!(
            sh(&dir, &["rev-parse", &format!("{}^", point.commit)]),
            sh(&dir, &["rev-parse", "HEAD"])
        );

        // No scratch index left behind.
        let leftovers: Vec<_> = std::fs::read_dir(dir.join(".git"))
            .unwrap()
            .filter_map(|entry| entry.ok())
            .filter(|entry| entry.file_name().to_string_lossy().starts_with("claudhub-"))
            .collect();
        assert!(leftovers.is_empty(), "{leftovers:?}");
    }

    #[test]
    fn the_diff_since_a_point_is_only_what_moved_after_it() {
        let dir = worked_on("since");
        std::fs::write(dir.join("draft.txt"), "a\n").unwrap();
        let point = mark(&dir).unwrap();
        let range = Range::Since {
            point: point.commit.clone(),
        };
        assert!(diff::files(&dir, &range).unwrap().is_empty());

        // The next round: a file rewritten again, a new one, an untracked
        // file grown, another removed. `staged.txt` is left as it was.
        std::fs::write(dir.join("tree.txt"), "one\nTWO\nthree\n").unwrap();
        std::fs::write(dir.join("draft.txt"), "a\nb\n").unwrap();
        std::fs::write(dir.join("later.txt"), "a\nb\n").unwrap();
        std::fs::remove_file(dir.join("new.txt")).unwrap();

        let mut files = diff::files(&dir, &range).unwrap();
        files.sort_by(|a, b| a.path.cmp(&b.path));
        let summary: Vec<(String, usize, usize)> = files
            .iter()
            .map(|file| (file.path.display().to_string(), file.added, file.removed))
            .collect();
        assert_eq!(
            summary,
            vec![
                ("draft.txt".to_string(), 1, 0),
                ("later.txt".to_string(), 2, 0),
                ("new.txt".to_string(), 0, 1),
                ("tree.txt".to_string(), 1, 0),
            ]
        );

        // One file's diff: the line added since, numbered as on disk.
        let patch = diff::file(&dir, &range, Path::new("tree.txt"), None, 3).unwrap();
        let added: Vec<_> = patch.hunks[0]
            .lines
            .iter()
            .filter(|line| line.kind == crate::git::DiffLineKind::Added)
            .map(|line| (line.new_no, line.text.as_str()))
            .collect();
        assert_eq!(added, vec![(Some(3), "three")]);
        // A file untracked then and now reads as a change, not as a whole
        // file added: the point knew it.
        let draft = diff::file(&dir, &range, Path::new("draft.txt"), None, 3).unwrap();
        let lines: Vec<_> = draft.hunks[0]
            .lines
            .iter()
            .map(|line| (line.kind, line.text.as_str()))
            .collect();
        assert_eq!(
            lines,
            vec![
                (crate::git::DiffLineKind::Context, "a"),
                (crate::git::DiffLineKind::Added, "b"),
            ]
        );

        // Reading the diff wrote nothing into the user's index either.
        assert!(sh(&dir, &["ls-files"])
            .lines()
            .all(|path| path != "later.txt"));
    }

    #[test]
    fn a_point_whose_worktree_is_gone_is_pruned_by_the_next_one() {
        let dir = worked_on("prune");
        let tree = working_tree(&dir).unwrap();
        let orphan = sh(
            &dir,
            &[
                "commit-tree",
                &tree,
                "-m",
                "Claudhub review point",
                "-m",
                "Worktree: /nowhere/claudhub-gone",
            ],
        );
        let orphan_ref = format!("{REF_ROOT}/0000000000000000");
        sh(&dir, &["update-ref", &orphan_ref, orphan.trim()]);

        mark(&dir).unwrap();

        assert!(!ok(&dir, &["rev-parse", "--verify", "-q", &orphan_ref]));
        assert!(ok(&dir, &["rev-parse", "--verify", "-q", &ref_name(&dir)]));
    }

    #[test]
    fn a_ref_name_is_a_valid_ref_whatever_the_folder_is_called() {
        let odd = Path::new("/home/me/my project/..~:weird");
        let name = ref_name(odd);
        let dir = scratch("refname");
        assert!(ok(&dir, &["check-ref-format", &name]), "{name}");
        assert_ne!(name, ref_name(Path::new("/home/me/other")));
        // Stable: the same folder, the same ref, run after run.
        assert_eq!(name, ref_name(odd));
    }

    #[test]
    fn the_points_listing_is_read_back_by_name_and_worktree() {
        let out = "refs/claudhub/review/aa\0Worktree: /w/one\n\0\nrefs/claudhub/review/bb\0something else\n\0";
        assert_eq!(
            parse_points(out),
            vec![(
                "refs/claudhub/review/aa".to_string(),
                PathBuf::from("/w/one")
            )]
        );
    }
}
