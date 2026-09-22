//! Diffs: the list of touched files, and one file's content cut into hunks for
//! the review view.
//!
//! Claudhub does not compute a diff itself — git does it better, with rename
//! detection, `.gitattributes` rules and the user's filters. This module only
//! reads its unified output.

use std::path::{Path, PathBuf};

use anyhow::Result;

use super::{git, split_nul};

/// The empty tree's digest, as git computes it everywhere.
const EMPTY_TREE: &str = "4b825dc642cb6eb9a060e54bf8d69288fbee4904";

/// What the review compares.
///
/// `Hash` because each range's files are filed by range: two panels show two
/// lists at the same time, and they do not overlap.
/// Serialisable: a review note remembers the range it was taken in, and the
/// state store reads it back on the next launch.
#[derive(Debug, Clone, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub enum Range {
    /// Everything separating the working tree from HEAD, staged or not.
    ///
    /// Claudhub does not offer to compare the index and the working tree
    /// separately: the distinction is a git plumbing detail, and the review view
    /// renders it through one checkbox per file rather than two lists that have
    /// to be stitched back together mentally.
    Working,
    /// One specific commit, compared against its first parent.
    ///
    /// `parent` is explicit rather than derived from a `^`: a root commit has no
    /// parent, and `<sha>^` fails there instead of returning the first commit's
    /// full diff.
    Commit { id: String, parent: Option<String> },
    /// Branch review: from the divergence with `base` up to HEAD.
    ///
    /// Written `base...HEAD` (three dots) and not `base..HEAD`: the former
    /// starts at the divergence point, so it shows only what the branch wrote,
    /// where the latter would mix in everything that has landed on the base
    /// since — noise the reviewer has no business reading.
    Branch { base: String },
    /// What has changed since the last review point (`git::snapshot`): the
    /// state read then against the state on disk now, uncommitted work and
    /// untracked files on both sides.
    ///
    /// Two trees and not `git diff <point>`: that one walks the index, so a
    /// file untracked then and still untracked now read as deleted. The
    /// current side is built the way the point was, into a scratch index —
    /// see `snapshot::working_tree`.
    ///
    /// `point` is the commit and not the ref: the ref moves with the next
    /// point, and a list filed under it would silently change meaning.
    Since { point: String },
}

impl Range {
    /// The revisions handed to `git diff`, once whatever has to be computed
    /// has been. Only `Since` computes something: the tree of the state on
    /// disk, which is a command of its own.
    fn resolve(&self, dir: &Path) -> Result<Vec<String>> {
        match self {
            Self::Since { point } => Ok(vec![point.clone(), super::snapshot::working_tree(dir)?]),
            _ => Ok(self.args()),
        }
    }

    fn args(&self) -> Vec<String> {
        match self {
            Self::Working => vec!["HEAD".into()],
            // Never asked: `resolve` answers for it. Were it asked, the point
            // against the index would be the nearest honest reading.
            Self::Since { point } => vec![point.clone()],
            Self::Branch { base } => vec![format!("{base}...HEAD")],
            Self::Commit { id, parent } => match parent {
                Some(parent) => vec![parent.clone(), id.clone()],
                // The empty tree: the only comparison point for a commit with
                // no parent. Its digest is a git constant, the same in every
                // repository.
                None => vec![EMPTY_TREE.to_string(), id.clone()],
            },
        }
    }
}

