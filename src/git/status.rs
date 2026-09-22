//! The working tree's state: what the "changed files" panel shows, and what it
//! stages on.
//!
//! The source is `git status --porcelain=v2 -z --branch`. v2 is the only one
//! that clearly separates the index's state from the working tree's (a file can
//! be added *and* modified again), gives the rename score, and separates paths
//! with null bytes — needed as soon as a file contains a space, a quote or a
//! newline.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use anyhow::Result;

use super::{git, split_nul};

/// One side's code (index or working tree) for a file.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum StatusCode {
    Unmodified,
    Modified,
    Added,
    Deleted,
    Renamed,
    Copied,
    TypeChanged,
    Untracked,
    Ignored,
    /// Merge conflict: both sides carry this code.
    Unmerged,
}

impl StatusCode {
    fn from_char(c: char) -> Self {
        match c {
            'M' => Self::Modified,
            'A' => Self::Added,
            'D' => Self::Deleted,
            'R' => Self::Renamed,
            'C' => Self::Copied,
            'T' => Self::TypeChanged,
            '?' => Self::Untracked,
            '!' => Self::Ignored,
            'U' => Self::Unmerged,
            _ => Self::Unmodified,
        }
    }

    /// The letter shown in the list, left of the path.
    pub fn letter(self) -> &'static str {
        match self {
            Self::Unmodified => " ",
            Self::Modified => "M",
            Self::Added => "A",
            Self::Deleted => "D",
            Self::Renamed => "R",
            Self::Copied => "C",
            Self::TypeChanged => "T",
            Self::Untracked => "?",
            Self::Ignored => "!",
            Self::Unmerged => "U",
        }
    }
}

/// A file as it appears in the review panel.
///
/// `index` and `worktree` are independent: a file staged then modified again is
/// `index: Modified, worktree: Modified` and appears on both sides of the list.
/// That is precisely what v1 made painful to read.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct FileStatus {
    pub path: PathBuf,
    /// Former path of a renamed or copied file.
    pub original: Option<PathBuf>,
    pub index: StatusCode,
    pub worktree: StatusCode,
    /// A gitlink's state relative to the index, separate from its staged change.
    pub submodule: Option<SubmoduleStatus>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct SubmoduleStatus {
    pub commit_changed: bool,
    pub modified: bool,
    pub untracked: bool,
}

impl SubmoduleStatus {
    fn parse(field: &str) -> Option<Self> {
        let bytes = field.as_bytes();
        (bytes.len() == 4 && bytes[0] == b'S').then(|| Self {
            commit_changed: bytes[1] == b'C',
            modified: bytes[2] == b'M',
            untracked: bytes[3] == b'U',
        })
    }
}

impl FileStatus {
    /// Has something to commit (at least part of it is in the index).
    pub fn is_staged(&self) -> bool {
        !matches!(self.index, StatusCode::Unmodified | StatusCode::Untracked)
    }

    /// Has changes outside the index.
    pub fn is_unstaged(&self) -> bool {
        !matches!(self.worktree, StatusCode::Unmodified)
    }

    /// Dirty files inside a submodule belong to its own index. Only a changed
    /// gitlink (or a deletion/type change) can be staged in the parent.
    pub fn is_stageable(&self) -> bool {
        self.is_unstaged()
            && self
                .submodule
                .is_none_or(|sub| sub.commit_changed || self.worktree != StatusCode::Modified)
    }

    pub fn is_untracked(&self) -> bool {
        self.index == StatusCode::Untracked || self.worktree == StatusCode::Untracked
    }

    pub fn is_conflicted(&self) -> bool {
        self.index == StatusCode::Unmerged || self.worktree == StatusCode::Unmerged
    }

    /// File name alone, for the left column; the directory is shown beside it,
    /// dimmed.
    pub fn file_name(&self) -> String {
        file_name(&self.path)
    }

    pub fn directory(&self) -> String {
        directory(&self.path)
    }
}

/// The name a list shows in its left column.
///
/// A free function: the review builds the same two columns for the ranges that
/// come from `--numstat`, where there is no `FileStatus` at all.
pub fn file_name(path: &Path) -> String {
    path.file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.display().to_string())
}

