//! What `gh` answers, and what is decided on it — pure, tested on fixtures.
//!
//! The panel that paints it is `ui::github`; this is the half that knows no
//! gpui, on the model of `sentry.rs`: the commands, the shapes they come back
//! in, and the few decisions taken on them — which runs are still going, which
//! jobs open by default, what an agent is handed, how a pull request becomes a
//! checkout. Everything here is a function of text, which is what lets it be
//! tested against the answers `gh` actually gave.
//!
//! **Two shapes of reading, and the difference is not a taste.** A list of
//! runs is flat and comes back as tab-separated lines, formatted by
//! `gh --template`: a format that cannot drift. A run's jobs, a pull request's
//! review threads, are trees, and a Go template folding them into lines would
//! be a parser written inside a string — they are read as JSON, **field by
//! field** (`serde_json::Value`), never deserialised into a struct: the
//! GraphQL API writes `null` for a thread whose line has left the diff and for
//! a comment whose author deleted their account, and one `null` in a struct's
//! `String` fails the whole answer. It is Sentry's lesson, paid for there.

use std::path::{Path, PathBuf};

use serde_json::Value;

/// How many runs the branch list asks for. Twenty is a branch's recent
/// history: past that one is reading the project's, which is what the web
/// page is for.
const RUNS: usize = 20;

/// How many runs each of the live reads asks for. A repository with more than
/// this many running at once is a repository whose web page is the right tool.
const LIVE: usize = 30;

/// The fields a run line carries, in order. Written once because it is read
/// once: [`parse_runs`] takes them back by position.
///
/// One trap is written into it and it is not a refinement — `printf "%.0f"` on
/// the id, because a Go template formats a JSON number as a float, so
/// `{{.databaseId}}` gives `3.2494024323e+10`, an id `gh run view` does not
/// recognise, and nothing says so before the first click.
const TEMPLATE: &str = "--json databaseId,displayTitle,workflowName,status,conclusion,headBranch,event,startedAt,url \
     --template '{{range .}}{{printf \"%.0f\" .databaseId}}{{\"\\t\"}}{{.displayTitle}}{{\"\\t\"}}\
{{.workflowName}}{{\"\\t\"}}{{.status}}{{\"\\t\"}}{{.conclusion}}{{\"\\t\"}}{{.headBranch}}{{\"\\t\"}}\
{{.event}}{{\"\\t\"}}{{.startedAt}}{{\"\\t\"}}{{.url}}{{\"\\n\"}}{{end}}'";

// — The runs ————————————————————————————————————————————————————————

/// One run of GitHub Actions.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Run {
    pub id: String,
    pub title: String,
    pub workflow: String,
    /// `completed`, `in_progress`, `queued`, `waiting`…
    pub status: String,
    /// `success`, `failure`, empty while it runs.
    pub conclusion: String,
    pub branch: String,
    /// `push`, `pull_request`, `schedule`…
    pub event: String,
    /// RFC 3339, UTC, as `gh` writes it.
    pub started_at: String,
    pub url: String,
}

impl Run {
    /// What the row says on its right: the conclusion once there is one, the
    /// status until then.
    pub fn tally(&self) -> &str {
        if self.conclusion.is_empty() {
            &self.status
        } else {
            &self.conclusion
        }
    }

    /// The glyph. A run in flight has its own, which is what tells "not yet"
    /// from "not well".
    pub fn glyph(&self) -> &'static str {
        stage_of(&self.status, &self.conclusion).glyph()
    }

    pub fn stage(&self) -> Stage {
        stage_of(&self.status, &self.conclusion)
    }

    /// Whether it is still going — running, queued, waiting for an approval.
    /// What the live view lists, and what keeps it refreshing.
    pub fn is_active(&self) -> bool {
        !self.status.is_empty() && self.status != "completed"
    }

    /// Whether "re-run the failed jobs" means something: over, and not well.
    /// `gh` refuses it on a run in flight and on one that passed, and a button
    /// one can foresee being refused is a button one should not be shown.
    pub fn can_rerun(&self) -> bool {
        !self.is_active() && self.stage() == Stage::Failed
    }

    /// Whether the filter's word is in what the row shows.
    pub fn matches(&self, needle: &str) -> bool {
        let needle = needle.trim().to_lowercase();
        needle.is_empty()
            || self.title.to_lowercase().contains(&needle)
            || self.workflow.to_lowercase().contains(&needle)
            || self.branch.to_lowercase().contains(&needle)
    }
}

/// The command that lists a branch's runs.
///
/// **Filtered by branch**, which is what the panel says it shows: a bare
/// `gh run list` answers with the repository's last twenty runs, so a branch
/// whose own run was pushed out by somebody else's had none on screen and
/// nothing said why. A detached HEAD has no branch to filter on and gets the
/// repository's, which is better than an empty list.
pub fn list_command(branch: Option<&str>) -> String {
    let filter = branch
        .map(|branch| format!(" --branch {}", quote(branch)))
        .unwrap_or_default();
    format!("gh run list{filter} --limit {RUNS} {TEMPLATE}")
}

