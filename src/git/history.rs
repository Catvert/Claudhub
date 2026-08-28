//! The history, and the graph that makes it readable.
//!
//! A git history is a directed acyclic graph, not a list: two parallel
//! branches, a merge, and chronological order alone no longer says anything
//! about what descends from what. The graph is therefore computed here, in the
//! shape the view needs — one column per row and the lines that join them — and
//! not delegated to `git log --graph`, whose output is a drawing in characters
//! that would have to be re-parsed back into coordinates.

use std::path::{Path, PathBuf};

use anyhow::Result;

use super::{git, FileDiff};

/// A commit as the list shows it.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Commit {
    pub id: String,
    pub short: String,
    /// Parents in git's order: the first is the main line.
    pub parents: Vec<String>,
    pub summary: String,
    pub author: String,
    /// Relative date, as git phrases it.
    pub date: String,
    /// Branches and tags pointing at this commit.
    pub refs: Vec<String>,
}

impl Commit {
    pub fn is_merge(&self) -> bool {
        self.parents.len() > 1
    }
}

/// What the history shows.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum LogRange {
    /// The current checkout's history.
    Head,
    /// One named reference's history — a branch picked from the list beside the
    /// log, which is what reading a branch means before deciding to check it
    /// out. A remote-tracking name works the same: `origin/main` is a ref like
    /// any other.
    Ref { name: String },
    /// What the branch has added since it diverged from `base` — the same range
    /// as the branch review, seen as a sequence of commits.
    Branch { base: String },
    /// Every reference: this is where the graph earns its keep, parallel
    /// branches being visible side by side.
    All,
    /// The history of a handful of lines of one file — `git log -L`.
    ///
    /// The path is relative to the worktree and the lines are git's: 1-based,
    /// both ends included, and counted **in HEAD**, not in the buffer being
    /// edited. Whoever asks does the mapping — see `ui::hunks::to_base`.
    ///
    /// git follows renames on its own here; `--follow` neither is needed nor
    /// allowed.
    Lines {
        path: PathBuf,
        start: usize,
        end: usize,
    },
}

impl LogRange {
    /// The revisions this range names, **and the terminator that closes them**.
    ///
    /// A branch and a file may bear the same name — a `dev` directory beside a
    /// `dev` branch is the case that found this — and git refuses to guess:
    /// `ambiguous argument 'dev': both revision and filename`. Nothing here
    /// ever passes a pathspec, so the list is closed with nothing after it.
    /// `-L` carries its own path inside the option and is unaffected.
    fn args(&self) -> Vec<String> {
        let mut args = self.revisions();
        args.push("--".into());
        args
    }

    fn revisions(&self) -> Vec<String> {
        match self {
            Self::Head => vec!["HEAD".into()],
            Self::Ref { name } => vec![name.clone()],
            Self::Branch { base } => vec![format!("{base}..HEAD")],
            // `--all` without `--topo-order` would interleave the branches by
            // date, which gives an unreadable graph: the lines would jump from
            // one branch to another on every row.
            Self::All => vec!["--all".into(), "--topo-order".into()],
            // `-L` takes no pathspec and refuses `--graph`: the list it returns
            // has holes — a commit's parent is usually missing — which is why
            // the view paints no lanes in this mode.
            Self::Lines { path, start, end } => {
                vec!["-L".into(), format!("{start},{end}:{}", path.display())]
            }
        }
    }
}

/// Field separator. A control character no commit message contains, where `|`
/// or `\t` always end up appearing in a subject sooner or later.
const FIELD: char = '\u{1f}';

pub fn commits(dir: &Path, range: &LogRange, limit: usize) -> Result<Vec<Commit>> {
    let format = format!("--format=%H{f}%h{f}%P{f}%an{f}%ar{f}%D{f}%s", f = "%x1f");
    let mut args: Vec<String> = vec![
        "log".into(),
        "-z".into(),
        format,
        format!("--max-count={limit}"),
    ];
    // Without this, `-L` writes its patch after the format, and it would spill
    // into the next record's fields — `-z` separates commits, not sections.
    if matches!(range, LogRange::Lines { .. }) {
        args.push("--no-patch".into());
    }
    args.extend(range.args());
    let out = git(dir, &args)?;
    Ok(parse(&out))
}