/// The folder shown beside the name, empty at the root.
pub fn directory(path: &Path) -> String {
    path.parent()
        .filter(|p| !p.as_os_str().is_empty())
        .map(|p| p.display().to_string())
        .unwrap_or_default()
}

/// A checkout's full state at a given moment.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Status {
    /// Current branch, `None` on a detached HEAD.
    pub branch: Option<String>,
    pub upstream: Option<String>,
    /// Commits ahead of and behind the upstream, when there is one.
    pub ahead: usize,
    pub behind: usize,
    pub files: Vec<FileStatus>,
    /// Changes inside initialized submodules, each with its own index.
    pub submodules: Vec<SubmoduleChanges>,
    /// Interrupted merge, rebase or cherry-pick.
    ///
    /// It lives in the status because it is read at the same moment and it
    /// changes how everything else reads: while it lasts, the index carries
    /// conflicts and `HEAD` does not point where you think.
    pub pending: Option<super::repo::Pending>,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct SubmoduleChanges {
    /// Relative to the repository holding this gitlink.
    pub path: PathBuf,
    pub status: Status,
}

impl Status {
    /// Local file edits, excluding changes to submodule commit references.
    pub fn has_file_changes(&self) -> bool {
        self.files
            .iter()
            .any(|file| file.submodule.is_none() && file.index != StatusCode::Ignored)
            || self
                .submodules
                .iter()
                .any(|sub| sub.status.has_file_changes())
    }

    /// The index and branch of a repository displayed inside this checkout.
    pub fn repository(&self, path: &Path) -> Option<&Status> {
        if path.as_os_str().is_empty() {
            return Some(self);
        }
        self.submodules.iter().find_map(|sub| {
            path.strip_prefix(&sub.path)
                .ok()
                .and_then(|local| sub.status.repository(local))
        })
    }

    /// Finds a displayed path, including paths inside a submodule.
    pub fn file(&self, path: &Path) -> Option<&FileStatus> {
        if let Some(file) = self.files.iter().find(|file| file.path == path) {
            return Some(file);
        }
        self.submodules.iter().find_map(|sub| {
            path.strip_prefix(&sub.path)
                .ok()
                .and_then(|path| sub.status.file(path))
        })
    }

    /// Splits a displayed path into its repository and repository-local path.
    /// The gitlink itself belongs to its parent; only its children move inside.
    pub fn file_location(&self, path: &Path) -> (PathBuf, PathBuf) {
        for sub in &self.submodules {
            if let Ok(local) = path.strip_prefix(&sub.path) {
                if !local.as_os_str().is_empty() {
                    let (repo, local) = sub.status.file_location(local);
                    return (crate::wslpath::join(&sub.path, repo), local);
                }
            }
        }
        (PathBuf::new(), path.to_path_buf())
    }

    pub fn files_recursive(&self) -> Vec<(PathBuf, &FileStatus)> {
        let mut files: Vec<_> = self
            .files
            .iter()
            .map(|file| (file.path.clone(), file))
            .collect();
        for sub in &self.submodules {
            files.extend(
                sub.status
                    .files_recursive()
                    .into_iter()
                    .map(|(path, file)| (crate::wslpath::join(&sub.path, path), file)),
            );
        }
        files
    }

    pub fn staged(&self) -> impl Iterator<Item = &FileStatus> {
        self.files.iter().filter(|f| f.is_staged())
    }

    pub fn unstaged(&self) -> impl Iterator<Item = &FileStatus> {
        self.files.iter().filter(|f| f.is_unstaged())
    }

    pub fn conflicted(&self) -> impl Iterator<Item = &FileStatus> {
        self.files.iter().filter(|f| f.is_conflicted())
    }

    pub fn is_clean(&self) -> bool {
        self.files.is_empty()
    }
}

/// Reads `dir`'s state.
///
/// Ignored files are not asked for: a review list drowned under `target/` or
/// `node_modules/` has no value, and enumerating them costs a full walk of the
/// excluded folders.
pub fn status(dir: &Path) -> Result<Status> {
    status_in(dir, &mut std::collections::HashSet::new())
}