/// The command that lists what the **repository** has going.
///
/// Two reads and not one list filtered here: `--status` takes one value, and
/// the repository's last thirty runs are, on a busy project, thirty runs that
/// finished — a long job started an hour ago would not be among them. Queued
/// comes second so that a run moving between the two reads is found at least
/// once; [`parse_runs`] drops the second sighting.
pub fn live_command() -> String {
    format!(
        "gh run list --status in_progress --limit {LIVE} {TEMPLATE} && \
         gh run list --status queued --limit {LIVE} {TEMPLATE}"
    )
}

/// The command that reads one run's failing log.
pub fn log_command(id: &str) -> String {
    format!("gh run view {} --log-failed", quote(id))
}

/// The command that reads one run's jobs and their steps.
pub fn jobs_command(id: &str) -> String {
    format!("gh run view {} --json status,conclusion,jobs", quote(id))
}

/// The command that cancels a run.
pub fn cancel_command(id: &str) -> String {
    format!("gh run cancel {}", quote(id))
}

/// The command that re-runs the jobs of a run that failed — and only those,
/// with what they depend on: re-running the jobs that passed is paying for
/// them twice.
pub fn rerun_command(id: &str) -> String {
    format!("gh run rerun {} --failed", quote(id))
}

/// One line per run, fields apart by a tab — it is `gh` that formats.
///
/// A short line is skipped rather than failing the read: `gh` writes its own
/// notices to stdout on occasion, and one of them must not empty the list.
/// Five fields are enough — the four at the end are what the live view adds,
/// and a line without them is still a run. A run seen twice, which the live
/// command's two reads can do, is kept the first time.
pub fn parse_runs(out: &str) -> Vec<Run> {
    let mut seen = std::collections::HashSet::new();
    out.lines()
        .filter_map(|line| {
            let fields: Vec<&str> = line.split('\t').collect();
            if fields.len() < 5 {
                return None;
            }
            let field = |rank: usize| fields.get(rank).copied().unwrap_or("").to_string();
            Some(Run {
                id: field(0),
                title: field(1),
                workflow: field(2),
                status: field(3),
                conclusion: field(4),
                branch: field(5),
                event: field(6),
                started_at: field(7),
                url: field(8),
            })
        })
        .filter(|run| seen.insert(run.id.clone()))
        .collect()
}

/// The end of a log, which is the part the failure is written in.
pub fn tail(text: &str, lines: usize) -> String {
    let all: Vec<&str> = text.lines().collect();
    let start = all.len().saturating_sub(lines);
    all[start..].join("\n")
}

// — What a run, a job, a step amounts to ——————————————————————————————

/// The state of a run, a job or a step, once status and conclusion are read
/// as one thing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Stage {
    Passed,
    Failed,
    Running,
    /// Queued, waiting for a runner or an approval: not started.
    Waiting,
    /// Skipped or neutral — over, and nothing to say.
    Skipped,
}

impl Stage {
    pub fn glyph(self) -> &'static str {
        match self {
            Self::Passed => "circle-check",
            Self::Failed => "circle-x",
            Self::Running => "loader-circle",
            Self::Waiting => "clock",
            Self::Skipped => "circle-dashed",
        }
    }
}

/// Reads a status and a conclusion, in either case — the REST API writes them
/// in lower case, the GraphQL one in capitals.
pub fn stage_of(status: &str, conclusion: &str) -> Stage {
    let status = status.to_ascii_lowercase();
    match status.as_str() {
        "in_progress" => return Stage::Running,
        "queued" | "waiting" | "pending" | "requested" => return Stage::Waiting,
        _ => {}
    }
    match conclusion.to_ascii_lowercase().as_str() {
        "success" => Stage::Passed,
        // **Skipped is not a failure.** A job whose `if` said no is not
        // something that went wrong, and painting a cross on it paints one on
        // a run where nothing is.
        "skipped" | "neutral" => Stage::Skipped,
        // Completed without a conclusion yet — GitHub writes the status first.
        "" if status == "completed" => Stage::Running,
        "" => Stage::Waiting,
        _ => Stage::Failed,
    }
}

/// One step of a job.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Step {
    pub number: u64,
    pub name: String,
    pub status: String,
    pub conclusion: String,
    pub started_at: String,
    pub completed_at: String,
}

/// One job of a run, with its steps.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Job {
    pub id: u64,
    pub name: String,
    pub status: String,
    pub conclusion: String,
    pub started_at: String,
    pub completed_at: String,
    pub url: String,
    pub steps: Vec<Step>,
}

