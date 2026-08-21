//! The history, and the graph that makes it readable.
//!
//! A git history is a directed acyclic graph, not a list: two parallel
//! branches, a merge, and chronological order alone no longer says anything
//! about what descends from what. The graph is therefore computed here, in the
//! shape the view needs — one column per row and the lines that join them — and
//! not delegated to `git log --graph`, whose output is a drawing in characters
//! that would have to be re-parsed back into coordinates.

use std::path::Path;

use anyhow::Result;

use super::git;

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
    /// What the branch has added since it diverged from `base` — the same range
    /// as the branch review, seen as a sequence of commits.
    Branch { base: String },
    /// Every reference: this is where the graph earns its keep, parallel
    /// branches being visible side by side.
    All,
}

impl LogRange {
    fn args(&self) -> Vec<String> {
        match self {
            Self::Head => vec!["HEAD".into()],
            Self::Branch { base } => vec![format!("{base}..HEAD")],
            // `--all` without `--topo-order` would interleave the branches by
            // date, which gives an unreadable graph: the lines would jump from
            // one branch to another on every row.
            Self::All => vec!["--all".into(), "--topo-order".into()],
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
    args.extend(range.args());
    let out = git(dir, &args)?;
    Ok(parse(&out))
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
    let mut lanes: Vec<Option<String>> = Vec::new();
    let mut rows = Vec::with_capacity(commits.len());

    for commit in commits {
        // Every lane that was waiting for this commit: several children may be
        // waiting, and they all converge on the same bullet.
        let waiting: Vec<usize> = lanes
            .iter()
            .enumerate()
            .filter(|(_, lane)| lane.as_deref() == Some(commit.id.as_str()))
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
        lanes[column] = commit.parents.first().cloned();

        let mut outgoing = Vec::new();
        for parent in commit.parents.iter().skip(1) {
            // A parent already awaited elsewhere does not deserve another lane:
            // the line joins the one that exists.
            let target = lanes
                .iter()
                .position(|lane| lane.as_deref() == Some(parent.as_str()))
                .unwrap_or_else(|| {
                    let ix = free_lane(&mut lanes);
                    lanes[ix] = Some(parent.clone());
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
fn free_lane(lanes: &mut Vec<Option<String>>) -> usize {
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
        assert_eq!(LogRange::Head.args(), vec!["HEAD"]);
        assert_eq!(
            LogRange::Branch {
                base: "main".into()
            }
            .args(),
            vec!["main..HEAD"]
        );
        // Topological order is what keeps the branches grouped.
        assert!(LogRange::All.args().contains(&"--topo-order".to_string()));
    }
}