fn status_in(dir: &Path, seen: &mut std::collections::HashSet<PathBuf>) -> Result<Status> {
    if !seen.insert(dir.canonicalize().unwrap_or_else(|_| dir.to_path_buf())) {
        return Ok(Status::default());
    }
    let mut args: Vec<String> = [
        "status",
        "--porcelain=v2",
        "--branch",
        "-z",
        // `all` and not `normal`: without it, a wholly new folder appears
        // as a single `folder/` entry that can neither be read nor staged
        // file by file — and an agent worktree creates some. The cost is a
        // full walk of the untracked *and non-ignored* folders, which
        // `.gitignore` already bounds.
        "--untracked-files=all",
        // The review must include gitlinks even when a CLI preference
        // hides them. This overrides config for this read only.
        "--ignore-submodules=none",
    ]
    .map(String::from)
    .to_vec();
    let out = match git(dir, &args) {
        Ok(out) => out,
        // A submodule whose `.git` file points at a `modules/` folder that is
        // gone makes git give up on the **parent**: it opens every gitlink to
        // say whether it is dirty, and one it cannot open is fatal. Left alone,
        // one stale checkout emptied the review of everything else. Those
        // gitlinks are left out by pathspec — the rest comes back, and they
        // simply are not listed, which is what git could say of them anyway.
        // Only in the failure path: finding them costs an `ls-files`.
        Err(e) => {
            let broken = broken_submodules(dir);
            if broken.is_empty() {
                return Err(e);
            }
            log::warn!(
                "{}: left out of the status, their git directory is gone: {}",
                dir.display(),
                broken
                    .iter()
                    .map(|path| path.to_string_lossy())
                    .collect::<Vec<_>>()
                    .join(", ")
            );
            args.push("--".into());
            args.extend(
                broken
                    .iter()
                    .map(|path| format!(":(exclude,literal){}", path.to_string_lossy())),
            );
            git(dir, &args)?
        }
    };
    let mut status = parse(&out);
    // The git directory and the markers are both read from disk — no second
    // process per refresh, and one refresh arrives per file write. That is the
    // price of not leaving the user in a half-finished state nothing names.
    status.pending = super::repo::git_dir(dir)
        .as_deref()
        .and_then(super::repo::pending_in);
    for file in &status.files {
        if file.submodule.is_some() && dir.join(&file.path).join(".git").exists() {
            // A submodule that cannot be read keeps its gitlink line in the
            // parent; it does not take the parent's status down with it.
            match status_in(&dir.join(&file.path), seen) {
                Ok(inner) => status.submodules.push(SubmoduleChanges {
                    path: file.path.clone(),
                    status: inner,
                }),
                Err(e) => log::warn!(
                    "submodule {} left unread: {e:#}",
                    dir.join(&file.path).display()
                ),
            }
        }
    }
    Ok(status)
}

/// The gitlinks of `dir` whose checkout has a `.git` leading nowhere: a file
/// naming a git directory that no longer exists.
///
/// An uninitialized gitlink has no `.git` at all and git skips it by itself;
/// only this half-removed state is fatal to `git status`.
fn broken_submodules(dir: &Path) -> Vec<PathBuf> {
    let Ok(out) = git(dir, &["ls-files", "-z", "--stage"]) else {
        return Vec::new();
    };
    split_nul(&out)
        .filter_map(|record| {
            let (header, path) = record.split_once('\t')?;
            header.starts_with("160000 ").then(|| PathBuf::from(path))
        })
        .filter(|path| {
            let checkout = dir.join(path);
            checkout.join(".git").exists() && super::repo::git_dir_on_disk(&checkout).is_none()
        })
        .collect()
}

fn parse(out: &str) -> Status {
    let mut status = Status::default();
    let mut records = split_nul(out);

    while let Some(rec) = records.next() {
        let mut chars = rec.chars();
        match chars.next() {
            Some('#') => parse_header(rec, &mut status),
            Some('1') => {
                if let Some(f) = parse_ordinary(rec) {
                    status.files.push(f);
                }
            }
            Some('2') => {
                // A rename takes two records: the entry, then the former path.
                // Consuming the second here is what keeps the iterator aligned
                // for what follows.
                let original = records.next().map(PathBuf::from);
                if let Some(mut f) = parse_ordinary(rec) {
                    f.original = original;
                    status.files.push(f);
                }
            }
            Some('u') => {
                if let Some(f) = parse_unmerged(rec) {
                    status.files.push(f);
                }
            }
            Some('?') => status.files.push(FileStatus {
                path: PathBuf::from(&rec[2..]),
                original: None,
                index: StatusCode::Untracked,
                worktree: StatusCode::Untracked,
                submodule: None,
            }),
            Some('!') => status.files.push(FileStatus {
                path: PathBuf::from(&rec[2..]),
                original: None,
                index: StatusCode::Ignored,
                worktree: StatusCode::Ignored,
                submodule: None,
            }),
            _ => {}
        }
    }
    status
}

