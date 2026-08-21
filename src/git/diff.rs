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
}

impl Range {
    fn args(&self) -> Vec<String> {
        match self {
            Self::Working => vec!["HEAD".into()],
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
    pub text: String,
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
    let mut args: Vec<String> = vec!["diff".into(), "--numstat".into(), "-z".into(), "-M".into()];
    args.extend(range.args());
    let out = git(dir, &args)?;
    Ok(parse_numstat(&out))
}

/// A single file's diff.
///
/// `context` is the number of lines around each change; the view raises it when
/// "more context" is asked for.
pub fn file(dir: &Path, range: &Range, path: &Path, context: usize) -> Result<FileDiff> {
    let mut args: Vec<String> = vec![
        "diff".into(),
        format!("-U{context}"),
        "-M".into(),
        // Without this, a `diff.external` or a `.gitattributes` driver replaces
        // the unified output with a format we do not know how to read.
        "--no-ext-diff".into(),
        "--no-color".into(),
    ];
    args.extend(range.args());
    args.push("--".into());
    args.push(path.to_string_lossy().into_owned());
    let out = git(dir, &args)?;
    Ok(parse_unified(&out))
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

fn parse_unified(out: &str) -> FileDiff {
    let mut diff = FileDiff {
        empty: true,
        ..Default::default()
    };
    let mut old_no = 0usize;
    let mut new_no = 0usize;

    for line in out.lines() {
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
        });
    }
    diff
}

/// `@@ -12,7 +12,9 @@ fn something()` → (12, 12).
fn parse_hunk_header(line: &str) -> (usize, usize) {
    let mut old = 1;
    let mut new = 1;
    for tok in line.split_whitespace() {
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
}