/// Commits that touched those lines, each with the patch **restricted to
/// them** — which is the whole point of `-L`: "how these eight lines became
/// what they are", commit after commit, without ever replaying a `git diff`.
///
/// One command and not two: the line range is expressed in HEAD's numbering,
/// so asking a single older commit for its patch would count the lines from the
/// wrong revision.
///
/// The record separator is `\x01` at the **start of a line**. A patch line
/// always begins with a space, `+`, `-` or `\`, so that byte can only ever
/// appear at column two or beyond, where a file happens to contain it.
pub fn line_history(
    dir: &Path,
    path: &Path,
    start: usize,
    end: usize,
    limit: usize,
) -> Result<Vec<(Commit, FileDiff)>> {
    let format = format!(
        "--format=%x01%H{f}%h{f}%P{f}%an{f}%ar{f}%D{f}%s",
        f = "%x1f"
    );
    let range = LogRange::Lines {
        path: path.to_path_buf(),
        start,
        end,
    };
    let mut args: Vec<String> = vec![
        "log".into(),
        format,
        format!("--max-count={limit}"),
        // A `diff.external` or a `.gitattributes` driver would replace the
        // unified output with a format we do not know how to read.
        "--no-ext-diff".into(),
        "--no-color".into(),
    ];
    args.extend(range.args());
    let out = git(dir, &args)?;
    Ok(parse_line_history(&out))
}

fn parse_line_history(out: &str) -> Vec<(Commit, FileDiff)> {
    out.split('\u{1}')
        .filter(|record| !record.trim().is_empty())
        .filter_map(|record| {
            let (fields, patch) = record.split_once('\n').unwrap_or((record, ""));
            let commit = parse_commit(fields)?;
            Some((commit, super::diff::parse_unified(patch)))
        })
        .collect()
}

/// The subjects of the latest commits, most recent first.
///
/// They serve as examples for the agent proposing a message: a repository's
/// language, the person of the verb and any prefixes cannot be guessed, and an
/// instruction written here would impose them on every repository. A brand-new
/// repository has none, which is not an error — hence the empty list.
pub fn recent_subjects(dir: &Path, limit: usize) -> Vec<String> {
    super::git_opt(
        dir,
        &[
            "log".to_string(),
            "-z".to_string(),
            "--format=%s".to_string(),
            format!("--max-count={limit}"),
        ],
    )
    .map(|out| {
        super::split_nul(&out)
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .collect()
    })
    .unwrap_or_default()
}

/// A commit's full story, read for the block the diff shows above its files.
///
/// Separate from `Commit` on purpose: the list carries two thousand of those,
/// and the body — the only field the list does not show — is the one that can
/// weigh a screenful each.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommitDetail {
    pub author: String,
    pub date: String,
    /// The raw message, subject line included (`%B`).
    pub message: String,
}

/// Reads one commit's message, author and date.
pub fn detail(dir: &Path, id: &str) -> Result<CommitDetail> {
    let format = format!("--format=%an{f}%ar{f}%B", f = "%x1f");
    let out = git(
        dir,
        &["show".into(), "--no-patch".into(), format, id.to_string()],
    )?;
    Ok(parse_detail(&out))
}

fn parse_detail(out: &str) -> CommitDetail {
    let mut f = out.splitn(3, FIELD);
    CommitDetail {
        author: f.next().unwrap_or_default().to_string(),
        date: f.next().unwrap_or_default().to_string(),
        // `%B` keeps the newline every commit message ends with, and `git show`
        // adds its own after the format: both belong to the plumbing, not to
        // the message.
        message: f.next().unwrap_or_default().trim_end().to_string(),
    }
}

fn parse(out: &str) -> Vec<Commit> {
    super::split_nul(out).filter_map(parse_commit).collect()
}

fn parse_commit(record: &str) -> Option<Commit> {
    // `git log -z` separates commits with a null byte but keeps the newline
    // `--format` ends with; it would spill onto the next field.
    let record = record.trim_start_matches('\n');
    let mut f = record.split(FIELD);
    let id = f.next()?.to_string();
    if id.is_empty() {
        return None;
    }
    let short = f.next().unwrap_or_default().to_string();
    let parents = f
        .next()
        .unwrap_or_default()
        .split_whitespace()
        .map(str::to_string)
        .collect();
    let author = f.next().unwrap_or_default().to_string();
    let date = f.next().unwrap_or_default().to_string();
    let refs = parse_refs(f.next().unwrap_or_default());
    let summary = f.next().unwrap_or_default().to_string();

    Some(Commit {
        id,
        short,
        parents,
        summary,
        author,
        date,
        refs,
    })
}