fn parse_header(rec: &str, status: &mut Status) {
    let rest = rec.trim_start_matches("# ");
    if let Some(head) = rest.strip_prefix("branch.head ") {
        // git writes "(detached)" literally when there is no branch.
        status.branch = (head != "(detached)").then(|| head.to_string());
    } else if let Some(up) = rest.strip_prefix("branch.upstream ") {
        status.upstream = Some(up.to_string());
    } else if let Some(ab) = rest.strip_prefix("branch.ab ") {
        // Format "+2 -3".
        for part in ab.split_whitespace() {
            let n: usize = part[1..].parse().unwrap_or(0);
            match part.as_bytes().first() {
                Some(b'+') => status.ahead = n,
                Some(b'-') => status.behind = n,
                _ => {}
            }
        }
    }
}

/// `1 <XY> <sub> <mH> <mI> <mW> <hH> <hI> <path>` and the rename variant `2`,
/// including the submodule's independent commit/content/untracked flags.
fn parse_ordinary(rec: &str) -> Option<FileStatus> {
    let mut fields = rec.splitn(9, ' ');
    fields.next()?; // '1' or '2'
    let xy = fields.next()?;
    let mut xy = xy.chars();
    let index = StatusCode::from_char(xy.next()?);
    let worktree = StatusCode::from_char(xy.next()?);
    let submodule = SubmoduleStatus::parse(fields.next()?);
    // mH, mI, mW, hH, hI.
    for _ in 0..5 {
        fields.next()?;
    }
    let rest = fields.next()?;
    // For a rename, the score field (`R100`) precedes the path.
    let path = if index == StatusCode::Renamed
        || index == StatusCode::Copied
        || worktree == StatusCode::Renamed
        || worktree == StatusCode::Copied
    {
        rest.split_once(' ').map(|(_, p)| p)?
    } else {
        rest
    };
    Some(FileStatus {
        path: PathBuf::from(path),
        original: None,
        index,
        worktree,
        submodule,
    })
}