impl Job {
    pub fn stage(&self) -> Stage {
        stage_of(&self.status, &self.conclusion)
    }

    /// Whether its steps show without being asked for: the job running, which
    /// is what one watches, and the job that failed, which is what one came
    /// for. Twelve jobs of twenty steps each, all open, is a list nobody reads.
    pub fn opens_by_default(&self) -> bool {
        matches!(self.stage(), Stage::Running | Stage::Failed)
    }
}

impl Step {
    pub fn stage(&self) -> Stage {
        stage_of(&self.status, &self.conclusion)
    }
}

/// The run's own status and conclusion, which the jobs' read carries too.
///
/// It is what keeps an unfolded run true once it has finished: the live list
/// is the runs in flight, so a run that ends **leaves it** — the moment one was
/// watching for — and without this its detail would say "in progress" over
/// jobs that all say otherwise.
pub fn run_state(out: &str) -> Option<(String, String)> {
    let root: Value = serde_json::from_str(out.trim()).ok()?;
    let status = root.get("status").and_then(Value::as_str)?;
    Some((status.to_string(), text(&root, "conclusion")))
}

/// The jobs `gh run view --json jobs` answers with.
pub fn parse_jobs(out: &str) -> Result<Vec<Job>, String> {
    let root: Value = serde_json::from_str(out.trim()).map_err(|why| why.to_string())?;
    let jobs = root
        .get("jobs")
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or_default();
    Ok(jobs
        .iter()
        .map(|job| Job {
            id: job.get("databaseId").and_then(Value::as_u64).unwrap_or(0),
            name: text(job, "name"),
            status: text(job, "status"),
            conclusion: text(job, "conclusion"),
            started_at: text(job, "startedAt"),
            completed_at: text(job, "completedAt"),
            url: text(job, "url"),
            steps: job
                .get("steps")
                .and_then(Value::as_array)
                .map(Vec::as_slice)
                .unwrap_or_default()
                .iter()
                .map(|step| Step {
                    number: step.get("number").and_then(Value::as_u64).unwrap_or(0),
                    name: text(step, "name"),
                    status: text(step, "status"),
                    conclusion: text(step, "conclusion"),
                    started_at: text(step, "startedAt"),
                    completed_at: text(step, "completedAt"),
                })
                .collect(),
        })
        .collect())
}

/// How long something has taken — or has been taking, while it runs.
///
/// `ended` is read only when the thing is over: `gh` writes
/// `0001-01-01T00:00:00Z` as the end of a job in flight, which is a valid date
/// two thousand years ago and would give a negative duration. A start that does
/// not read gives nothing, never a wrong figure.
pub fn duration(started: &str, ended: Option<&str>, now: i64) -> Option<String> {
    let start = crate::sentry::instant_of(started)?;
    let end = match ended {
        Some(ended) => crate::sentry::instant_of(ended)?,
        None => now,
    };
    (end >= start).then(|| clock(end - start))
}

/// A duration as a stopwatch writes it: the two largest units, nothing more.
pub fn clock(seconds: i64) -> String {
    let (h, m, s) = (seconds / 3600, seconds / 60 % 60, seconds % 60);
    if h > 0 {
        format!("{h}h {m:02}m")
    } else if m > 0 {
        format!("{m}m {s:02}s")
    } else {
        format!("{s}s")
    }
}

/// When a job's or a step's duration is taken: its own end once it has one.
pub fn end_of<'a>(status: &str, completed_at: &'a str) -> Option<&'a str> {
    status
        .eq_ignore_ascii_case("completed")
        .then_some(completed_at)
}

/// Whether anything shown is still going — what keeps the live view reading.
///
/// A job in flight counts even when its run is not in the list: the run
/// unfolded may be one of the branch's, and its steps are what one watches.
pub fn still_going(runs: &[Run], jobs: &[Job]) -> bool {
    runs.iter().any(Run::is_active)
        || jobs
            .iter()
            .any(|job| matches!(job.stage(), Stage::Running | Stage::Waiting))
}

// — A pull request's review threads ————————————————————————————————

/// One comment of a thread.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Comment {
    pub author: String,
    pub body: String,
    pub url: String,
}

/// One review thread: a place in the diff, and what was said about it.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Thread {
    pub path: String,
    /// The line in the current diff; `None` once the line has left it.
    pub line: Option<u64>,
    /// The line the thread was opened on, which is what remains of an outdated
    /// thread's place.
    pub original_line: Option<u64>,
    pub resolved: bool,
    pub outdated: bool,
    pub comments: Vec<Comment>,
}