/// A file in the review list, with its change volume.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct DiffFile {
    pub path: PathBuf,
    /// Former path of a rename.
    pub original: Option<PathBuf>,
    pub added: usize,
    pub removed: usize,
    /// git does not count a binary's lines: nothing to show on the text side.
    pub binary: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum DiffLineKind {
    Context,
    Added,
    Removed,
    /// "\ No newline at end of file" — to be shown, never counted.
    NoNewline,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct DiffLine {
    pub kind: DiffLineKind,
    /// Numbers in the old and the new file; an added line has no old number, a
    /// removed line no new one.
    pub old_no: Option<usize>,
    pub new_no: Option<usize>,
    /// The line's text, **without** the carriage return a CRLF file ends it
    /// with: that byte is the file's line ending and not something to paint.
    pub text: String,
    /// The line ended with `\r` before its `\n` — a CRLF file, or one CRLF
    /// line in a file of LFs. Kept apart from the text so that the view never
    /// sees it, and kept at all so that `hunk_patch` writes it back: a context
    /// line missing its `\r` does not match the file any more, and git refuses
    /// the patch.
    pub cr: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Hunk {
    /// The `@@ … @@` header as it is, with the section git adds to it.
    pub header: String,
    pub old_start: usize,
    pub new_start: usize,
    pub lines: Vec<DiffLine>,
}

/// A single file's diff.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct FileDiff {
    pub hunks: Vec<Hunk>,
    pub binary: bool,
    /// Diff truncated by git (very large file) or empty.
    pub empty: bool,
}

/// Lists the review range's files with their volume.
pub fn files(dir: &Path, range: &Range) -> Result<Vec<DiffFile>> {
    files_in(dir, range, &mut std::collections::HashSet::new())
}

fn files_in(
    dir: &Path,
    range: &Range,
    seen: &mut std::collections::HashSet<PathBuf>,
) -> Result<Vec<DiffFile>> {
    if !seen.insert(dir.canonicalize().unwrap_or_else(|_| dir.to_path_buf())) {
        return Ok(Vec::new());
    }
    let mut args: Vec<String> = vec![
        "diff".into(),
        "--numstat".into(),
        "-z".into(),
        "-M".into(),
        "--ignore-submodules=none".into(),
        "--submodule=short".into(),
    ];
    args.extend(range.resolve(dir)?);
    // The revision list is closed, as it is under `file` — which closes it
    // because it has a path to add. A base named like a directory is refused
    // outright otherwise: `ambiguous argument 'dev': both revision and
    // filename`.
    args.push("--".into());
    let out = git(dir, &args)?;
    let mut files = parse_numstat(&out);
    if matches!(range, Range::Working) {
        let submodules: Vec<_> = files
            .iter()
            .filter(|file| dir.join(&file.path).join(".git").exists())
            .map(|file| file.path.clone())
            .collect();
        for path in submodules {
            for mut file in files_in(&dir.join(&path), range, seen)? {
                file.path = path.join(file.path);
                file.original = file.original.map(|original| path.join(original));
                files.push(file);
            }
        }
    }
    Ok(files)
}

/// A single file's diff.
///
/// `context` is the number of lines around each change; the view raises it when
/// "more context" is asked for.
///
/// `original` is the path a rename came from, and it goes into the pathspec
/// beside the new one: `-M` pairs the two halves of a rename **among the paths
/// it was given**, so restricted to the new name alone it saw an added file and
/// no deleted one — and a file renamed with three lines changed read as a
/// whole file added.
pub fn file(
    dir: &Path,
    range: &Range,
    path: &Path,
    original: Option<&Path>,
    context: usize,
) -> Result<FileDiff> {
    let (owner, local) = if matches!(range, Range::Working) {
        super::repo::file_repository(dir, path)
    } else {
        (dir.to_path_buf(), path.to_path_buf())
    };
    // The former path belongs to the same repository as the new one, or it is
    // not a rename this diff can pair: a file moved into a submodule is two
    // changes in two repositories.
    let original = original.and_then(|original| {
        let (from, local) = if matches!(range, Range::Working) {
            super::repo::file_repository(dir, original)
        } else {
            (dir.to_path_buf(), original.to_path_buf())
        };
        (from == owner).then_some(local)
    });
    let (dir, path) = (owner.as_path(), local.as_path());
    // Once, before the two reads below: under `Since` it builds a tree.
    let revisions = range.resolve(dir)?;
    let read = |original: Option<&Path>| -> Result<String> {
        let mut args: Vec<String> = vec![
            super::LITERAL_PATHS.into(),
            "diff".into(),
            format!("-U{context}"),
            "-M".into(),
            // Without this, a `diff.external` or a `.gitattributes` driver
            // replaces the unified output with a format we do not know how to
            // read.
            "--no-ext-diff".into(),
            "--no-color".into(),
            "--ignore-submodules=none".into(),
            "--submodule=short".into(),
        ];
        args.extend(revisions.iter().cloned());
        args.push("--".into());
        args.extend(original.map(|original| original.to_string_lossy().into_owned()));
        args.push(path.to_string_lossy().into_owned());
        // Bytes and not `git`'s answer: that one strips the last line's `\r`,
        // and this diff is what a hunk's patch is rebuilt from.
        let out = super::git_bytes(dir, &args)?;
        Ok(String::from_utf8_lossy(&out).into_owned())
    };
    let mut out = read(original.as_deref())?;
    // The status and this range need not agree on the rename: a file moved
    // in the index and then rewritten in the tree is renamed for one and not
    // for the other. Unpaired, the two paths are two sections, and the first
    // one — the deletion, whenever the old name sorts first — is all
    // `parse_unified` reads. The new path alone is then the right question.
    if original.is_some() && !pairs_a_rename(&out) {
        out = read(None)?;
    }
    Ok(parse_unified(&out))
}

/// Does this diff open on a rename? Read off the extended header git writes
/// before the first hunk.
fn pairs_a_rename(out: &str) -> bool {
    out.lines()
        .take_while(|line| !line.starts_with("@@"))
        .any(|line| line.starts_with("rename from "))
}

/// The unstaged remainder of one file: index → working tree, plain
/// `git diff` with no range. On a partially staged file this is exactly what
/// the next commit would leave behind; on a fully staged one it is empty.
pub fn unstaged_file(dir: &Path, path: &Path, context: usize) -> Result<FileDiff> {
    let (owner, local) = super::repo::file_repository(dir, path);
    let (dir, path) = (owner.as_path(), local.as_path());
    let args: Vec<String> = vec![
        super::LITERAL_PATHS.into(),
        "diff".into(),
        format!("-U{context}"),
        "-M".into(),
        "--no-ext-diff".into(),
        "--no-color".into(),
        "--ignore-submodules=none".into(),
        "--submodule=short".into(),
        "--".into(),
        path.to_string_lossy().into_owned(),
    ];
    // Bytes, for `file`'s reason: the remainder's hunks are staged from this.
    let out = super::git_bytes(dir, &args)?;
    Ok(parse_unified(&String::from_utf8_lossy(&out)))
}

/// The raw text of what is staged, as git writes it.
///
/// Neither `DiffFile` nor `FileDiff`: that diff is not displayed, it is **read
/// by an agent** asked for a commit message. The unified format is precisely
/// what a model can read, and cutting it up only to recompose it afterwards
/// would just lose the headers that say which file changes.
///
/// The context is cut down to three lines: what is paid for here is the number
/// of tokens sent. A changed binary only puts one line in it — git does not
/// write it without `--text`, and that is what we want.
pub fn staged_text(dir: &Path) -> Result<String> {
    git(
        dir,
        &[
            "diff",
            "--cached",
            "-U3",
            "-M",
            "--no-ext-diff",
            "--no-color",
            "--ignore-submodules=none",
            "--submodule=short",
        ],
    )
}

/// An untracked file's diff: git does not know it, so `diff` alone returns
/// empty output. `--no-index` against `/dev/null` produces the same format as
/// for other files, which avoids a second display path.
pub fn untracked_file(dir: &Path, path: &Path) -> Result<FileDiff> {
    let full = dir.join(path);
    // `--no-index` exits with code 1 as soon as there is a difference, which is
    // the normal case here: the whole file *is* the difference. Going through
    // `git` would throw the output away along with the "error", and the file
    // displayed empty — that is what made a new file look unreadable.
    let out = super::git_tolerant(
        dir,
        &[
            "diff",
            "--no-index",
            "--no-color",
            "--no-ext-diff",
            "/dev/null",
            &full.to_string_lossy(),
        ],
        1,
    )?;
    Ok(parse_unified(&out))
}

/// `--numstat -z`: `added\tremoved\tpath\0`, and for a rename
/// `added\tremoved\t\0old\0new\0`.
fn parse_numstat(out: &str) -> Vec<DiffFile> {
    let mut files = Vec::new();
    let mut records = split_nul(out);
    while let Some(rec) = records.next() {
        let mut f = rec.splitn(3, '\t');
        let added = f.next().unwrap_or("");
        let removed = f.next().unwrap_or("");
        let path = f.next().unwrap_or("");
        // git writes "-" for a binary, whose line counts make no sense.
        let binary = added == "-" || removed == "-";
        let (path, original) = if path.is_empty() {
            // Rename: the path is empty and followed by two records.
            let old = records.next().unwrap_or("");
            let new = records.next().unwrap_or("");
            (new.to_string(), Some(PathBuf::from(old)))
        } else {
            (path.to_string(), None)
        };
        if path.is_empty() {
            continue;
        }
        files.push(DiffFile {
            path: PathBuf::from(path),
            original,
            added: added.parse().unwrap_or(0),
            removed: removed.parse().unwrap_or(0),
            binary,
        });
    }
    files
}

pub(super) fn parse_unified(out: &str) -> FileDiff {
    let mut diff = FileDiff {
        empty: true,
        ..Default::default()
    };
    let mut old_no = 0usize;
    let mut new_no = 0usize;

    // Split on `\n` alone, and not by `lines()`: that one eats the `\r` of a
    // CRLF line, and the patch rebuilt from it no longer matched the file. The
    // `\r` is taken off here, once, and remembered on the line.
    let body = out.strip_suffix('\n').unwrap_or(out);
    for line in body.split('\n') {
        let (line, cr) = match line.strip_suffix('\r') {
            Some(line) => (line, true),
            None => (line, false),
        };
        if line.starts_with("@@") {
            let (old_start, new_start) = parse_hunk_header(line);
            old_no = old_start;
            new_no = new_start;
            diff.hunks.push(Hunk {
                header: line.to_string(),
                old_start,
                new_start,
                lines: Vec::new(),
            });
            diff.empty = false;
            continue;
        }
        if diff.hunks.is_empty() {
            // Still in the header (`diff --git`, `index`, `---`, `+++`).
            if line.starts_with("Binary files") || line.starts_with("GIT binary patch") {
                diff.binary = true;
                diff.empty = false;
            }
            continue;
        }
        let hunk = diff.hunks.last_mut().expect("a hunk is open");
        let (kind, text) = match line.as_bytes().first() {
            Some(b'+') => (DiffLineKind::Added, &line[1..]),
            Some(b'-') => (DiffLineKind::Removed, &line[1..]),
            Some(b' ') => (DiffLineKind::Context, &line[1..]),
            Some(b'\\') => (DiffLineKind::NoNewline, line),
            // An empty line in a unified diff is a context line whose leading
            // space git has trimmed.
            None => (DiffLineKind::Context, line),
            // The `diff --git` of a following file: we only read one file.
            _ => break,
        };
        let (l_old, l_new) = match kind {
            DiffLineKind::Added => {
                let n = new_no;
                new_no += 1;
                (None, Some(n))
            }
            DiffLineKind::Removed => {
                let n = old_no;
                old_no += 1;
                (Some(n), None)
            }
            DiffLineKind::Context => {
                let (a, b) = (old_no, new_no);
                old_no += 1;
                new_no += 1;
                (Some(a), Some(b))
            }
            DiffLineKind::NoNewline => (None, None),
        };
        hunk.lines.push(DiffLine {
            kind,
            old_no: l_old,
            new_no: l_new,
            text: text.to_string(),
            cr,
        });
    }
    diff
}

/// `@@ -12,7 +12,9 @@ fn something()` → (12, 12).
///
/// Only the two ranges between the `@@`s are read. What follows the second is
/// the function git names as context — a line of the file, which may well hold
/// a `-1` or a `+2` of its own: `@@ -15,3 +15,3 @@ limit = -1` read as starting
/// at line 1, and every number of the hunk was off.
fn parse_hunk_header(line: &str) -> (usize, usize) {
    let mut old = 1;
    let mut new = 1;
    let ranges = line
        .trim_start_matches('@')
        .split("@@")
        .next()
        .unwrap_or("");
    for tok in ranges.split_whitespace().take(2) {
        let (target, body) = match tok.as_bytes().first() {
            Some(b'-') => (&mut old, &tok[1..]),
            Some(b'+') => (&mut new, &tok[1..]),
            _ => continue,
        };
        let start = body.split(',').next().unwrap_or("");
        if let Ok(n) = start.parse::<usize>() {
            *target = n;
        }
    }
    (old, new)
}

/// Rebuilds an applicable patch for a single hunk.
///
/// This is what the view sends to `git apply --cached` to stage an isolated
/// piece: git has no "add that hunk" command, only the index and a patch.
pub fn hunk_patch(path: &Path, original: Option<&Path>, hunk: &Hunk, reverse: bool) -> String {
    let new_path = path.to_string_lossy();
    let old_path = original
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_else(|| new_path.to_string());
    let mut patch =
        format!("diff --git a/{old_path} b/{new_path}\n--- a/{old_path}\n+++ b/{new_path}\n");
    let (old_count, new_count) = hunk.lines.iter().fold((0, 0), |(o, n), l| match l.kind {
        DiffLineKind::Added => (o, n + 1),
        DiffLineKind::Removed => (o + 1, n),
        DiffLineKind::Context => (o + 1, n + 1),
        DiffLineKind::NoNewline => (o, n),
    });
    patch.push_str(&format!(
        "@@ -{},{} +{},{} @@\n",
        hunk.old_start, old_count, hunk.new_start, new_count
    ));
    for line in &hunk.lines {
        match line.kind {
            DiffLineKind::Added => patch.push('+'),
            DiffLineKind::Removed => patch.push('-'),
            DiffLineKind::Context => patch.push(' '),
            DiffLineKind::NoNewline => {}
        }
        patch.push_str(&line.text);
        if line.cr {
            patch.push('\r');
        }
        patch.push('\n');
    }
    // `reverse` does not flip the text: `git apply --reverse` takes care of it,
    // and does it right, including for file endings without a newline.
    let _ = reverse;
    patch
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A new file reads whole.
    ///
    /// `--no-index` exits with code 1 as soon as it finds a difference, and that
    /// is the normal case here: the whole file *is* the difference. A read
    /// treating that code as a failure returns an empty diff, and the view shows
    /// "no change" on a file that is nothing but additions.
    #[test]
    fn an_untracked_file_is_read_whole() {
        let dir = tempdir();
        std::process::Command::new("git")
            .args(["init", "-q", "."])
            .current_dir(&dir)
            .status()
            .expect("git init");
        std::fs::write(dir.join("new.txt"), "one\ntwo\n").unwrap();

        let diff = untracked_file(&dir, Path::new("new.txt")).expect("read");
        let lines: Vec<&str> = diff
            .hunks
            .iter()
            .flat_map(|hunk| hunk.lines.iter())
            .map(|line| line.text.as_str())
            .collect();
        assert_eq!(lines, vec!["one", "two"]);
        assert!(diff
            .hunks
            .iter()
            .flat_map(|h| h.lines.iter())
            .all(|l| l.kind == DiffLineKind::Added));

        let _ = std::fs::remove_dir_all(&dir);
    }

    fn tempdir() -> PathBuf {
        let dir = std::env::temp_dir().join(format!("claudhub-diff-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("test directory");
        dir
    }

    #[test]
    fn reads_numstat_with_a_rename_and_a_binary() {
        let out = "3\t1\tsrc/main.rs\0\
                   12\t0\t\0assets/old name.svg\0assets/new name.svg\0\
                   -\t-\tassets/logo.png\0";
        let files = parse_numstat(out);
        assert_eq!(files.len(), 3);
        assert_eq!(files[0].path, PathBuf::from("src/main.rs"));
        assert_eq!((files[0].added, files[0].removed), (3, 1));

        assert_eq!(files[1].path, PathBuf::from("assets/new name.svg"));
        assert_eq!(
            files[1].original,
            Some(PathBuf::from("assets/old name.svg"))
        );

        assert!(files[2].binary, "git writes \"-\" for a binary");
        assert_eq!((files[2].added, files[2].removed), (0, 0));
    }

    #[test]
    fn numbers_lines_on_both_sides() {
        let out = "\
diff --git a/src/lib.rs b/src/lib.rs
index 1234567..89abcde 100644
--- a/src/lib.rs
+++ b/src/lib.rs
@@ -10,4 +10,5 @@ impl Repo {
 fn unchanged() {
-    old_one();
+    new_one();
+    added();
 }
";
        let d = parse_unified(out);
        assert!(!d.empty && !d.binary);
        assert_eq!(d.hunks.len(), 1);
        let h = &d.hunks[0];
        assert_eq!((h.old_start, h.new_start), (10, 10));

        let l = &h.lines;
        assert_eq!(l[0].kind, DiffLineKind::Context);
        assert_eq!((l[0].old_no, l[0].new_no), (Some(10), Some(10)));
        // A removal advances the old file's counter only.
        assert_eq!(l[1].kind, DiffLineKind::Removed);
        assert_eq!((l[1].old_no, l[1].new_no), (Some(11), None));
        assert_eq!(l[2].kind, DiffLineKind::Added);
        assert_eq!((l[2].old_no, l[2].new_no), (None, Some(11)));
        assert_eq!(l[3].new_no, Some(12));
        // The final context line resumes after both sides.
        assert_eq!((l[4].old_no, l[4].new_no), (Some(12), Some(13)));
        assert_eq!(l[1].text, "    old_one();");
    }

    #[test]
    fn detects_a_binary_file() {
        let out =
            "diff --git a/logo.png b/logo.png\nBinary files a/logo.png and b/logo.png differ\n";
        let d = parse_unified(out);
        assert!(d.binary);
        assert!(d.hunks.is_empty());
    }

    #[test]
    fn an_empty_diff_stays_empty() {
        assert!(parse_unified("").empty);
    }

    #[test]
    fn rebuilds_an_applicable_patch() {
        let out = "\
@@ -5,3 +5,4 @@
 context
-old
+new
+more
";
        let d = parse_unified(out);
        let patch = hunk_patch(Path::new("src/x.rs"), None, &d.hunks[0], false);
        assert!(patch.starts_with("diff --git a/src/x.rs b/src/x.rs\n"));
        // The counts are recomputed from the lines kept, not copied from the
        // original header: an isolated hunk can be shorter.
        assert!(patch.contains("@@ -5,2 +5,3 @@\n"), "patch = {patch}");
        assert!(patch.ends_with(" context\n-old\n+new\n+more\n"));
    }

    /// The `\r` of a CRLF line leaves the text and comes back in the patch —
    /// the last line's too, which `lines()` and a trimmed output both ate.
    #[test]
    fn a_crlf_line_keeps_its_carriage_return_out_of_the_text() {
        let out = "@@ -1,2 +1,2 @@\r\n one\r\n-two\r\n+TWO\r\n";
        let d = parse_unified(out);
        let lines = &d.hunks[0].lines;
        assert_eq!(lines.len(), 3);
        assert!(lines.iter().all(|line| line.cr));
        assert_eq!(lines[2].text, "TWO");
        let patch = hunk_patch(Path::new("w.txt"), None, &d.hunks[0], false);
        assert!(
            patch.ends_with("@@ -1,2 +1,2 @@\n one\r\n-two\r\n+TWO\r\n"),
            "{patch:?}"
        );
    }

    #[test]
    fn a_commit_compares_against_its_parent_or_the_empty_tree() {
        let with_parent = Range::Commit {
            id: "abc".into(),
            parent: Some("def".into()),
        };
        assert_eq!(with_parent.args(), vec!["def", "abc"]);

        // A root commit compares against the empty tree: `abc^` does not exist.
        let root = Range::Commit {
            id: "abc".into(),
            parent: None,
        };
        assert_eq!(root.args(), vec![EMPTY_TREE, "abc"]);
    }

    #[test]
    fn header_defaults_to_one_when_unparsable() {
        assert_eq!(parse_hunk_header("@@ -1 +1 @@"), (1, 1));
        assert_eq!(parse_hunk_header("@@ -0,0 +1,5 @@"), (0, 1));
    }

    /// The function git names after the second `@@` is a line of the file, and
    /// its numbers are not the hunk's.
    #[test]
    fn the_section_after_the_header_is_not_read_as_a_range() {
        assert_eq!(parse_hunk_header("@@ -15,3 +15,3 @@ limit = -1"), (15, 15));
        assert_eq!(parse_hunk_header("@@ -7 +9,2 @@ x = +2 - -3"), (7, 9));
    }

    /// A real repository of its own, with an author, and CRLF left alone.
    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("claudhub-diff-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("test directory");
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

    /// A hunk of a CRLF file is staged: its lines keep the `\r` git wrote, the
    /// last line included, and the view is never handed it.
    #[test]
    fn a_hunk_of_a_crlf_file_is_staged() {
        let dir = scratch("crlf");
        std::fs::write(dir.join("win.txt"), "one\r\ntwo\r\nthree\r\n").unwrap();
        sh(&dir, &["add", "win.txt"]);
        sh(&dir, &["commit", "-q", "-m", "first"]);
        std::fs::write(dir.join("win.txt"), "one\r\nTWO\r\nthree\r\n").unwrap();

        let diff = file(&dir, &Range::Working, Path::new("win.txt"), None, 3).unwrap();
        let hunk = &diff.hunks[0];
        assert!(hunk.lines.iter().all(|line| line.cr), "{hunk:?}");
        assert!(
            hunk.lines.iter().all(|line| !line.text.contains('\r')),
            "the line ending is not text to paint: {hunk:?}"
        );

        let patch = hunk_patch(Path::new("win.txt"), None, hunk, false);
        super::super::repo::apply_patch(&dir, &patch, false).expect("the patch applies");
        assert_eq!(
            sh(&dir, &["show", ":win.txt"]),
            "one\r\nTWO\r\nthree\r\n",
            "staged byte for byte"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A file renamed and then edited reads as its edit, not as a whole file
    /// added: the former path is part of the question.
    #[test]
    fn a_renamed_and_edited_file_shows_its_edit() {
        let dir = scratch("rename");
        let text: String = (1..=12).map(|n| format!("line {n}\n")).collect();
        std::fs::write(dir.join("old.txt"), &text).unwrap();
        sh(&dir, &["add", "old.txt"]);
        sh(&dir, &["commit", "-q", "-m", "first"]);
        sh(&dir, &["mv", "old.txt", "new.txt"]);
        std::fs::write(dir.join("new.txt"), text.replace("line 6\n", "line six\n")).unwrap();

        let path = Path::new("new.txt");
        let diff = file(&dir, &Range::Working, path, Some(Path::new("old.txt")), 3).unwrap();
        let changed: Vec<_> = diff
            .hunks
            .iter()
            .flat_map(|hunk| &hunk.lines)
            .filter(|line| line.kind != DiffLineKind::Context)
            .map(|line| (line.kind, line.text.as_str()))
            .collect();
        assert_eq!(
            changed,
            [
                (DiffLineKind::Removed, "line 6"),
                (DiffLineKind::Added, "line six")
            ]
        );

        // Rewritten past recognition, the two are no rename to this range: the
        // new path is then read alone, and not the deletion sorting first.
        std::fs::write(dir.join("new.txt"), "something else entirely\n").unwrap();
        let diff = file(&dir, &Range::Working, path, Some(Path::new("old.txt")), 3).unwrap();
        let lines: Vec<_> = diff.hunks.iter().flat_map(|hunk| &hunk.lines).collect();
        assert_eq!(lines.len(), 1, "{lines:?}");
        assert_eq!(lines[0].kind, DiffLineKind::Added);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A path with brackets is a path: the diff of `app/[id]/page.tsx` is not
    /// the diff of every `app/?/page.tsx`.
    #[test]
    fn a_path_with_brackets_is_not_a_pattern() {
        let dir = scratch("literal");
        for path in ["app/[id]", "app/i"] {
            std::fs::create_dir_all(dir.join(path)).unwrap();
            std::fs::write(dir.join(path).join("page.tsx"), "a\n").unwrap();
        }
        sh(&dir, &["add", "."]);
        sh(&dir, &["commit", "-q", "-m", "first"]);
        std::fs::write(dir.join("app/i/page.tsx"), "b\n").unwrap();

        let diff = file(
            &dir,
            &Range::Working,
            Path::new("app/[id]/page.tsx"),
            None,
            3,
        )
        .unwrap();
        assert!(diff.hunks.is_empty(), "{diff:?}");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