/// `u <XY> <sub> <m1> <m2> <m3> <mW> <h1> <h2> <h3> <path>`
fn parse_unmerged(rec: &str) -> Option<FileStatus> {
    let mut fields = rec.splitn(11, ' ');
    fields.next()?; // 'u'
    let xy = fields.next()?;
    let mut xy = xy.chars();
    let index = StatusCode::from_char(xy.next()?);
    let worktree = StatusCode::from_char(xy.next()?);
    let submodule = SubmoduleStatus::parse(fields.next()?);
    for _ in 0..7 {
        fields.next()?;
    }
    Some(FileStatus {
        path: PathBuf::from(fields.next()?),
        original: None,
        index,
        worktree,
        submodule,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rec(parts: &[&str]) -> String {
        let mut s = String::new();
        for p in parts {
            s.push_str(p);
            s.push('\0');
        }
        s
    }

    #[test]
    fn reads_branch_and_divergence() {
        let out = rec(&[
            "# branch.oid abc123",
            "# branch.head feature/x",
            "# branch.upstream origin/feature/x",
            "# branch.ab +2 -3",
        ]);
        let st = parse(&out);
        assert_eq!(st.branch.as_deref(), Some("feature/x"));
        assert_eq!(st.upstream.as_deref(), Some("origin/feature/x"));
        assert_eq!((st.ahead, st.behind), (2, 3));
        assert!(st.is_clean());
    }

    #[test]
    fn detached_head_has_no_branch() {
        let st = parse(&rec(&["# branch.head (detached)"]));
        assert_eq!(st.branch, None);
    }

    #[test]
    fn separates_index_from_worktree() {
        // File staged then modified again: both sides must appear.
        let out = rec(&["1 MM N... 100644 100644 100644 aaa bbb src/main.rs"]);
        let st = parse(&out);
        let f = &st.files[0];
        assert_eq!(f.path, PathBuf::from("src/main.rs"));
        assert_eq!(f.index, StatusCode::Modified);
        assert_eq!(f.worktree, StatusCode::Modified);
        assert!(f.is_staged() && f.is_unstaged());
    }

    #[test]
    fn reads_a_rename_and_its_original_path() {
        // The former path is a separate record in -z mode.
        let out = rec(&[
            "2 R. N... 100644 100644 100644 aaa bbb R100 ui/new name.rs",
            "ui/old name.rs",
            "1 .M N... 100644 100644 100644 ccc ddd src/lib.rs",
        ]);
        let st = parse(&out);
        assert_eq!(
            st.files.len(),
            2,
            "the former path must not become an entry"
        );
        assert_eq!(st.files[0].path, PathBuf::from("ui/new name.rs"));
        assert_eq!(st.files[0].original, Some(PathBuf::from("ui/old name.rs")));
        assert_eq!(st.files[0].index, StatusCode::Renamed);
        // The next entry did start again from the right record.
        assert_eq!(st.files[1].path, PathBuf::from("src/lib.rs"));
    }

    #[test]
    fn reads_untracked_and_conflicts() {
        let out = rec(&[
            "? new file.txt",
            "u UU N... 100644 100644 100644 100644 aaa bbb ccc src/conflict.rs",
        ]);
        let st = parse(&out);
        assert!(st.files[0].is_untracked());
        assert_eq!(st.files[0].path, PathBuf::from("new file.txt"));
        assert!(st.files[1].is_conflicted());
        assert_eq!(st.files[1].path, PathBuf::from("src/conflict.rs"));
        assert_eq!(st.conflicted().count(), 1);
    }

    #[test]
    fn submodule_flags_survive_renames_and_conflicts() {
        let st = parse(&rec(&[
            "2 RM SCMU 160000 160000 160000 aaa bbb R100 new submodule",
            "old submodule",
            "u UU S... 160000 160000 160000 160000 aaa bbb ccc conflicted submodule",
        ]));
        assert_eq!(st.files[0].original, Some("old submodule".into()));
        assert_eq!(
            st.files[0].submodule,
            Some(SubmoduleStatus {
                commit_changed: true,
                modified: true,
                untracked: true,
            })
        );
        assert!(st.files[1].is_conflicted());
        assert!(st.files[1].submodule.is_some());
    }
}

/// A checkout's summary: enough to describe it in one line in the sidebar,
/// without opening it.
///
/// Two commands rather than one because git has none that gives both:
/// `--numstat` counts lines but ignores what it does not track, and `status`
/// sees new files without knowing what they contain. An agent worktree is
/// precisely full of new files.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Summary {
    /// Files touched, new ones included.
    pub files: usize,
    pub added: usize,
    pub removed: usize,
}

impl Summary {
    pub fn is_empty(&self) -> bool {
        self.files == 0
    }
}

/// Past this size, a new file is not read.
///
/// This summary runs in a loop over every open worktree: a SQL dump or an
/// archive forgotten in a corner must not be re-read every ten seconds. The
/// file still counts as a touched file.
const MAX_UNTRACKED_READ: u64 = 1 << 20;

/// Counts the lines of the new files, which `--numstat` leaves out.
///
/// A binary file has no lines; it still counts as a touched file, which `files`
/// already carries.
fn untracked_lines(dir: &std::path::Path, status: &Status) -> usize {
    status
        .files
        .iter()
        .filter(|file| file.is_untracked())
        .map(|file| lines_of(&dir.join(&file.path)))
        .sum()
}

/// What says a file has not moved since it was last counted: its size and the
/// date it was modified — the same pair git itself trusts to skip a file.
type Stamp = (u64, Option<std::time::SystemTime>);

/// The line counts of the new files, from one summary to the next.
///
/// The summary runs over every open worktree every ten seconds, and re-reading
/// a megabyte of new files that have not changed is all it costs. An entry is
/// dropped when its file's stamp moves; the whole table is dropped when it
/// grows past what a session plausibly touches, which is what keeps a deleted
/// file from staying here for ever.
static COUNTS: std::sync::LazyLock<std::sync::Mutex<HashMap<PathBuf, (Stamp, usize)>>> =
    std::sync::LazyLock::new(|| std::sync::Mutex::new(HashMap::new()));

/// Past this many remembered files, the table is cleared rather than pruned:
/// forgetting costs one re-read, and pruning would need the list of what still
/// exists.
const MAX_REMEMBERED: usize = 10_000;

fn lines_of(path: &std::path::Path) -> usize {
    let Ok(meta) = std::fs::metadata(path) else {
        return 0;
    };
    if meta.len() > MAX_UNTRACKED_READ {
        return 0;
    }
    let stamp: Stamp = (meta.len(), meta.modified().ok());
    if let Ok(counts) = COUNTS.lock() {
        if let Some((seen, lines)) = counts.get(path) {
            if *seen == stamp {
                return *lines;
            }
        }
    }
    let lines = match std::fs::read(path) {
        // A binary file has no lines.
        Ok(bytes) if !bytes.contains(&0) => bytes.iter().filter(|b| **b == b'\n').count(),
        _ => 0,
    };
    if let Ok(mut counts) = COUNTS.lock() {
        if counts.len() >= MAX_REMEMBERED {
            counts.clear();
        }
        counts.insert(path.to_path_buf(), (stamp, lines));
    }
    lines
}

pub fn summary(dir: &std::path::Path) -> Result<Summary> {
    let status = status(dir)?;
    let files = status
        .files
        .iter()
        .filter(|file| !matches!(file.index, StatusCode::Ignored))
        .count();
    let changed = super::diff::files(dir, &super::DiffRange::Working)?;
    Ok(Summary {
        files,
        added: changed.iter().map(|f| f.added).sum::<usize>() + untracked_lines(dir, &status),
        removed: changed.iter().map(|f| f.removed).sum(),
    })
}

#[cfg(test)]
mod counting_tests {
    use super::*;

    /// A remembered count must not outlive the file it counted: a new file is
    /// written, counted, then written again — and the second answer is the new
    /// one.
    #[test]
    fn a_remembered_count_is_dropped_when_the_file_moves() {
        let dir = std::env::temp_dir().join(format!("claudhub-lines-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("new.rs");

        std::fs::write(&file, "one\ntwo\n").unwrap();
        assert_eq!(lines_of(&file), 2);
        // The same answer, this time from the table.
        assert_eq!(lines_of(&file), 2);

        std::fs::write(&file, "one\ntwo\nthree\n").unwrap();
        assert_eq!(lines_of(&file), 3);

        // A binary file has no lines, and neither has one that is gone.
        std::fs::write(&file, [b'a', 0, b'b']).unwrap();
        assert_eq!(lines_of(&file), 0);
        std::fs::remove_file(&file).unwrap();
        assert_eq!(lines_of(&file), 0);

        let _ = std::fs::remove_dir_all(&dir);
    }
}

#[cfg(test)]
mod broken_submodule_tests {
    use crate::git::submodule_tests::{add_submodule, Fixture};
    use std::path::Path;

    /// `rm -rf .git/modules/<name>` with the checkout left in place: git
    /// refuses the parent's whole status, and the review used to go blank.
    #[test]
    fn a_submodule_without_its_git_directory_does_not_empty_the_parent_status() {
        let f = Fixture::new();
        let healthy = add_submodule(&f.parent, "healthy", Path::new("deps/healthy"));
        std::fs::remove_dir_all(f.parent.join(".git/modules/app-tech")).unwrap();
        std::fs::write(f.parent.join("new file"), "x").unwrap();
        std::fs::write(healthy.join("src/deep/code.txt"), "after\n").unwrap();

        let st = super::status(&f.parent).expect("the rest of the status comes back");
        let paths: Vec<&Path> = st.files.iter().map(|file| file.path.as_path()).collect();
        assert!(paths.contains(&Path::new("new file")), "{paths:?}");
        // The healthy submodule is still read, dirt included.
        let gitlink = st
            .files
            .iter()
            .find(|file| file.path == Path::new("deps/healthy"))
            .expect("the healthy gitlink is listed");
        assert!(gitlink.submodule.is_some_and(|sub| sub.modified));
        assert!(st
            .submodules
            .iter()
            .any(|sub| sub.path == Path::new("deps/healthy")));
        // The broken one is left out, its name with a space and all.
        assert!(!paths.contains(&f.path.as_path()), "{paths:?}");
        assert_eq!(super::broken_submodules(&f.parent), vec![f.path.clone()]);
    }
}