/// The command that reads a pull request's review threads.
///
/// **GraphQL and not `gh pr view --json reviews,comments`**: those are the
/// reviews' bodies and the conversation, with neither file nor line, and
/// nothing that says whether a thread was resolved — which is the one thing
/// that decides what an agent is handed. `{owner}` and `{repo}` are filled in
/// by `gh` from the repository of the directory it runs in; `$owner` is
/// GraphQL's, inside single quotes, so the shell leaves it alone.
pub fn threads_command(number: u64) -> String {
    format!(
        "gh api graphql -F owner='{{owner}}' -F name='{{repo}}' -F number={number} -f query='\
         query($owner:String!,$name:String!,$number:Int!){{repository(owner:$owner,name:$name)\
         {{pullRequest(number:$number){{reviewThreads(first:100){{nodes{{isResolved isOutdated \
         path line originalLine comments(first:50){{nodes{{author{{login}} body url}}}}}}}}}}}}}}'"
    )
}

/// The threads the answer carries.
pub fn parse_threads(out: &str) -> Result<Vec<Thread>, String> {
    let root: Value = serde_json::from_str(out.trim()).map_err(|why| why.to_string())?;
    // An error of GraphQL's comes back with a `data` whose pull request is
    // null — "no thread" would be the wrong thing to say.
    if let Some(message) = root
        .get("errors")
        .and_then(Value::as_array)
        .and_then(|errors| errors.first())
        .and_then(|error| error.get("message"))
        .and_then(Value::as_str)
    {
        return Err(message.to_string());
    }
    let nodes = root
        .pointer("/data/repository/pullRequest/reviewThreads/nodes")
        .and_then(Value::as_array)
        .ok_or_else(|| "no review threads in the answer".to_string())?;
    Ok(nodes
        .iter()
        .map(|thread| Thread {
            path: text(thread, "path"),
            line: thread.get("line").and_then(Value::as_u64),
            original_line: thread.get("originalLine").and_then(Value::as_u64),
            resolved: thread
                .get("isResolved")
                .and_then(Value::as_bool)
                .unwrap_or(false),
            outdated: thread
                .get("isOutdated")
                .and_then(Value::as_bool)
                .unwrap_or(false),
            comments: thread
                .pointer("/comments/nodes")
                .and_then(Value::as_array)
                .map(Vec::as_slice)
                .unwrap_or_default()
                .iter()
                .map(|comment| Comment {
                    // A deleted account answers `null`: GitHub shows "ghost".
                    author: comment
                        .pointer("/author/login")
                        .and_then(Value::as_str)
                        .unwrap_or("ghost")
                        .to_string(),
                    body: text(comment, "body"),
                    url: text(comment, "url"),
                })
                .collect(),
        })
        .collect())
}

/// The threads still open, in the order one reads a diff: by file, then line.
pub fn unresolved(threads: &[Thread]) -> Vec<&Thread> {
    let mut open: Vec<&Thread> = threads.iter().filter(|thread| !thread.resolved).collect();
    open.sort_by(|a, b| {
        (a.path.as_str(), a.line.or(a.original_line))
            .cmp(&(b.path.as_str(), b.line.or(b.original_line)))
    });
    open
}

/// The prompt handed to the agent of the pull request's worktree: every
/// unresolved thread, where it is, who said what.
///
/// The introduction arrives already translated from the view: `tr!` belongs to
/// the `ui` feature, and this module compiles in the headless server. The rest
/// is structure, in English like the Sentry prompt's — the agent reads it, and
/// the comments it quotes are in whatever language the reviewers wrote.
///
/// **An outdated thread is said to be one**: its line is where the code *was*,
/// and an agent sent there without the warning edits the wrong place.
pub fn threads_prompt(
    intro: &str,
    number: u64,
    title: &str,
    url: &str,
    threads: &[Thread],
) -> String {
    let mut out = String::new();
    out.push_str(intro);
    out.push_str(&format!("\n\nPull request #{number}: {title}\n{url}\n"));
    for thread in unresolved(threads) {
        out.push('\n');
        let place = match (thread.line, thread.original_line) {
            (Some(line), _) => format!("{}:{line}", thread.path),
            (None, Some(line)) => format!(
                "{}:{line} (outdated — the line has changed since)",
                thread.path
            ),
            (None, None) => thread.path.clone(),
        };
        let place = if thread.outdated && thread.line.is_some() {
            format!("{place} (outdated)")
        } else {
            place
        };
        out.push_str(&format!("## {place}\n"));
        for comment in &thread.comments {
            // Every line of the body under its author, indented: a comment of
            // three paragraphs must not read as three comments.
            let mut lines = comment.body.trim().lines();
            let first = lines.next().unwrap_or("");
            out.push_str(&format!("- {}: {first}\n", comment.author));
            for line in lines {
                out.push_str(&format!("  {line}\n"));
            }
        }
        if let Some(link) = thread.comments.first().map(|comment| &comment.url) {
            if !link.is_empty() {
                out.push_str(&format!("{link}\n"));
            }
        }
    }
    out
}