/// `%D` yields "HEAD -> main, origin/main, tag: v1.2".
fn parse_refs(text: &str) -> Vec<String> {
    text.split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(|s| {
            // `HEAD -> main` names the checked-out branch: we keep the branch
            // name, the arrow teaching nothing the bullet already on the current
            // row does not.
            s.strip_prefix("HEAD -> ").unwrap_or(s).to_string()
        })
        .collect()
}

/// A commit's place in the graph.
#[derive(Debug, Clone, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
pub struct GraphRow {
    /// Column where the commit's bullet sits.
    pub column: usize,
    /// Columns crossed end to end by a vertical line, unrelated to this commit:
    /// other branches carrying on.
    pub through: Vec<usize>,
    /// Columns from which a line comes down to the bullet: the lanes this commit
    /// closes, that is, its children placed elsewhere.
    pub incoming: Vec<usize>,
    /// Columns a line leaves for under the bullet: its parents placed
    /// elsewhere, so the branches a merge brings together.
    pub outgoing: Vec<usize>,
}

/// Computes the graph's layout.
///
/// The algorithm is every history viewer's: we keep a list of lanes, each
/// waiting for one specific commit. A commit takes the lane that was waiting for
/// it — or opens one — installs its first parent there, and places its other
/// parents on neighbouring lanes. Freed lanes are reused before new ones are
/// opened, which keeps the graph narrow.
///
/// The output has exactly as many entries as the input: the view shows them
/// side by side, and being off by one row would make every line point at the
/// wrong commit.
pub fn layout(commits: &[Commit]) -> Vec<GraphRow> {
    // Each slot is the commit that lane is waiting for, or `None` if free.
    // Borrowed from the commits: a thousand-commit history copied every
    // identifier once per lane it travelled through.
    let mut lanes: Vec<Option<&str>> = Vec::new();
    let mut rows = Vec::with_capacity(commits.len());

    for commit in commits {
        // Every lane that was waiting for this commit: several children may be
        // waiting, and they all converge on the same bullet.
        let waiting: Vec<usize> = lanes
            .iter()
            .enumerate()
            .filter(|(_, lane)| **lane == Some(commit.id.as_str()))
            .map(|(ix, _)| ix)
            .collect();

        let column = match waiting.first() {
            Some(&first) => first,
            // Nobody was waiting for it: this is a branch tip.
            None => free_lane(&mut lanes),
        };

        // The crossing lanes are those that stay occupied and do not touch this
        // row. Collected before the update, otherwise the first parent about to
        // be installed would wrongly appear among them.
        let through: Vec<usize> = lanes
            .iter()
            .enumerate()
            .filter(|(ix, lane)| *ix != column && lane.is_some() && !waiting.contains(ix))
            .map(|(ix, _)| ix)
            .collect();

        // The surplus lanes that were waiting for this commit close here.
        let incoming: Vec<usize> = waiting.iter().skip(1).copied().collect();
        for &ix in &incoming {
            lanes[ix] = None;
        }

        // The first parent carries on in the commit's lane; with no parent, the
        // lane is freed (root commit).
        lanes[column] = commit.parents.first().map(String::as_str);

        let mut outgoing = Vec::new();
        for parent in commit.parents.iter().skip(1) {
            // A parent already awaited elsewhere does not deserve another lane:
            // the line joins the one that exists.
            let target = lanes
                .iter()
                .position(|lane| *lane == Some(parent.as_str()))
                .unwrap_or_else(|| {
                    let ix = free_lane(&mut lanes);
                    lanes[ix] = Some(parent.as_str());
                    ix
                });
            outgoing.push(target);
        }

        rows.push(GraphRow {
            column,
            through,
            incoming,
            outgoing,
        });
    }

    rows
}

/// The first free lane, or a new one. Reusing the gaps rather than stacking
/// keeps a graph from widening indefinitely along a slightly lively history.
fn free_lane(lanes: &mut Vec<Option<&str>>) -> usize {
    match lanes.iter().position(Option::is_none) {
        Some(ix) => ix,
        None => {
            lanes.push(None);
            lanes.len() - 1
        }
    }
}