// — A pull request, checked out ————————————————————————————————————

/// What a pull request says about where its commits are.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PrHead {
    pub number: u64,
    pub head: String,
    pub base: String,
    /// Its branch lives in a fork: `origin` has no branch of that name, only
    /// `refs/pull/<n>/head`.
    pub cross: bool,
}

impl PrHead {
    /// The local branch the pull request is checked out on.
    ///
    /// Its own name when it is the repository's. **`pr/<n>` for a fork's**:
    /// the fork's `main` or `fix` would take a name this repository already
    /// uses, and the number is what names a pull request everywhere else.
    pub fn local_branch(&self) -> String {
        if self.cross {
            format!("pr/{}", self.number)
        } else {
            self.head.clone()
        }
    }

    /// The base the review compares against: the remote's, which the fetch
    /// has just brought up to date — the local one may be weeks behind, and a
    /// review against it shows everything merged since as the pull request's.
    pub fn review_base(&self) -> String {
        format!("origin/{}", self.base)
    }

    /// The command that brings the pull request's commits here.
    ///
    /// In the **main** checkout and never through `gh pr checkout`, which
    /// would switch that checkout's branch. The refspecs are explicit and
    /// forced: a pull request is force-pushed as a matter of course, and a
    /// fork's `pr/<n>` must follow it. The base comes too, for
    /// [`Self::review_base`]. `GIT_TERMINAL_PROMPT=0` for the reason every git
    /// command of this program carries it: a password asked on a closed stdin
    /// holds the worker until the timeout.
    pub fn fetch_command(&self) -> String {
        let head = if self.cross {
            format!(
                "+refs/pull/{}/head:refs/heads/pr/{}",
                self.number, self.number
            )
        } else {
            format!("+refs/heads/{0}:refs/remotes/origin/{0}", self.head)
        };
        let base = format!("+refs/heads/{0}:refs/remotes/origin/{0}", self.base);
        format!(
            "GIT_TERMINAL_PROMPT=0 git fetch origin {} {}",
            quote(&head),
            quote(&base)
        )
    }

    /// The branch the creation is asked for, once fetched.
    ///
    /// A fork's is local already — the fetch wrote it. The repository's own
    /// is its local branch when there is one, `origin/<head>` otherwise, which
    /// the creation turns into a local branch of the short name.
    pub fn branch_to_create(&self, local_branches: &[&str]) -> String {
        let local = self.local_branch();
        if self.cross || local_branches.contains(&local.as_str()) {
            local
        } else {
            format!("origin/{}", self.head)
        }
    }
}

/// A checkout and the branch it holds — `None` on a detached HEAD.
pub type Checkout = (PathBuf, Option<String>);

/// The worktree that already has the pull request checked out, if one does.
///
/// Git refuses two checkouts of one branch, so there is at most one — and
/// going there is the whole of the gesture.
pub fn worktree_of<'a>(pr: &PrHead, worktrees: &'a [Checkout]) -> Option<&'a Path> {
    let local = pr.local_branch();
    worktrees
        .iter()
        .find(|(_, branch)| branch.as_deref() == Some(local.as_str()))
        .map(|(path, _)| path.as_path())
}

// — Plumbing ————————————————————————————————————————————————————————

/// A string field, empty when it is absent **or null**.
fn text(value: &Value, key: &str) -> String {
    value
        .get(key)
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string()
}

/// Single-quotes a value for `sh -c`.
///
/// A branch name, a pull request's body, go out on a command line: one
/// apostrophe in them, and the rest of the command is read as something else.
pub fn quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', r"'\''"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_short_line_is_skipped_rather_than_failing_the_read() {
        let out = "42\tFix the thing\tCI\tcompleted\tsuccess\n\
                   oops, a notice\n\
                   43\tAnother\tRelease\tin_progress\t\n";
        let runs = parse_runs(out);
        assert_eq!(runs.len(), 2);
        assert_eq!(runs[0].id, "42");
        assert_eq!(runs[0].tally(), "success");
        assert_eq!(runs[0].glyph(), "circle-check");
        // In flight: the status stands in, and the glyph says "not yet" rather
        // than "not well".
        assert_eq!(runs[1].tally(), "in_progress");
        assert_eq!(runs[1].glyph(), "loader-circle");
        assert!(runs[1].is_active());
        assert!(!runs[0].is_active());
    }

    /// The line `gh` writes with the template, taken from a real answer.
    #[test]
    fn a_full_line_carries_the_branch_the_event_and_the_start() {
        let out = "35772030709\trustdoc: fix ICE\tCI\tin_progress\t\tice-intra-doc\tpull_request\t\
                   2026-09-22T19:10:13Z\thttps://github.com/rust-lang/rust/actions/runs/35772030709\n";
        let run = parse_runs(out).pop().expect("a run");
        assert_eq!(run.branch, "ice-intra-doc");
        assert_eq!(run.event, "pull_request");
        assert_eq!(run.started_at, "2026-09-22T19:10:13Z");
        assert!(run.url.ends_with("/35772030709"));
        assert!(run.matches("ice-intra"));
    }

    /// A run that moves from queued to running between the live command's two
    /// reads is listed once.
    #[test]
    fn a_run_seen_twice_is_listed_once() {
        let out = "7\tt\tw\tin_progress\t\n8\tu\tw\tqueued\t\n7\tt\tw\tqueued\t\n";
        let runs = parse_runs(out);
        assert_eq!(runs.len(), 2);
        assert_eq!(runs[0].status, "in_progress");
    }

    #[test]
    fn a_failure_draws_the_cross_and_offers_a_rerun() {
        let runs = parse_runs("7\tt\tw\tcompleted\tfailure\n8\tt\tw\tcompleted\tsuccess\n");
        assert_eq!(runs[0].glyph(), "circle-x");
        assert!(runs[0].can_rerun());
        assert!(!runs[1].can_rerun());
    }

    #[test]
    fn the_tail_is_the_end_and_a_short_log_is_itself() {
        assert_eq!(tail("a\nb\nc", 2), "b\nc");
        assert_eq!(tail("a\nb", 5), "a\nb");
        assert_eq!(tail("", 5), "");
    }

    /// The trap that costs a click: a Go template writes a JSON number as a
    /// float, and `3.2494024323e+10` is an id `gh run view` does not know.
    #[test]
    fn the_listing_asks_for_the_id_as_an_integer() {
        for command in [list_command(None), live_command()] {
            assert!(
                command.contains(r#"printf "%.0f" .databaseId"#),
                "{command}"
            );
        }
        assert!(log_command("42").contains("--log-failed"));
    }

    /// The panel says it shows the branch's runs; a bare `gh run list` shows
    /// the repository's.
    #[test]
    fn the_listing_is_filtered_by_branch_when_there_is_one() {
        assert!(list_command(Some("wt/thing")).contains("--branch 'wt/thing'"));
        assert!(!list_command(None).contains("--branch"));
    }

    /// What the repository has going is two reads, and neither is filtered
    /// by branch.
    #[test]
    fn the_live_read_asks_for_running_and_queued() {
        let command = live_command();
        assert!(command.contains("--status in_progress"), "{command}");
        assert!(command.contains("--status queued"), "{command}");
        assert!(!command.contains("--branch"), "{command}");
    }

    #[test]
    fn the_gestures_on_a_run_name_it() {
        assert_eq!(cancel_command("42"), "gh run cancel '42'");
        assert_eq!(rerun_command("42"), "gh run rerun '42' --failed");
        assert_eq!(
            jobs_command("42"),
            "gh run view '42' --json status,conclusion,jobs"
        );
    }

    #[test]
    fn the_filter_reads_the_title_the_workflow_and_the_branch() {
        let runs = parse_runs("42\tFix the thing\tCI\tcompleted\tsuccess\twt/x\n");
        assert!(runs[0].matches("thing"));
        assert!(runs[0].matches("ci"));
        assert!(runs[0].matches("wt/x"));
        assert!(!runs[0].matches("42"));
        assert!(runs[0].matches(" "));
    }

    /// Cut down from `gh run view --json jobs` on a run in flight: a job
    /// running, whose end is the year one, one done, one failed.
    const JOBS: &str = r#"{"status":"in_progress","conclusion":"","jobs":[
        {"completedAt":"2026-09-22T19:06:23Z","conclusion":"success","databaseId":106894099483,
         "name":"Calculate job matrix","startedAt":"2026-09-22T19:05:35Z","status":"completed",
         "url":"https://github.com/rust-lang/rust/actions/runs/1/job/106894099483",
         "steps":[
           {"completedAt":"2026-09-22T19:05:36Z","conclusion":"success","name":"Set up job","number":1,"startedAt":"2026-09-22T19:05:36Z","status":"completed"},
           {"completedAt":"2026-09-22T19:05:49Z","conclusion":"skipped","name":"Test citool","number":3,"startedAt":"2026-09-22T19:05:49Z","status":"completed"}]},
        {"completedAt":"0001-01-01T00:00:00Z","conclusion":"","databaseId":106894432692,
         "name":"PR - test-aarch64","startedAt":"2026-09-22T19:06:28Z","status":"in_progress",
         "url":"https://github.com/rust-lang/rust/actions/runs/1/job/106894432692",
         "steps":[
           {"completedAt":"2026-09-22T19:06:29Z","conclusion":"success","name":"Set up job","number":1,"startedAt":"2026-09-22T19:06:29Z","status":"completed"},
           {"completedAt":null,"conclusion":null,"name":"Run build","number":2,"startedAt":"2026-09-22T19:06:44Z","status":"in_progress"},
           {"completedAt":null,"conclusion":null,"name":"Upload","number":3,"startedAt":null,"status":"pending"}]},
        {"completedAt":"2026-09-22T19:10:55Z","conclusion":"failure","databaseId":106894432721,
         "name":"PR - test-aarch64-2","startedAt":"2026-09-22T19:06:28Z","status":"completed","url":"u","steps":[]}
    ]}"#;

    #[test]
    fn the_jobs_are_read_off_the_answer_nulls_and_all() {
        let jobs = parse_jobs(JOBS).expect("the fixture reads");
        assert_eq!(jobs.len(), 3);
        assert_eq!(jobs[0].id, 106894099483);
        assert_eq!(jobs[0].stage(), Stage::Passed);
        assert_eq!(jobs[0].steps[1].stage(), Stage::Skipped);
        assert_eq!(jobs[1].stage(), Stage::Running);
        // A null conclusion is an empty one, not a broken answer.
        assert_eq!(jobs[1].steps[1].stage(), Stage::Running);
        assert_eq!(jobs[1].steps[2].stage(), Stage::Waiting);
        assert_eq!(jobs[2].stage(), Stage::Failed);
        assert!(parse_jobs("not json").is_err());
        assert!(parse_jobs("{}").expect("no jobs is an answer").is_empty());
    }

    /// The same read says where the run itself is — what keeps an unfolded run
    /// true once it has left the list of runs in flight.
    #[test]
    fn the_jobs_read_carries_the_runs_own_state() {
        assert_eq!(
            run_state(JOBS),
            Some(("in_progress".to_string(), String::new()))
        );
        assert_eq!(
            run_state(r#"{"status":"completed","conclusion":"failure","jobs":[]}"#),
            Some(("completed".to_string(), "failure".to_string()))
        );
        assert_eq!(run_state(r#"{"jobs":[]}"#), None);
    }

    /// Open by default: what one watches, and what one came for.
    #[test]
    fn a_running_or_failed_job_shows_its_steps() {
        let jobs = parse_jobs(JOBS).unwrap();
        assert!(!jobs[0].opens_by_default());
        assert!(jobs[1].opens_by_default());
        assert!(jobs[2].opens_by_default());
    }

    /// The year one is the end `gh` writes for a job in flight: its duration
    /// is taken up to now, never against that date.
    #[test]
    fn a_job_in_flight_is_timed_up_to_now() {
        let jobs = parse_jobs(JOBS).unwrap();
        let now = crate::sentry::instant_of("2026-09-22T19:08:28Z").unwrap();
        let running = &jobs[1];
        assert_eq!(
            duration(
                &running.started_at,
                end_of(&running.status, &running.completed_at),
                now
            ),
            Some("2m 00s".into())
        );
        let done = &jobs[0];
        assert_eq!(
            duration(
                &done.started_at,
                end_of(&done.status, &done.completed_at),
                now
            ),
            Some("48s".into())
        );
        assert_eq!(duration("", None, now), None);
        assert_eq!(clock(3 * 3600 + 5 * 60 + 9), "3h 05m");
    }

    #[test]
    fn something_still_going_keeps_the_view_reading() {
        let done = parse_runs("1\tt\tw\tcompleted\tsuccess\n");
        let going = parse_runs("2\tt\tw\tqueued\t\n");
        let jobs = parse_jobs(JOBS).unwrap();
        assert!(!still_going(&done, &[]));
        assert!(still_going(&going, &[]));
        // The branch's run is over but the job unfolded under it is not.
        assert!(still_going(&done, &jobs));
        assert!(!still_going(&done, &jobs[2..]));
    }

    /// Cut down from a real answer of the GraphQL API, with the nulls it
    /// writes: an outdated thread's line, a deleted account.
    const THREADS: &str = r#"{"data":{"repository":{"pullRequest":{"reviewThreads":{"nodes":[
        {"isResolved":false,"isOutdated":false,"path":"internal/flock/flock.go","line":32,"originalLine":32,
         "comments":{"nodes":[{"author":{"login":"copilot-pull-request-reviewer"},
           "body":"Lock attempts TryLock before observing ctx.\n\nCheck ctx first.",
           "url":"https://github.com/cli/cli/pull/14450#discussion_r1"},
           {"author":null,"body":"Agreed.","url":"https://github.com/cli/cli/pull/14450#discussion_r2"}]}},
        {"isResolved":true,"isOutdated":false,"path":"api/a.go","line":3,"originalLine":3,
         "comments":{"nodes":[{"author":{"login":"bob"},"body":"Done","url":"u"}]}},
        {"isResolved":false,"isOutdated":true,"path":"api/http_client.go","line":null,"originalLine":19,
         "comments":{"nodes":[{"author":{"login":"alice"},"body":"Keep this public.","url":"https://x/3"}]}}
    ]}}}}}"#;

    #[test]
    fn the_threads_are_read_off_the_answer() {
        let threads = parse_threads(THREADS).expect("the fixture reads");
        assert_eq!(threads.len(), 3);
        assert_eq!(threads[0].comments[1].author, "ghost");
        assert_eq!(threads[2].line, None);
        assert_eq!(threads[2].original_line, Some(19));
        // Unresolved, by file: `api/` before `internal/`.
        let open = unresolved(&threads);
        assert_eq!(open.len(), 2);
        assert_eq!(open[0].path, "api/http_client.go");
    }

    /// GraphQL's refusal is an error, not "no thread".
    #[test]
    fn a_refusal_is_said() {
        let refused = r#"{"data":{"repository":{"pullRequest":null}},
            "errors":[{"type":"NOT_FOUND","message":"Could not resolve to a PullRequest with the number of 14470."}]}"#;
        let why = parse_threads(refused).expect_err("an error");
        assert!(why.contains("14470"), "{why}");
    }

    #[test]
    fn the_prompt_quotes_every_open_thread_where_it_is() {
        let threads = parse_threads(THREADS).unwrap();
        let text = threads_prompt("Address these.", 14450, "Flock", "https://pr", &threads);
        assert!(text.starts_with("Address these.\n\nPull request #14450: Flock\nhttps://pr\n"));
        assert!(text.contains("## internal/flock/flock.go:32\n"), "{text}");
        // The body keeps its paragraphs under its author.
        assert!(text.contains("- copilot-pull-request-reviewer: Lock attempts TryLock before observing ctx.\n  \n  Check ctx first.\n"), "{text}");
        assert!(text.contains("- ghost: Agreed.\n"), "{text}");
        assert!(text.contains("https://github.com/cli/cli/pull/14450#discussion_r1\n"));
        // Outdated: said, with the line it was opened on.
        assert!(
            text.contains("## api/http_client.go:19 (outdated"),
            "{text}"
        );
        // Resolved: not handed over.
        assert!(!text.contains("api/a.go"), "{text}");
    }

    #[test]
    fn the_query_names_the_pull_request_and_leaves_the_placeholders_to_gh() {
        let command = threads_command(86);
        assert!(command.contains("-F number=86"), "{command}");
        assert!(command.contains("-F owner='{owner}'"), "{command}");
        assert!(command.contains("reviewThreads(first:100)"), "{command}");
        assert!(command.contains("isResolved"), "{command}");
    }

    fn pr(cross: bool) -> PrHead {
        PrHead {
            number: 86,
            head: "wt/offer".into(),
            base: "master".into(),
            cross,
        }
    }

    #[test]
    fn a_fork_is_fetched_from_its_pull_ref_into_a_numbered_branch() {
        let fork = pr(true);
        assert_eq!(fork.local_branch(), "pr/86");
        let command = fork.fetch_command();
        assert!(
            command.contains("'+refs/pull/86/head:refs/heads/pr/86'"),
            "{command}"
        );
        assert!(
            command.contains("'+refs/heads/master:refs/remotes/origin/master'"),
            "{command}"
        );
        assert!(command.starts_with("GIT_TERMINAL_PROMPT=0 git fetch origin"));
        assert_eq!(fork.branch_to_create(&[]), "pr/86");
        assert_eq!(fork.review_base(), "origin/master");
    }

    #[test]
    fn the_repositorys_own_branch_keeps_its_name() {
        let own = pr(false);
        assert_eq!(own.local_branch(), "wt/offer");
        assert!(own
            .fetch_command()
            .contains("'+refs/heads/wt/offer:refs/remotes/origin/wt/offer'"));
        // Local already: that one. Not yet: the remote's, which the creation
        // turns into a local branch.
        assert_eq!(own.branch_to_create(&["master", "wt/offer"]), "wt/offer");
        assert_eq!(own.branch_to_create(&["master"]), "origin/wt/offer");
    }

    /// A worktree that has the branch is where the gesture goes.
    #[test]
    fn a_checkout_already_there_is_found() {
        let worktrees = vec![
            (PathBuf::from("/p/site"), Some("master".to_string())),
            (PathBuf::from("/p/site-wt/pr-86"), Some("pr/86".to_string())),
            (PathBuf::from("/p/site-wt/detached"), None),
        ];
        assert_eq!(
            worktree_of(&pr(true), &worktrees),
            Some(Path::new("/p/site-wt/pr-86"))
        );
        assert_eq!(worktree_of(&pr(false), &worktrees), None);
    }
}