/// Number of columns a graph occupies, to size the gutter.
pub fn width(rows: &[GraphRow]) -> usize {
    rows.iter()
        .map(|row| {
            let max = row
                .through
                .iter()
                .chain(row.incoming.iter())
                .chain(row.outgoing.iter())
                .copied()
                .max()
                .unwrap_or(0);
            max.max(row.column) + 1
        })
        .max()
        .unwrap_or(1)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn commit(id: &str, parents: &[&str]) -> Commit {
        Commit {
            id: id.into(),
            short: id.into(),
            parents: parents.iter().map(|p| p.to_string()).collect(),
            summary: format!("commit {id}"),
            author: "An author".into(),
            date: "yesterday".into(),
            refs: Vec::new(),
        }
    }

    fn record(fields: &[&str]) -> String {
        fields.join("\u{1f}")
    }

    #[test]
    fn a_detail_keeps_its_body_and_sheds_the_plumbing_newlines() {
        let out = "An author\u{1f}2 hours ago\u{1f}Fix the diff\n\nThe body,\non two lines.\n\n";
        let detail = parse_detail(out);
        assert_eq!(detail.author, "An author");
        assert_eq!(detail.date, "2 hours ago");
        assert_eq!(detail.message, "Fix the diff\n\nThe body,\non two lines.");
    }

    #[test]
    fn reads_a_commit_with_its_refs_and_parents() {
        let out = format!(
            "{}\0{}\0",
            record(&[
                "abc123def",
                "abc123d",
                "parent1 parent2",
                "An author",
                "2 hours ago",
                "HEAD -> main, origin/main, tag: v1.0",
                "Fix the diff rendering",
            ]),
            record(&[
                "parent1",
                "parent1",
                "",
                "Someone",
                "yesterday",
                "",
                "The initial commit",
            ]),
        );
        let commits = parse(&out);
        assert_eq!(commits.len(), 2);

        let first = &commits[0];
        assert_eq!(first.id, "abc123def");
        assert_eq!(first.parents, vec!["parent1", "parent2"]);
        assert!(first.is_merge());
        assert_eq!(first.summary, "Fix the diff rendering");
        assert_eq!(first.author, "An author");
        // The arrow of `HEAD -> main` is removed, the rest is kept.
        assert_eq!(first.refs, vec!["main", "origin/main", "tag: v1.0"]);

        // A root commit has no parent, and that is not an error.
        assert!(commits[1].parents.is_empty());
        assert!(commits[1].refs.is_empty());
    }

    #[test]
    fn a_straight_history_stays_on_one_column() {
        let commits = vec![commit("c", &["b"]), commit("b", &["a"]), commit("a", &[])];
        let rows = layout(&commits);
        assert!(rows.iter().all(|r| r.column == 0));
        assert!(rows.iter().all(|r| r.through.is_empty()));
        assert!(rows.iter().all(|r| r.outgoing.is_empty()));
        assert_eq!(width(&rows), 1);
    }

    #[test]
    fn a_merge_opens_a_second_column_and_closes_it() {
        //   m      merge of f into main
        //   |\
        //   | f    the branch
        //   |/
        //   b      the common base
        let commits = vec![
            commit("m", &["b2", "f"]),
            commit("b2", &["b"]),
            commit("f", &["b"]),
            commit("b", &[]),
        ];
        let rows = layout(&commits);

        // The merge sits on the main column and sends a line towards the second
        // parent's column.
        assert_eq!(rows[0].column, 0);
        assert_eq!(rows[0].outgoing, vec![1]);

        // The branch lives on the column opened for it.
        assert_eq!(rows[2].column, 1);
        // Meanwhile, the main column carries on.
        assert!(rows[2].through.contains(&0));

        // The common base closes the second column: both lanes were waiting for
        // it, only the first carries the bullet.
        assert_eq!(rows[3].column, 0);
        assert_eq!(rows[3].incoming, vec![1]);

        assert_eq!(width(&rows), 2);
        assert_eq!(rows.len(), commits.len(), "one row per commit");
    }

    #[test]
    fn parallel_branches_reuse_freed_columns() {
        // Two independent tips then their base: the column freed by the first
        // must be reused rather than opening a third.
        let commits = vec![
            commit("x", &["base"]),
            commit("y", &["base"]),
            commit("base", &[]),
            commit("z", &[]),
        ];
        let rows = layout(&commits);
        assert_eq!(rows[0].column, 0);
        assert_eq!(rows[1].column, 1);
        assert_eq!(rows[2].column, 0);
        assert_eq!(rows[2].incoming, vec![1], "both lanes converge");
        assert_eq!(rows[3].column, 0);
        // `z` is awaited by nobody: it takes column 0 back, now free.
        assert_eq!(width(&rows), 2);
    }

    #[test]
    fn an_octopus_merge_places_every_parent() {
        let commits = vec![commit("o", &["p1", "p2", "p3"])];
        let rows = layout(&commits);
        assert_eq!(rows[0].column, 0);
        assert_eq!(rows[0].outgoing, vec![1, 2]);
        assert_eq!(width(&rows), 3);
    }

    /// Reads a real repository's history — this one's — and checks that every
    /// field arrives filled in. The format and its separator are exactly the
    /// kind of thing that works on a hand-written example and slips by one
    /// field on the real output.
    #[test]
    fn reads_this_repository() {
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
        let Ok(commits) = commits(dir, &LogRange::Head, 5) else {
            return; // no git repository at build time: nothing to check
        };
        assert!(!commits.is_empty(), "this repository has commits");

        for commit in &commits {
            assert_eq!(commit.id.len(), 40, "full digest expected");
            assert!(!commit.short.is_empty());
            assert!(
                !commit.summary.is_empty(),
                "the subject of commit {} is empty — the format has slipped by one field",
                commit.short
            );
            assert!(!commit.author.is_empty(), "missing author");
            assert!(!commit.date.is_empty(), "missing date");
            // The subject must not have swallowed the following fields.
            assert!(
                !commit.summary.contains('\u{1f}'),
                "the separator leaked into the subject: {}",
                commit.summary
            );
        }

        let rows = layout(&commits);
        assert_eq!(rows.len(), commits.len());
    }

    #[test]
    fn ranges_use_the_right_revision_syntax() {
        assert_eq!(LogRange::Head.args(), vec!["HEAD", "--"]);
        // A named ref is passed as it stands: `origin/main` is a revision, and
        // dressing it up as one is what would break it.
        assert_eq!(
            LogRange::Ref {
                name: "origin/main".into()
            }
            .args(),
            vec!["origin/main", "--"]
        );
        assert_eq!(
            LogRange::Branch {
                base: "main".into()
            }
            .args(),
            vec!["main..HEAD", "--"]
        );
        // Topological order is what keeps the branches grouped.
        assert!(LogRange::All.args().contains(&"--topo-order".to_string()));
        // And every one of them ends the revision list: a branch named like a
        // directory is refused outright otherwise.
        for range in [
            LogRange::Head,
            LogRange::All,
            LogRange::Ref { name: "dev".into() },
        ] {
            assert_eq!(range.args().last().map(String::as_str), Some("--"));
        }
        assert_eq!(
            LogRange::Lines {
                path: "src/ui/app.rs".into(),
                start: 12,
                end: 20,
            }
            .args(),
            vec!["-L", "12,20:src/ui/app.rs", "--"]
        );
    }

    #[test]
    fn a_line_history_reads_its_commits_and_their_patches() {
        // What `git log -L` writes: the format, then the patch, one record per
        // commit, each opened by the `\x01` the format starts with.
        let out =
            "\u{1}aaa\u{1f}aaa1234\u{1f}bbb\u{1f}Ada\u{1f}2 days ago\u{1f}HEAD -> main\u{1f}Second
diff --git a/f.rs b/f.rs
index 1..2 100644
--- a/f.rs
+++ b/f.rs
@@ -12,3 +12,3 @@
 kept
-was
+is
\u{1}bbb\u{1f}bbb5678\u{1f}\u{1f}Bo\u{1f}3 weeks ago\u{1f}\u{1f}First
diff --git a/f.rs b/f.rs
--- /dev/null
+++ b/f.rs
@@ -0,0 +1,2 @@
+kept
+was
";
        let found = parse_line_history(out);
        assert_eq!(found.len(), 2);

        let (commit, patch) = &found[0];
        assert_eq!(commit.short, "aaa1234");
        assert_eq!(commit.summary, "Second");
        assert_eq!(commit.parents, vec!["bbb"]);
        // The arrow is dropped, the branch kept.
        assert_eq!(commit.refs, vec!["main"]);
        assert_eq!(patch.hunks.len(), 1);
        // The patch is the one `-L` restricted, and it is numbered from the
        // file it belongs to: line 12, not line 1.
        assert_eq!(patch.hunks[0].old_start, 12);
        assert_eq!(patch.hunks[0].lines.len(), 3);

        // A root commit: no parent, and the patch git writes against nothing.
        let (commit, patch) = &found[1];
        assert!(commit.parents.is_empty());
        assert!(commit.refs.is_empty());
        assert_eq!(patch.hunks.len(), 1);
        assert!(!patch.empty);
    }

    #[test]
    fn a_line_history_of_nothing_is_an_empty_list() {
        assert!(parse_line_history("").is_empty());
        assert!(parse_line_history("\n").is_empty());
    }
}
