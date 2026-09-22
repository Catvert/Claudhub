//! The repository's pull requests and its GitHub Actions, read through `gh`.
//!
//! **`gh` and not GitHub's API**, which is the whole reason this view is cheap:
//! the CLI is already authenticated — it is a program the user installs, like
//! the agents in the terminal — and Claudhub has no token to hold, no OAuth
//! flow to walk and no host to ask about. What it costs is a process per read,
//! on the background queue, which is the `wt` sweep's profile.
//!
//! What is decided on `gh`'s answers — the run lines, the jobs, the review
//! threads, how a pull request becomes a checkout — lives in `crate::github`,
//! pure and tested; this is the view, and the state it keeps between answers.
//!
//! **The live view reads again while something runs, and only while it is
//! painted.** A timer does not read: it marks the reading due and asks for a
//! frame. The read is sent from `ensure_github`, which only a painted panel
//! calls — so a panel folded away, or a tab behind another, costs nothing
//! however long its runs take, and shows fresh figures the moment it comes
//! back. One read in flight at a time, and an id per send drops the answer
//! that comes back late.

use std::collections::HashMap;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use gpui_kit::component::{
    button::{Button, ButtonVariants as _},
    checkbox::Checkbox,
    h_flex,
    input::{Input, InputState, Textarea, TextareaState},
    v_flex, ActiveTheme, Disableable as _, Selectable as _, Sizable as _, WindowExt as _,
};
use gpui_kit::{div, prelude::*, AnyElement, Context, Entity, Hsla, SharedString, Window};

use crate::github::{
    cancel_command, duration, end_of, jobs_command, list_command, live_command, log_command,
    parse_jobs, parse_runs, parse_threads, quote, rerun_command, run_state, still_going, tail,
    threads_command, threads_prompt, unresolved, worktree_of, Checkout, Job, PrHead, Run, Stage,
};
use crate::runtime::protocol::Caller;
use crate::runtime::Cmd;
use crate::tr;
use crate::ui::app::ClaudhubApp;
use crate::ui::find::Pane;
use crate::ui::icons::icon;

/// How many lines of a log are kept on screen. The **end**, which is where the
/// failure is written.
const TAIL: usize = 80;

/// How many go to the agent — more than one reads, because the agent reads it
/// all and the line that explains is not always the last.
const TAIL_FOR_AGENT: usize = 120;

/// How often the live view reads again while something runs.
///
/// Ten seconds: a step lasts tens of seconds, a job minutes, and what one
/// watches for is the moment it turns red — seen within ten seconds is seen.
/// Faster would be a `gh` process and a round trip to GitHub every few seconds
/// for figures that have not moved.
const LIVE_PERIOD: Duration = Duration::from_secs(10);

/// How long a pull request being opened in a worktree waits for that worktree
/// to appear.
///
/// A creation through `wt` asks its questions and may clone databases for
/// minutes; one that failed never brings the worktree, and the landing must not
/// wait for it forever — a checkout of that branch made by hand an hour later
/// would otherwise be jumped to.
const LANDING_WAIT: Duration = Duration::from_secs(15 * 60);

// — The pull request ————————————————————————————————————————————————

/// The fields of a pull request this panel reads.
///
/// Deserialised straight from `gh --json`, which is why the names are the
/// API's and not ours: a rename here is a `serde(rename)` there, and the pair
/// would drift on the first field added. Every string goes through
/// [`nullable`]: `#[serde(default)]` fills an *absent* field and does nothing
/// for one present as `null`, which fails the whole list.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PullRequest {
    pub number: u64,
    pub title: String,
    pub url: String,
    pub is_draft: bool,
    pub base_ref_name: String,
    pub head_ref_name: String,
    /// Its branch lives in a fork — `origin` has only `refs/pull/<n>/head`.
    #[serde(default)]
    pub is_cross_repository: bool,
    /// `MERGEABLE`, `CONFLICTING`, or `UNKNOWN` while GitHub works it out.
    #[serde(default, deserialize_with = "nullable")]
    pub mergeable: String,
    /// `APPROVED`, `CHANGES_REQUESTED`, `REVIEW_REQUIRED`, and **empty** when
    /// no review has been asked for — which is not the same as "not reviewed"
    /// and must not be shown as one.
    #[serde(default, deserialize_with = "nullable")]
    pub review_decision: String,
    #[serde(default)]
    pub status_check_rollup: Vec<Check>,
    #[serde(default)]
    pub author: Author,
}

/// Who opened it. A struct for one field because that is the shape `gh`
/// answers with.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Deserialize)]
pub struct Author {
    #[serde(default, deserialize_with = "nullable")]
    pub login: String,
}

/// One entry of the check rollup.
///
/// **Two shapes under one type.** GitHub answers with `CheckRun`s — an Action's
/// job, which has a `status` and then a `conclusion` — and with
/// `StatusContext`s, the older commit statuses, which have a single `state`.
/// Every field is optional here because each shape leaves the other's blank,
/// and `verdict` is what reads them as one thing.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Check {
    #[serde(default, deserialize_with = "nullable")]
    pub status: String,
    #[serde(default, deserialize_with = "nullable")]
    pub conclusion: String,
    #[serde(default, deserialize_with = "nullable")]
    pub state: String,
    /// A `CheckRun`'s name…
    #[serde(default, deserialize_with = "nullable")]
    pub name: String,
    /// …and a `StatusContext`'s.
    #[serde(default, deserialize_with = "nullable")]
    pub context: String,
    #[serde(default, deserialize_with = "nullable")]
    pub workflow_name: String,
    /// Where a `CheckRun` is read…
    #[serde(default, deserialize_with = "nullable")]
    pub details_url: String,
    /// …and a `StatusContext`.
    #[serde(default, deserialize_with = "nullable")]
    pub target_url: String,
}

/// A string `gh` may write as `null`, read as an empty one.
fn nullable<'de, D: serde::Deserializer<'de>>(de: D) -> Result<String, D::Error> {
    use serde::Deserialize as _;
    Ok(Option::<String>::deserialize(de)?.unwrap_or_default())
}

/// What a check amounts to, once the two shapes are read as one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verdict {
    Passed,
    Failed,
    Running,
}

impl Check {
    pub fn verdict(&self) -> Verdict {
        // A `StatusContext` is recognised by carrying a state at all: it is the
        // only shape that has one.
        if !self.state.is_empty() {
            return match self.state.as_str() {
                "SUCCESS" => Verdict::Passed,
                "PENDING" | "EXPECTED" => Verdict::Running,
                _ => Verdict::Failed,
            };
        }
        if self.status != "COMPLETED" {
            return Verdict::Running;
        }
        match self.conclusion.as_str() {
            // **Skipped counts as passed.** A job whose `if` said no is not a
            // failure, and counting it as one paints a red cross on a branch
            // where nothing is wrong — this repository's own workflow skips
            // its browser tests that way.
            "SUCCESS" | "NEUTRAL" | "SKIPPED" => Verdict::Passed,
            _ => Verdict::Failed,
        }
    }

    /// The finer state its row shows: a skipped check is not drawn as one that
    /// passed, though it counts as one.
    pub fn stage(&self) -> Stage {
        if self.state.is_empty() {
            return crate::github::stage_of(&self.status, &self.conclusion);
        }
        match self.verdict() {
            Verdict::Passed => Stage::Passed,
            Verdict::Failed => Stage::Failed,
            Verdict::Running => Stage::Waiting,
        }
    }

    /// The name its row shows, whichever shape it came in.
    pub fn label(&self) -> &str {
        if self.name.is_empty() {
            &self.context
        } else {
            &self.name
        }
    }

    /// Where it is read, whichever shape it came in.
    pub fn link(&self) -> &str {
        if self.details_url.is_empty() {
            &self.target_url
        } else {
            &self.details_url
        }
    }
}

/// The tally the row shows: how many passed, failed, and are still running.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Checks {
    pub passed: usize,
    pub failed: usize,
    pub running: usize,
}

impl PullRequest {
    pub fn checks(&self) -> Checks {
        let mut tally = Checks::default();
        for check in &self.status_check_rollup {
            match check.verdict() {
                Verdict::Passed => tally.passed += 1,
                Verdict::Failed => tally.failed += 1,
                Verdict::Running => tally.running += 1,
            }
        }
        tally
    }

    /// Where its commits are, which is what opening it in a worktree reads.
    pub fn head(&self) -> PrHead {
        PrHead {
            number: self.number,
            head: self.head_ref_name.clone(),
            base: self.base_ref_name.clone(),
            cross: self.is_cross_repository,
        }
    }

    /// What the second line says about the review, or nothing at all when no
    /// review has been asked for.
    pub fn review_note(&self) -> Option<SharedString> {
        match self.review_decision.as_str() {
            "APPROVED" => Some(tr!("github-review-approved")),
            "CHANGES_REQUESTED" => Some(tr!("github-review-changes")),
            "REVIEW_REQUIRED" => Some(tr!("github-review-required")),
            _ => None,
        }
    }

    /// Whether the filter's word is in what the row shows.
    pub fn matches(&self, needle: &str) -> bool {
        let needle = needle.trim().to_lowercase();
        needle.is_empty()
            || self.title.to_lowercase().contains(&needle)
            || self.head_ref_name.to_lowercase().contains(&needle)
            || self.author.login.to_lowercase().contains(&needle)
            || self.number.to_string().contains(&needle)
    }

    /// Whether GitHub says the merge would conflict. `UNKNOWN` is not a
    /// warning: it is the answer given while the question is still being
    /// worked out, and showing it as one cries wolf on every fresh push.
    pub fn conflicts(&self) -> bool {
        self.mergeable == "CONFLICTING"
    }
}

/// How many pull requests the list asks for.
///
/// The whole of what a repository has open, in practice: past this one is
/// reading a backlog, and the branch's own would have to be looked for by name
/// again — the list is what says whether it has one.
const PRS: usize = 50;

/// The command that reads the repository's open pull requests.
///
/// **`pr list` and not `pr view`**, although `pr view` is what names the
/// gesture of "the pull request of this branch": with none open, `pr view`
/// exits non-zero and says so on stderr, so "there is none" and "`gh` is not
/// installed" would arrive down the same wire as the same kind of thing.
/// `pr list` answers `[]`, which is an answer — and it answers for every
/// branch at once, so the panel shows the others rather than one process per
/// branch nobody asked about.
pub fn pr_list_command() -> String {
    format!(
        "gh pr list --state open --limit {PRS} --json \
         number,title,url,isDraft,baseRefName,headRefName,isCrossRepository,mergeable,\
         reviewDecision,statusCheckRollup,author"
    )
}

/// The pull requests the answer carries.
///
/// An empty answer is **no pull request** and not a broken read: `gh` writes
/// its own notices on occasion, and a repository with nothing open is the
/// ordinary case this panel opens on.
pub fn parse_prs(out: &str) -> Result<Vec<PullRequest>, String> {
    let out = out.trim();
    if out.is_empty() {
        return Ok(Vec::new());
    }
    serde_json::from_str(out).map_err(|why| why.to_string())
}

/// The command that opens one.
///
/// `--body` and not `--body-file`: a temporary file would have to outlive the
/// send and be cleaned up after an answer that may never come, where a quoted
/// argument is gone when the process is.
pub fn create_command(
    base: Option<&str>,
    title: &str,
    body: &str,
    draft: bool,
    push: bool,
) -> String {
    let mut command = String::new();
    // **The push comes first when there is no upstream.** `gh pr create` asks
    // where to push a branch that has never been published, and stdin is
    // closed in a worker: the question would come back as a failure nobody can
    // answer. Opening a pull request is publishing the branch anyway.
    if push {
        command.push_str("git push --set-upstream origin HEAD && ");
    }
    command.push_str("gh pr create");
    if let Some(base) = base {
        command.push_str(&format!(" --base {}", quote(base)));
    }
    command.push_str(&format!(" --title {}", quote(title)));
    command.push_str(&format!(" --body {}", quote(body)));
    if draft {
        command.push_str(" --draft");
    }
    command
}

/// The subjects of the commits the branch adds, which the dialog is filled
/// from.
pub fn commits_command(base: &str) -> String {
    format!("git log --reverse --format=%s {}..HEAD", quote(base))
}

/// The base as `gh` wants it: a branch of the repository, not a remote-tracking
/// name.
///
/// **Only an `origin/` prefix is taken off**, and not everything up to the
/// first slash: the branches this window is built for carry slashes of their
/// own — `wt/…` is the worktree tool's convention — and cutting at the first
/// one would target a branch that does not exist.
pub fn base_for_gh(base: &str) -> &str {
    base.strip_prefix("origin/").unwrap_or(base)
}

/// The title and the body a new pull request opens with.
///
/// One commit gives its subject and nothing else — there is nothing a list of
/// one adds. Several give the first as the title, because that is the one that
/// says what was set out to be done, and all of them as the body: what a
/// reviewer wants first is the shape of the branch.
pub fn draft_from(subjects: &str, branch: &str) -> (String, String) {
    let subjects: Vec<&str> = subjects
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .collect();
    match subjects.as_slice() {
        [] => (branch.to_string(), String::new()),
        [only] => (only.to_string(), String::new()),
        [first, rest @ ..] => {
            let body = std::iter::once(first)
                .chain(rest)
                .map(|subject| format!("- {subject}"))
                .collect::<Vec<_>>()
                .join("\n");
            (first.to_string(), body)
        }
    }
}

/// The check tally, drawn.
///
/// A count that is zero is **not drawn**: three glyphs of which two say nothing
/// is a row one has to read before seeing there is nothing to read. It takes
/// its colours rather than the context, because it is called from inside a
/// virtualised list's closure, where the application cannot be read.
fn tally_elements(checks: Checks, colors: [Hsla; 3]) -> Vec<AnyElement> {
    let [success, danger, warning] = colors;
    [
        ("circle-check", success, checks.passed),
        ("circle-x", danger, checks.failed),
        ("loader-circle", warning, checks.running),
    ]
    .into_iter()
    .filter(|(_, _, count)| *count > 0)
    .map(|(glyph, color, count)| {
        h_flex()
            .gap_1()
            .items_center()
            .text_color(color)
            .child(icon(glyph).xsmall())
            .child(div().text_xs().child(SharedString::from(count.to_string())))
            .into_any_element()
    })
    .collect()
}

/// The colour a state is drawn in. Taken as a palette rather than from the
/// context, for the reason `tally_elements` gives.
#[derive(Clone, Copy)]
struct Palette {
    success: Hsla,
    danger: Hsla,
    warning: Hsla,
    muted: Hsla,
}

impl Palette {
    fn of(cx: &gpui_kit::App) -> Self {
        let theme = cx.theme();
        Self {
            success: theme.success,
            danger: theme.danger,
            warning: theme.warning,
            muted: theme.muted_foreground,
        }
    }

    fn stage(self, stage: Stage) -> Hsla {
        match stage {
            Stage::Passed => self.success,
            Stage::Failed => self.danger,
            Stage::Running => self.warning,
            Stage::Waiting | Stage::Skipped => self.muted,
        }
    }
}

/// Now, in the seconds the durations are counted in.
fn now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| elapsed.as_secs() as i64)
        .unwrap_or(0)
}

// — The state ———————————————————————————————————————————————————————

/// Which list the panel shows.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum Mode {
    #[default]
    Prs,
    /// The branch's runs.
    Runs,
    /// What the repository has going — every branch, every workflow.
    Live,
}

/// The two writes a run offers, both behind a confirmation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Gesture {
    Cancel,
    Rerun,
}

/// A pull request being opened in a worktree, between the fetch and the
/// worktree list that has it.
///
/// It **outlives a change of worktree**, unlike everything else here: the
/// creation's own dialog is up in between, and the list that brings the new
/// checkout is the moment to move to it.
#[derive(Debug, Clone)]
pub struct Landing {
    pub main: PathBuf,
    /// The local branch the worktree will have checked out.
    pub branch: String,
    /// The review's base once there: the pull request's.
    pub base: String,
    pub until: Instant,
}

/// The pull request whose review threads are being read, and whose agent they
/// go to.
#[derive(Debug, Clone)]
pub struct ThreadsFor {
    pub number: u64,
    pub title: String,
    pub url: String,
    pub worktree: PathBuf,
}

/// What the view shows and what it is waiting for.
#[derive(Default)]
pub struct GithubState {
    /// The worktree this reading is about, which is what says it is stale.
    pub worktree: Option<PathBuf>,
    /// The repository's open pull requests. The branch's own is **found in
    /// here** rather than read on its own: one process answers both questions,
    /// and a list that did not contain it would be a list one could not trust.
    pub prs: Vec<PullRequest>,
    pub chosen_pr: Option<usize>,
    pub pr_loading: bool,
    /// Which of the lists the panel shows. Pull requests: that is what the
    /// panel is for, and the runs are what one goes to **from** one.
    pub mode: Mode,
    pub pr_scroll: gpui_kit::UniformListScrollHandle,
    /// The branch's runs.
    pub runs: Vec<Run>,
    /// The repository's runs in flight or queued.
    pub live: Vec<Run>,
    /// The run unfolded — a copy, known **by its id** and not by a rank: both
    /// lists are read again while something runs, and a rank would move under
    /// the choice, the jobs shown and the log handed to the agent being
    /// another run's.
    ///
    /// A copy and not a place in a list, because a run that finishes **leaves**
    /// the live list, at the moment one was watching for. It is refreshed from
    /// every list that still has it, and from its own jobs' read, which says
    /// where the run is (`github::run_state`).
    pub chosen: Option<Run>,
    /// The log of the chosen run, once asked for: the button is what asks.
    pub log: Option<SharedString>,
    /// The worktree this reading was made for. `None` is "never read".
    ///
    /// The same device as `SentryState::read_for`, and simpler for the same
    /// job: what a run list depends on is one checkout and nothing else.
    pub read_for: Option<PathBuf>,
    /// The same, for the live list, which is read the first time it is shown.
    pub live_read_for: Option<PathBuf>,
    pub loading: bool,
    pub live_loading: bool,
    pub error: Option<SharedString>,
    /// The sends in flight — a late answer is dropped rather than shown.
    pub pr_call: u64,
    pub list_call: u64,
    pub live_call: u64,
    pub log_call: u64,
    /// The run `log_call` was sent for, by id. The answer carries only the
    /// text: without this it would land under whatever run is chosen when it
    /// arrives — and be handed to the agent as that run's failure.
    pub log_run: Option<String>,
    /// The chosen run's jobs, and the run they are of.
    pub jobs: Vec<Job>,
    pub jobs_for: Option<String>,
    pub jobs_call: u64,
    pub jobs_loading: bool,
    /// A job folded or unfolded by hand, against `Job::opens_by_default`.
    pub job_folds: HashMap<u64, bool>,
    /// A cancel or a re-run, sent.
    pub gesture_call: u64,
    pub gesture: Option<Gesture>,
    /// A reading is due: the timer has fired, and the next paint sends it.
    pub due: bool,
    /// The timer armed, by the id it was armed with; zero is none. A timer
    /// from before the state was reset finds another id and does nothing.
    pub timer: u64,
    /// The `git log` that fills the new-pull-request dialog, and the `gh pr
    /// create` that answers it.
    pub draft_call: u64,
    pub create_call: u64,
    /// The base the dialog was opened for, kept across the read that fills it:
    /// the answer carries subjects and nothing else.
    pub draft_base: Option<String>,
    /// A pull request is being opened. What it disables is the button that
    /// would open a second one.
    pub creating: bool,
    /// What the log, once it lands, is for: reading, or handing over.
    pub log_for_agent: bool,
    pub scroll: gpui_kit::UniformListScrollHandle,
    pub live_scroll: gpui_kit::UniformListScrollHandle,
    /// The chosen pull request's checks are unfolded.
    pub checks_open: bool,
    /// The fetch that brings a pull request here, and the pull request.
    pub checkout_call: u64,
    pub checkout: Option<PrHead>,
    pub landing: Option<Landing>,
    pub threads_call: u64,
    pub threads_for: Option<ThreadsFor>,
}

impl GithubState {
    /// A state for `worktree`, keeping what is not about the worktree: the
    /// list one was looking at, and a landing still on its way.
    fn fresh(&mut self, worktree: Option<PathBuf>) -> Self {
        Self {
            worktree,
            mode: self.mode,
            landing: self.landing.take(),
            ..Default::default()
        }
    }

    /// The chosen run.
    pub fn chosen_run(&self) -> Option<&Run> {
        self.chosen.as_ref()
    }

    fn chosen_id(&self) -> Option<&str> {
        self.chosen.as_ref().map(|run| run.id.as_str())
    }

    /// Takes the chosen run's figures from a list just read, if it has it.
    fn refresh_chosen(&mut self, from: Mode) {
        let Some(id) = self.chosen_id() else {
            return;
        };
        if let Some(run) = self.list(from).iter().find(|run| run.id == id).cloned() {
            self.chosen = Some(run);
        }
    }

    /// Whether the unfolded run is worth reading again: it, or one of its
    /// jobs, is still going.
    fn chosen_going(&self) -> bool {
        self.chosen.as_ref().is_some_and(Run::is_active) || still_going(&[], &self.jobs)
    }

    /// The list a mode shows.
    fn list(&self, mode: Mode) -> &[Run] {
        match mode {
            Mode::Live => &self.live,
            _ => &self.runs,
        }
    }

    /// Whether a job's steps show.
    fn job_open(&self, job: &Job) -> bool {
        self.job_folds
            .get(&job.id)
            .copied()
            .unwrap_or_else(|| job.opens_by_default())
    }
}

impl ClaudhubApp {
    fn ask_gh(&mut self, command: String, worktree: PathBuf) -> u64 {
        self.github_seq += 1;
        let call = self.github_seq;
        self.git.send(Cmd::Call {
            caller: Caller::Github,
            call,
            cap: crate::outside::Cap::Shell { worktree, command },
        });
        call
    }

    /// The branch checked out **now** in the worktree being looked at.
    ///
    /// The status's and not the worktree list's: a checkout rereads the status,
    /// and nothing rereads the list — which is the same reading
    /// `ClaudhubApp::status_arrived` trusts.
    fn branch_here(&self) -> Option<String> {
        let worktree = self.active.as_deref()?;
        self.review.get(worktree)?.status.branch.clone()
    }

    /// Whether the branch has never been published, which is what decides
    /// whether opening a pull request has to push first.
    fn unpublished(&self) -> bool {
        self.active
            .as_deref()
            .and_then(|worktree| self.review.get(worktree))
            .is_some_and(|state| state.status.upstream.is_none())
    }

    /// The comparison base of the branch review, which is the branch a pull
    /// request would target: the two questions are the same question, and
    /// answering them differently would compare one thing on screen and merge
    /// another.
    fn base_here(&self) -> Option<String> {
        let worktree = self.active.as_deref()?;
        self.review.get(worktree)?.base.clone()
    }

    /// Reads what the panel shows **the first time it is drawn**, and never
    /// before — and, while something runs, again when the timer says so.
    ///
    /// Each read is a process and a network round trip of its own, and a
    /// checkout one passes through on the way to another is not a reason for
    /// any. See `ClaudhubApp::ensure_sentry` for the whole of the rule; this is
    /// also the only place a timed reading is sent from, which is what keeps a
    /// hidden panel silent.
    pub(super) fn ensure_github(&mut self, cx: &mut Context<Self>) {
        if !self.github.loading && !self.github.pr_loading && self.github.read_for != self.active {
            self.load_github(cx);
        }
        if self.github.mode == Mode::Live
            && !self.github.live_loading
            && self.github.live_read_for != self.active
        {
            self.load_live(cx);
        }
        if std::mem::take(&mut self.github.due) {
            self.read_again(cx);
        }
    }

    /// Reads the pull requests and the branch's runs, replacing whatever was
    /// there.
    pub(super) fn load_github(&mut self, cx: &mut Context<Self>) {
        let worktree = self.active.clone();
        let branch = self.branch_here();
        self.github = self.github.fresh(worktree.clone());
        self.github.read_for = worktree.clone();
        let Some(worktree) = worktree else {
            cx.notify();
            return;
        };
        self.github.loading = true;
        self.github.pr_loading = true;
        self.github.list_call = self.ask_gh(list_command(branch.as_deref()), worktree.clone());
        self.github.pr_call = self.ask_gh(pr_list_command(), worktree);
        cx.notify();
    }

    /// Reads what the repository has going.
    fn load_live(&mut self, cx: &mut Context<Self>) {
        let Some(worktree) = self.active.clone() else {
            return;
        };
        self.github.live_read_for = Some(worktree.clone());
        self.github.live_loading = true;
        self.github.live_call = self.ask_gh(live_command(), worktree);
        cx.notify();
    }

    /// Reads again what the panel shows and what is still going: the list of
    /// the mode, and the unfolded run's jobs. Nothing that is already on its
    /// way is asked twice.
    fn read_again(&mut self, cx: &mut Context<Self>) {
        let Some(worktree) = self.active.clone() else {
            return;
        };
        match self.github.mode {
            Mode::Live if !self.github.live_loading => self.load_live(cx),
            Mode::Runs if !self.github.loading => {
                let branch = self.branch_here();
                self.github.loading = true;
                self.github.list_call =
                    self.ask_gh(list_command(branch.as_deref()), worktree.clone());
            }
            _ => {}
        }
        // The unfolded run's jobs, while it or they are going: a run that has
        // finished and whose jobs have said so has nothing new to say.
        if self.github.mode != Mode::Prs && !self.github.jobs_loading && self.github.chosen_going()
        {
            if let Some(id) = self.github.jobs_for.clone() {
                self.github.jobs_loading = true;
                self.github.jobs_call = self.ask_gh(jobs_command(&id), worktree);
            }
        }
        cx.notify();
    }

    /// Arms the timer, if something shown is still going and none is armed.
    ///
    /// The timer **does not read**: it marks the reading due and asks for a
    /// frame. Only a painted panel runs `ensure_github`, which is what sends —
    /// see the module's comment.
    fn arm_live_timer(&mut self, cx: &mut Context<Self>) {
        let github = &self.github;
        let going = github.mode != Mode::Prs
            && (still_going(github.list(github.mode), &[]) || github.chosen_going());
        if !going || github.timer != 0 {
            return;
        }
        self.github_seq += 1;
        let token = self.github_seq;
        self.github.timer = token;
        cx.spawn(async move |this, cx| {
            cx.background_executor().timer(LIVE_PERIOD).await;
            let _ = this.update(cx, |this, cx| {
                if this.github.timer != token {
                    return;
                }
                this.github.timer = 0;
                this.github.due = true;
                cx.notify();
            });
        })
        .detach();
    }

    /// The branch's own pull request, among the ones the repository has open.
    pub(super) fn branch_pr(&self) -> Option<&PullRequest> {
        let branch = self.branch_here()?;
        self.github.prs.iter().find(|pr| pr.head_ref_name == branch)
    }

    /// Chooses one. It asks for nothing: everything a row shows came with the
    /// list.
    pub(super) fn choose_pr(&mut self, rank: usize, cx: &mut Context<Self>) {
        self.github.chosen_pr = Some(rank);
        cx.notify();
    }

    /// Shows one list or another.
    ///
    /// A list whose runs are still going is stale by definition: it is read
    /// again at the next paint rather than shown as it was.
    pub(super) fn set_github_mode(&mut self, mode: Mode, cx: &mut Context<Self>) {
        self.github.mode = mode;
        if mode != Mode::Prs && still_going(self.github.list(mode), &[]) {
            self.github.due = true;
        }
        cx.notify();
    }

    /// Opens a pull request in the browser — by its address, which came with
    /// the list: no process, and the browser is the one of the desktop the
    /// window is on, which under WSL is not the one `gh --web` would look for.
    pub(super) fn open_pr_in_browser(&mut self, rank: usize, cx: &mut Context<Self>) {
        if let Some(pr) = self.github.prs.get(rank) {
            cx.open_url(&pr.url);
        }
    }

    /// Asks for the subjects of the commits the branch adds, then opens the
    /// dialog with them.
    ///
    /// The read is paid for **on the click** and not with the panel: a title
    /// proposed to somebody who will not open a pull request is a `git log`
    /// nobody asked for.
    pub(super) fn prompt_new_pr(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let (Some(worktree), Some(branch)) = (self.active.clone(), self.branch_here()) else {
            return;
        };
        let Some(base) = self.base_here() else {
            // Without a base there is nothing to list, and `gh` still knows the
            // repository's default branch: the dialog opens on the branch name.
            self.open_pr_dialog(None, branch.clone(), String::new(), window, cx);
            return;
        };
        self.github.draft_base = Some(base.clone());
        self.github.draft_call = self.ask_gh(commits_command(&base), worktree);
        cx.notify();
    }

    /// Opens a pull request, and reads everything back once it is there.
    fn create_pr(
        &mut self,
        base: Option<String>,
        title: String,
        body: String,
        draft: bool,
        cx: &mut Context<Self>,
    ) {
        let Some(worktree) = self.active.clone() else {
            return;
        };
        if title.trim().is_empty() {
            return;
        }
        let command = create_command(
            base.as_deref().map(base_for_gh),
            title.trim(),
            &body,
            draft,
            self.unpublished(),
        );
        self.github.creating = true;
        self.github.create_call = self.ask_gh(command, worktree);
        cx.notify();
    }

    /// The view follows the worktree, like every other panel.
    ///
    /// It **forgets** rather than reads: the next paint of the panel is what
    /// asks for the arriving worktree's. The list one was looking at stays the
    /// one shown, and a pull request on its way to a worktree keeps going there.
    pub(super) fn github_follows_worktree(&mut self, cx: &mut Context<Self>) {
        if self.github.worktree == self.active {
            return;
        }
        self.github = self.github.fresh(self.active.clone());
        cx.notify();
    }

    /// Unfolds a run: its jobs are read at once.
    ///
    /// Choosing **is** the gesture that asks — the jobs are what one unfolds a
    /// run to see. The log is not: it is the second process, and the button
    /// says when it is wanted.
    pub(super) fn choose_ci_run(&mut self, run: Run, cx: &mut Context<Self>) {
        let Some(worktree) = self.active.clone() else {
            return;
        };
        if self.github.chosen_id() == Some(run.id.as_str()) {
            return;
        }
        self.github.log = None;
        self.github.jobs = Vec::new();
        self.github.job_folds.clear();
        self.github.jobs_loading = true;
        self.github.jobs_for = Some(run.id.clone());
        self.github.jobs_call = self.ask_gh(jobs_command(&run.id), worktree);
        self.github.chosen = Some(run);
        cx.notify();
    }

    /// Folds or unfolds a job's steps.
    fn toggle_job(&mut self, id: u64, cx: &mut Context<Self>) {
        let open = self
            .github
            .jobs
            .iter()
            .find(|job| job.id == id)
            .is_some_and(|job| self.github.job_open(job));
        self.github.job_folds.insert(id, !open);
        cx.notify();
    }

    /// Asks for the chosen run's failing log — to read, or to hand over.
    pub(super) fn read_ci_log(&mut self, for_agent: bool, cx: &mut Context<Self>) {
        let Some(worktree) = self.active.clone() else {
            return;
        };
        let Some(id) = self.github.chosen_run().map(|run| run.id.clone()) else {
            return;
        };
        self.github.log_call = self.ask_gh(log_command(&id), worktree);
        self.github.log_run = Some(id);
        self.github.log_for_agent = for_agent;
        cx.notify();
    }

    /// Asks whether to cancel the chosen run, or to re-run its failed jobs.
    ///
    /// Both behind a confirmation: they act on GitHub, for everyone watching
    /// the run, and a cancelled deployment is not undone by a second click.
    fn confirm_run_gesture(
        &mut self,
        gesture: Gesture,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(run) = self.github.chosen_run().cloned() else {
            return;
        };
        let (title, body, label) = match gesture {
            Gesture::Cancel => (
                tr!("ci-cancel-title"),
                tr!("ci-cancel-body", { title: run.title.clone(), workflow: run.workflow.clone() }),
                tr!("ci-cancel"),
            ),
            Gesture::Rerun => (
                tr!("ci-rerun-title"),
                tr!("ci-rerun-body", { title: run.title.clone(), workflow: run.workflow.clone() }),
                tr!("ci-rerun"),
            ),
        };
        let entity = cx.entity();
        window.open_dialog(cx, move |dialog, _window, _cx| {
            // Cloned into the closure and never read from it: `open_dialog`
            // keeps a `Fn` called back from the root's own render.
            let (entity, id) = (entity.clone(), run.id.clone());
            dialog
                .title(title.clone())
                .child(div().text_sm().child(body.clone()))
                .overlay_closable(false)
                .close_button(false)
                .footer(crate::ui::dialogs::submit(label.clone()))
                .on_ok(move |_, _window, cx| {
                    let id = id.clone();
                    entity.update(cx, |this, cx| this.run_gesture(gesture, id, cx));
                    true
                })
        });
    }

    /// Sends a confirmed cancel or re-run.
    fn run_gesture(&mut self, gesture: Gesture, id: String, cx: &mut Context<Self>) {
        let Some(worktree) = self.active.clone() else {
            return;
        };
        let command = match gesture {
            Gesture::Cancel => cancel_command(&id),
            Gesture::Rerun => rerun_command(&id),
        };
        self.github.gesture = Some(gesture);
        self.github.gesture_call = self.ask_gh(command, worktree);
        cx.notify();
    }

    /// Opens the chosen run on GitHub.
    fn open_run_in_browser(&mut self, cx: &mut Context<Self>) {
        if let Some(run) = self.github.chosen_run() {
            if !run.url.is_empty() {
                cx.open_url(&run.url);
            }
        }
    }

    // — A pull request, in a worktree ————————————————————————————————

    /// The worktrees of the repository being looked at, with their branches.
    fn worktrees_here(&self) -> Option<(PathBuf, Vec<Checkout>)> {
        let active = self.active.as_deref()?;
        let repo = self.repo_of(active)?;
        let worktrees = repo
            .worktrees
            .iter()
            .map(|worktree| (worktree.path.clone(), worktree.branch.clone()))
            .collect();
        Some((repo.main.clone(), worktrees))
    }

    /// Opens a pull request in a worktree: the one that has its branch, or a
    /// new one on it.
    ///
    /// A new one goes **the road every creation goes** —
    /// `worktree_from_branch`, so `wt`'s questions, copies, ports and
    /// `post_new` when the project has a `wt.toml`, the bare `git worktree
    /// add` otherwise — once the branch is here: the fetch comes first, in the
    /// main checkout, and never as `gh pr checkout`, which would switch that
    /// checkout's branch. A fork's branch is not on `origin`; its
    /// `refs/pull/<n>/head` is, fetched into `pr/<n>`.
    pub(super) fn open_pr_worktree(
        &mut self,
        rank: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(head) = self.github.prs.get(rank).map(PullRequest::head) else {
            return;
        };
        let Some((main, worktrees)) = self.worktrees_here() else {
            return;
        };
        if let Some(path) = worktree_of(&head, &worktrees).map(PathBuf::from) {
            self.land_on(path, head.review_base(), window, cx);
            return;
        }
        self.github.checkout = Some(head.clone());
        self.github.checkout_call = self.ask_gh(head.fetch_command(), main);
        cx.notify();
    }

    /// The pull request is here: its worktree is created, and landed on once
    /// the worktree list has it.
    fn pr_fetched(&mut self, head: PrHead, window: &mut Window, cx: &mut Context<Self>) {
        let Some(active) = self.active.clone() else {
            return;
        };
        let Some(repo) = self.repo_of(&active) else {
            return;
        };
        let main = repo.main.clone();
        let locals: Vec<&str> = repo
            .branches
            .iter()
            .filter(|branch| branch.kind == crate::git::BranchKind::Local)
            .map(|branch| branch.name.as_str())
            .collect();
        let branch = head.branch_to_create(&locals);
        self.github.landing = Some(Landing {
            main: main.clone(),
            branch: head.local_branch(),
            base: head.review_base(),
            until: Instant::now() + LANDING_WAIT,
        });
        self.worktree_from_branch(main, branch, window, cx);
    }

    /// Goes to a pull request's worktree, and reviews its branch against the
    /// pull request's base: that comparison is what a review of it is.
    fn land_on(
        &mut self,
        worktree: PathBuf,
        base: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.select_worktree(worktree, window, cx);
        self.compare_against(base, window, cx);
    }

    /// A worktree list has arrived: if it has the worktree a pull request was
    /// being opened in, that is where the window goes.
    pub(super) fn pr_worktree_arrived(
        &mut self,
        main: &std::path::Path,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(landing) = self.github.landing.as_ref() else {
            return;
        };
        if landing.until < Instant::now() {
            self.github.landing = None;
            return;
        }
        if landing.main != main {
            return;
        }
        // By its main path and not through `repo_of`, which looks for a
        // worktree of that path — and a bare repository's main is none.
        let found = self
            .repos
            .iter()
            .find(|repo| repo.main == main)
            .and_then(|repo| {
                repo.worktrees
                    .iter()
                    .find(|worktree| worktree.branch.as_deref() == Some(landing.branch.as_str()))
                    .map(|worktree| worktree.path.clone())
            });
        let Some(path) = found else {
            return;
        };
        let Some(landing) = self.github.landing.take() else {
            return;
        };
        self.land_on(path, landing.base, window, cx);
    }

    /// Reads a pull request's review threads, to hand the unresolved ones to
    /// the agent of its worktree.
    ///
    /// **Its worktree's agent** and not the one of the worktree being looked
    /// at: the comments are about that branch's code, and an agent in another
    /// checkout would fix them in the wrong one. Without a worktree, it says
    /// so — opening one is the button beside.
    fn hand_threads(&mut self, rank: usize, cx: &mut Context<Self>) {
        let Some(pr) = self.github.prs.get(rank).cloned() else {
            return;
        };
        let Some((_, worktrees)) = self.worktrees_here() else {
            return;
        };
        let Some(worktree) = worktree_of(&pr.head(), &worktrees).map(PathBuf::from) else {
            self.announce_error(tr!("github-threads-no-worktree", { n: pr.number }), cx);
            return;
        };
        let Some(active) = self.active.clone() else {
            return;
        };
        self.github.threads_for = Some(ThreadsFor {
            number: pr.number,
            title: pr.title.clone(),
            url: pr.url.clone(),
            worktree,
        });
        self.github.threads_call = self.ask_gh(threads_command(pr.number), active);
        cx.notify();
    }

    fn threads_arrived(
        &mut self,
        result: Result<String, String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(target) = self.github.threads_for.take() else {
            return;
        };
        let threads = match result.and_then(|out| parse_threads(&out)) {
            Ok(threads) => threads,
            Err(why) => {
                self.github.error = Some(SharedString::from(why));
                return;
            }
        };
        if unresolved(&threads).is_empty() {
            self.announce(tr!("github-threads-none", { n: target.number }), cx);
            return;
        }
        let text = threads_prompt(
            &tr!("github-threads-intro"),
            target.number,
            &target.title,
            &target.url,
            &threads,
        );
        // Into that worktree first: the dock shows the terminals of the
        // worktree being looked at, and an agent receiving the comments behind
        // the scenes is an agent nobody sees start on them.
        if self.active.as_deref() != Some(target.worktree.as_path()) {
            self.select_worktree(target.worktree.clone(), window, cx);
        }
        self.confirm_agent_prompt(target.worktree, text, window, cx);
    }

    /// One of the answers, back from the worker.
    pub(super) fn github_answered(
        &mut self,
        call: u64,
        result: Result<String, String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let github = &mut self.github;
        if call == github.list_call {
            github.loading = false;
            match result {
                Ok(out) => {
                    github.runs = parse_runs(&out);
                    github.refresh_chosen(Mode::Runs);
                    github.error = None;
                }
                Err(why) => {
                    github.runs = Vec::new();
                    github.error = Some(SharedString::from(why));
                }
            }
            self.arm_live_timer(cx);
        } else if call == github.live_call {
            github.live_loading = false;
            match result {
                Ok(out) => {
                    github.live = parse_runs(&out);
                    github.refresh_chosen(Mode::Live);
                }
                Err(why) => github.error = Some(SharedString::from(why)),
            }
            self.arm_live_timer(cx);
        } else if call == github.jobs_call {
            github.jobs_loading = false;
            // Only the chosen run's: one chosen meanwhile has its own read.
            if github.jobs_for.as_deref() == github.chosen_id() {
                if let (Ok(out), Some(chosen)) = (&result, github.chosen.as_mut()) {
                    if let Some((status, conclusion)) = run_state(out) {
                        chosen.status = status;
                        chosen.conclusion = conclusion;
                    }
                }
                match result.and_then(|out| parse_jobs(&out)) {
                    Ok(jobs) => github.jobs = jobs,
                    Err(why) => github.error = Some(SharedString::from(why)),
                }
            }
            self.arm_live_timer(cx);
        } else if call == github.pr_call {
            github.pr_loading = false;
            match result.and_then(|out| parse_prs(&out)) {
                Ok(prs) => github.prs = prs,
                Err(why) => {
                    github.prs = Vec::new();
                    github.error = Some(SharedString::from(why));
                }
            }
        } else if call == github.gesture_call {
            let gesture = github.gesture.take();
            match result {
                Ok(_) => {
                    let note = match gesture {
                        Some(Gesture::Cancel) => tr!("ci-cancelled"),
                        _ => tr!("ci-rerun-started"),
                    };
                    self.announce(note, cx);
                    // What was cancelled or restarted is what one looks at:
                    // read at once rather than in ten seconds.
                    self.read_again(cx);
                }
                Err(why) => github.error = Some(SharedString::from(why)),
            }
        } else if call == github.checkout_call {
            let head = github.checkout.take();
            match (result, head) {
                (Ok(_), Some(head)) => self.pr_fetched(head, window, cx),
                (Err(why), _) => github.error = Some(SharedString::from(why)),
                (Ok(_), None) => {}
            }
        } else if call == github.threads_call {
            self.threads_arrived(result, window, cx);
        } else if call == github.draft_call {
            let branch = self.branch_here().unwrap_or_default();
            let base = self.github.draft_base.take();
            match result {
                Ok(subjects) => {
                    let (title, body) = draft_from(&subjects, &branch);
                    self.open_pr_dialog(base, title, body, window, cx);
                }
                // The proposal is a convenience: losing it is not a reason to
                // refuse the gesture, so the dialog opens on the branch name.
                Err(why) => {
                    self.github.error = Some(SharedString::from(why));
                    self.open_pr_dialog(base, branch, String::new(), window, cx);
                }
            }
        } else if call == github.create_call {
            github.creating = false;
            match result {
                // Everything is read back: the pull request that has just been
                // opened, and the runs its push has started.
                Ok(_) => self.load_github(cx),
                Err(why) => github.error = Some(SharedString::from(why)),
            }
        } else if call == github.log_call {
            let asked = github.log_run.take();
            // Under the run that asked for it, or nowhere: choosing another
            // meanwhile must not paint — nor hand the agent — someone else's
            // failure.
            if asked.is_none() || asked.as_deref() != github.chosen_id() {
                cx.notify();
                return;
            }
            match result {
                Ok(text) if github.log_for_agent => {
                    github.log = Some(SharedString::from(tail(&text, TAIL)));
                    self.hand_ci_run(&text, window, cx);
                }
                Ok(text) => github.log = Some(SharedString::from(tail(&text, TAIL))),
                Err(why) => github.log = Some(SharedString::from(why)),
            }
        }
        cx.notify();
    }

    /// Hands the failure to an agent, with the end of its log.
    ///
    /// The paste goes into the agent's tab: it is what has the repository in
    /// its hands. Claudhub never talks to an API for this — the same framing as
    /// the proposed commit message.
    fn hand_ci_run(&mut self, log: &str, window: &mut Window, cx: &mut Context<Self>) {
        let Some(worktree) = self.active.clone() else {
            return;
        };
        let Some(run) = self.github.chosen_run().cloned() else {
            return;
        };
        let text = format!(
            "{}\n\n{}",
            tr!("ci-prompt", { title: run.title.clone(), workflow: run.workflow.clone() }),
            tail(log, TAIL_FOR_AGENT),
        );
        self.confirm_agent_prompt(worktree, text, window, cx);
    }

    /// Opens the dialog a pull request is written in.
    fn open_pr_dialog(
        &mut self,
        base: Option<String>,
        title: String,
        body: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let draft = cx.new(|cx| PrDraft {
            title: cx.new(|cx| {
                InputState::new(window, cx)
                    .placeholder(tr!("github-new-title-placeholder"))
                    .default_value(title)
            }),
            body: cx.new(|cx| {
                TextareaState::new(window, cx)
                    .auto_grow(6, 16)
                    .placeholder(tr!("github-new-body-placeholder"))
                    .default_value(body)
            }),
            draft: false,
            base: base
                .as_deref()
                .map(|base| SharedString::from(base_for_gh(base).to_string())),
            // Said in the dialog because it is a second thing the button does,
            // and a branch published without being asked is a surprise.
            push: self.unpublished(),
        });
        let entity = cx.entity();
        let field = draft.read(cx).title.clone();
        window.open_dialog(cx, move |dialog, _window, _cx| {
            // Cloned into the closure and never read from it: `open_dialog`
            // keeps a `Fn` called back from the root's own render, where
            // reading the application is a panic. See "Conventions gpui".
            let (entity, draft, base) = (entity.clone(), draft.clone(), base.clone());
            dialog
                .title(tr!("github-new-title"))
                .child(draft.clone())
                .overlay_closable(false)
                .close_button(false)
                .footer(crate::ui::dialogs::confirm())
                .on_ok(move |_, _window, cx| {
                    // Read on the click, where the borrow has been given back.
                    let (title, body, is_draft) = {
                        let draft = draft.read(cx);
                        (
                            draft.title.read(cx).value().to_string(),
                            draft.body.read(cx).value().to_string(),
                            draft.draft,
                        )
                    };
                    let base = base.clone();
                    entity.update(cx, |this, cx| {
                        this.create_pr(base, title, body, is_draft, cx)
                    });
                    true
                })
        });
        crate::ui::dialogs::focus_field(&field, window, cx);
    }

    /// The empty state, in the middle of the panel.
    fn render_github_note(&self, note: SharedString, cx: &Context<Self>) -> AnyElement {
        v_flex()
            .size_full()
            .items_center()
            .justify_center()
            .gap_2()
            .p_4()
            .text_color(cx.theme().muted_foreground)
            .child(icon("github"))
            .child(div().text_sm().text_center().child(note))
            .into_any_element()
    }

    /// The repository's open pull requests.
    ///
    /// **The whole repository and not this branch alone.** A panel that only
    /// ever spoke of the branch one stands on was empty on every branch that
    /// has no pull request — which is most of them, and every branch one is
    /// about to open one from. What one comes to this tab for is the others.
    fn render_pr_list(&mut self, cx: &mut Context<Self>) -> AnyElement {
        if self.active.is_none() {
            return self.render_github_note(tr!("no-worktree"), cx);
        }
        if self.github.pr_loading && self.github.prs.is_empty() {
            return self.render_github_note(tr!("github-pr-loading"), cx);
        }
        if self.github.prs.is_empty() {
            return self.render_github_note(tr!("github-pr-empty"), cx);
        }

        let query = self.query(Pane::Github, cx);
        let rows: std::rc::Rc<Vec<usize>> = std::rc::Rc::new(
            self.github
                .prs
                .iter()
                .enumerate()
                .filter(|(_, pr)| pr.matches(&query))
                .map(|(rank, _)| rank)
                .collect(),
        );
        let prs = std::rc::Rc::new(self.github.prs.clone());
        let here = self.branch_here();
        let chosen = self.github.chosen_pr;
        let entity = cx.entity();
        let theme = cx.theme();
        let (muted, accent, selected, hovered) = (
            theme.muted_foreground,
            theme.accent_foreground,
            theme.accent,
            theme.secondary,
        );
        let colors = [theme.success, theme.danger, theme.warning];
        let row_height = crate::ui::theme::row_height(cx) * 2.;
        let count = rows.len();
        v_flex()
            .size_full()
            .child(
                gpui_kit::uniform_list("github-prs", count, {
                    let rows = rows.clone();
                    move |range, _window, _cx| {
                        range
                            .map(|row| {
                                let rank = rows[row];
                                let pr = &prs[rank];
                                let app = entity.clone();
                                let tally = tally_elements(pr.checks(), colors);
                                h_flex()
                                    .id(("github-pr", rank))
                                    .w_full()
                                    .px_2()
                                    .gap_2()
                                    .items_center()
                                    .h(row_height)
                                    .when(chosen == Some(rank), |el| el.bg(selected))
                                    .hover(|el| el.bg(hovered))
                                    .child(
                                        icon(if pr.is_draft {
                                            "circle-dashed"
                                        } else {
                                            "git-pull-request"
                                        })
                                        .xsmall(),
                                    )
                                    .child(
                                        v_flex()
                                            .flex_1()
                                            .min_w_0()
                                            .child(
                                                h_flex()
                                                    .gap_1()
                                                    .items_center()
                                                    .child(div().text_xs().text_color(muted).child(
                                                        SharedString::from(format!(
                                                            "#{}",
                                                            pr.number
                                                        )),
                                                    ))
                                                    .child(
                                                        div()
                                                            .flex_1()
                                                            .min_w_0()
                                                            .truncate()
                                                            .text_sm()
                                                            .child(SharedString::from(
                                                                pr.title.clone(),
                                                            )),
                                                    ),
                                            )
                                            .child(
                                                h_flex()
                                                    .gap_1()
                                                    .items_center()
                                                    .text_xs()
                                                    .child(
                                                        div().truncate().text_color(muted).child(
                                                            SharedString::from(format!(
                                                                "{} ← {}",
                                                                pr.base_ref_name, pr.head_ref_name
                                                            )),
                                                        ),
                                                    )
                                                    // **Which one is this
                                                    // checkout's.** Without it
                                                    // the branch one stands on
                                                    // is a row like any other.
                                                    .when(
                                                        here.as_deref()
                                                            == Some(pr.head_ref_name.as_str()),
                                                        |el| {
                                                            el.child(
                                                                div()
                                                                    .text_color(accent)
                                                                    .child(tr!("github-pr-here")),
                                                            )
                                                        },
                                                    ),
                                            ),
                                    )
                                    .children(tally)
                                    .on_click(move |_, _window, cx| {
                                        app.update(cx, |this, cx| this.choose_pr(rank, cx));
                                    })
                                    .into_any_element()
                            })
                            .collect()
                    }
                })
                .track_scroll(&self.github.pr_scroll)
                .flex_1()
                .min_h_0(),
            )
            .children(self.render_pr_detail(cx))
            .into_any_element()
    }

    /// What the chosen pull request offers. It only appears once a row is
    /// chosen: an empty section would push the list out of sight to say
    /// nothing.
    fn render_pr_detail(&mut self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let rank = self.github.chosen_pr?;
        let pr = self.github.prs.get(rank)?.clone();
        let fetching = self
            .github
            .checkout
            .as_ref()
            .is_some_and(|head| head.number == pr.number);
        let reading_threads = self
            .github
            .threads_for
            .as_ref()
            .is_some_and(|target| target.number == pr.number);
        let theme = cx.theme();
        let (muted, danger, border) = (theme.muted_foreground, theme.danger, theme.border);
        let checks = self.render_pr_checks(&pr, cx);
        Some(
            v_flex()
                .gap_1()
                .p_2()
                .border_t_1()
                .border_color(border)
                .child(
                    div()
                        .text_sm()
                        .truncate()
                        .child(SharedString::from(pr.title.clone())),
                )
                .child(
                    div()
                        .text_xs()
                        .text_color(muted)
                        .child(SharedString::from(format!(
                            "{} ← {} · {}",
                            pr.base_ref_name, pr.head_ref_name, pr.author.login
                        ))),
                )
                .when_some(pr.review_note(), |el, note| {
                    el.child(div().text_xs().text_color(muted).child(note))
                })
                .when(pr.conflicts(), |el| {
                    el.child(
                        div()
                            .text_xs()
                            .text_color(danger)
                            .child(tr!("github-pr-conflicts", { base: pr.base_ref_name.clone() })),
                    )
                })
                .child(
                    h_flex()
                        .flex_wrap()
                        .gap_1()
                        .child(
                            Button::new("github-pr-worktree")
                                .primary()
                                .small()
                                .icon(icon("folder-plus"))
                                .label(tr!("github-pr-worktree"))
                                .loading(fetching)
                                .disabled(fetching)
                                .on_click(cx.listener(move |this, _, window, cx| {
                                    this.open_pr_worktree(rank, window, cx)
                                })),
                        )
                        .child(
                            Button::new("github-pr-threads")
                                .ghost()
                                .small()
                                .icon(icon("bot"))
                                .label(tr!("github-pr-threads"))
                                .loading(reading_threads)
                                .disabled(reading_threads)
                                .on_click(cx.listener(move |this, _, _window, cx| {
                                    this.hand_threads(rank, cx)
                                })),
                        )
                        .child(
                            Button::new("github-pr-open")
                                .ghost()
                                .small()
                                .icon(icon("external-link"))
                                .label(tr!("github-pr-open"))
                                .on_click(cx.listener(move |this, _, _window, cx| {
                                    this.open_pr_in_browser(rank, cx)
                                })),
                        ),
                )
                .children(checks)
                .into_any_element(),
        )
    }

    /// The pull request's checks, folded under their tally: one row each once
    /// unfolded, a click opening where it is read.
    fn render_pr_checks(&mut self, pr: &PullRequest, cx: &mut Context<Self>) -> Option<AnyElement> {
        if pr.status_check_rollup.is_empty() {
            return None;
        }
        let open = self.github.checks_open;
        let palette = Palette::of(cx);
        let colors = [palette.success, palette.danger, palette.warning];
        let hovered = cx.theme().secondary;
        let header = h_flex()
            .id("github-pr-checks")
            .gap_1()
            .items_center()
            .text_xs()
            .text_color(palette.muted)
            .cursor_pointer()
            .hover(|el| el.bg(hovered))
            .child(
                icon(if open {
                    "chevron-down"
                } else {
                    "chevron-right"
                })
                .xsmall(),
            )
            .child(tr!("github-checks"))
            .children(tally_elements(pr.checks(), colors))
            .on_click(cx.listener(|this, _, _window, cx| {
                this.github.checks_open = !this.github.checks_open;
                cx.notify();
            }));
        let rows = open.then(|| {
            div()
                .id("github-pr-check-rows")
                .max_h(gpui_kit::px(220.))
                .overflow_y_scroll()
                .children(
                    pr.status_check_rollup
                        .iter()
                        .enumerate()
                        .map(|(rank, check)| {
                            let stage = check.stage();
                            let link = check.link().to_string();
                            h_flex()
                                .id(("github-pr-check", rank))
                                .gap_1()
                                .px_1()
                                .items_center()
                                .text_xs()
                                .when(!link.is_empty(), |el| {
                                    el.cursor_pointer()
                                        .hover(|el| el.bg(hovered))
                                        .on_click(move |_, _window, cx| cx.open_url(&link))
                                })
                                .child(
                                    div()
                                        .text_color(palette.stage(stage))
                                        .child(icon(stage.glyph()).xsmall()),
                                )
                                .child(
                                    div()
                                        .flex_1()
                                        .min_w_0()
                                        .truncate()
                                        .child(SharedString::from(check.label().to_string())),
                                )
                                .when(!check.workflow_name.is_empty(), |el| {
                                    el.child(
                                        div()
                                            .truncate()
                                            .text_color(palette.muted)
                                            .child(SharedString::from(check.workflow_name.clone())),
                                    )
                                })
                        }),
                )
        });
        Some(
            v_flex()
                .gap_1()
                .child(header)
                .children(rows)
                .into_any_element(),
        )
    }

    /// A list of runs: the branch's, or what the repository has going.
    ///
    /// One drawing for both, because a run is a run: the glyph, the title, its
    /// workflow — with the branch and the event in the live list, where they
    /// are not all the same — and on the right how long it has been going, or
    /// how it ended.
    fn render_run_list(&mut self, mode: Mode, cx: &mut Context<Self>) -> AnyElement {
        // **An empty list is not a suspicion of `gh`.** Filtered by branch,
        // empty is the ordinary answer on a project whose workflows only run on
        // a tag; in the live list it is the ordinary answer, full stop. What
        // `gh` missing looks like is a failure, and a failure has its own strip.
        let (loading, runs) = match mode {
            Mode::Live => (self.github.live_loading, &self.github.live),
            _ => (self.github.loading, &self.github.runs),
        };
        let note = if self.active.is_none() {
            Some(tr!("no-worktree"))
        } else if !runs.is_empty() {
            None
        } else if loading && mode == Mode::Live {
            Some(tr!("ci-live-loading"))
        } else if loading {
            Some(tr!("ci-loading"))
        } else if mode == Mode::Live {
            Some(tr!("ci-live-empty"))
        } else {
            Some(match self.branch_here() {
                Some(branch) => tr!("ci-empty-branch", { branch: branch }),
                None => tr!("ci-empty"),
            })
        };
        if let Some(note) = note {
            return self.render_github_note(note, cx);
        }

        let query = self.query(Pane::Github, cx);
        let runs: std::rc::Rc<Vec<Run>> = std::rc::Rc::new(
            runs.iter()
                .filter(|run| run.matches(&query))
                .cloned()
                .collect(),
        );
        let chosen = self.github.chosen_id().map(str::to_string);
        let entity = cx.entity();
        let theme = cx.theme();
        let (selected, hovered) = (theme.accent, theme.secondary);
        let palette = Palette::of(cx);
        let row_height = crate::ui::theme::row_height(cx) * 2.;
        let count = runs.len();
        let live = mode == Mode::Live;
        let now = now();
        let (id, scroll) = match mode {
            Mode::Live => ("ci-live", &self.github.live_scroll),
            _ => ("ci-runs", &self.github.scroll),
        };
        v_flex()
            .size_full()
            .child(
                gpui_kit::uniform_list(id, count, {
                    move |range, _window, _cx| {
                        range
                            .map(|row| {
                                let run = &runs[row];
                                let app = entity.clone();
                                let picked = run.clone();
                                let stage = run.stage();
                                let mut second = vec![run.workflow.as_str()];
                                if live {
                                    second.extend([run.branch.as_str(), run.event.as_str()]);
                                }
                                let second = second
                                    .into_iter()
                                    .filter(|part| !part.is_empty())
                                    .collect::<Vec<_>>()
                                    .join(" · ");
                                let right = if run.is_active() {
                                    duration(&run.started_at, None, now)
                                        .unwrap_or_else(|| run.tally().to_string())
                                } else {
                                    run.tally().to_string()
                                };
                                h_flex()
                                    .id(("ci-run", row))
                                    .w_full()
                                    .px_2()
                                    .gap_2()
                                    .items_center()
                                    .h(row_height)
                                    .when(chosen.as_deref() == Some(run.id.as_str()), |el| {
                                        el.bg(selected)
                                    })
                                    .hover(|el| el.bg(hovered))
                                    .child(
                                        div()
                                            .text_color(palette.stage(stage))
                                            .child(icon(stage.glyph()).xsmall()),
                                    )
                                    .child(
                                        v_flex()
                                            .flex_1()
                                            .min_w_0()
                                            .child(
                                                div()
                                                    .truncate()
                                                    .text_sm()
                                                    .child(SharedString::from(run.title.clone())),
                                            )
                                            .child(
                                                div()
                                                    .truncate()
                                                    .text_xs()
                                                    .text_color(palette.muted)
                                                    .child(SharedString::from(second)),
                                            ),
                                    )
                                    .child(
                                        div()
                                            .text_xs()
                                            .text_color(palette.muted)
                                            .child(SharedString::from(right)),
                                    )
                                    .on_click(move |_, _window, cx| {
                                        let picked = picked.clone();
                                        app.update(cx, |this, cx| this.choose_ci_run(picked, cx));
                                    })
                                    .into_any_element()
                            })
                            .collect()
                    }
                })
                .track_scroll(scroll)
                .flex_1()
                .min_h_0(),
            )
            .children(self.render_run_detail(cx))
            .into_any_element()
    }

    /// What last failed, under the bar.
    ///
    /// A strip of its own and no longer the panel's whole message: a pull
    /// request that would not open must not take the runs off the screen, and
    /// `gh` refusing says why in a sentence one has to be able to read while
    /// looking at what one asked for.
    fn render_github_error(&mut self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let why = self.github.error.clone()?;
        Some(
            div()
                .w_full()
                .px_2()
                .py_1()
                .text_xs()
                .text_color(cx.theme().danger)
                .child(why)
                .into_any_element(),
        )
    }

    pub(super) fn render_github(
        &mut self,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let bar = self.render_github_bar(cx);
        let find = self.render_find(Pane::Github, cx);
        let error = self.render_github_error(cx);
        let body = match self.github.mode {
            Mode::Prs => self.render_pr_list(cx),
            mode => self.render_run_list(mode, cx),
        };
        v_flex()
            .size_full()
            .child(bar)
            .children(find)
            .children(error)
            .child(body)
    }

    /// What the chosen run offers: its gestures, its jobs, its log. It only
    /// appears once a run is chosen: an empty section would push the list out
    /// of sight to say nothing.
    fn render_run_detail(&mut self, cx: &mut Context<Self>) -> Option<impl IntoElement> {
        let run = self.github.chosen_run()?.clone();
        let log = self.github.log.clone();
        let mono = cx.theme().mono_font_family.clone();
        let (muted, border, secondary) = (
            cx.theme().muted_foreground,
            cx.theme().border,
            cx.theme().secondary,
        );
        let busy = self.github.gesture_call != 0 && self.github.gesture.is_some();
        let now = now();
        let mut facts = vec![format!("{} — {}", run.workflow, run.tally())];
        facts.extend(
            [run.branch.clone(), run.event.clone()]
                .into_iter()
                .filter(|part| !part.is_empty()),
        );
        if let Some(elapsed) = run
            .is_active()
            .then(|| duration(&run.started_at, None, now))
            .flatten()
        {
            facts.push(elapsed);
        }
        let jobs = self.render_jobs(&run, cx);
        Some(
            v_flex()
                .gap_1()
                .p_2()
                .border_t_1()
                .border_color(border)
                .child(
                    div()
                        .text_sm()
                        .truncate()
                        .child(SharedString::from(run.title.clone())),
                )
                .child(
                    div()
                        .text_xs()
                        .truncate()
                        .text_color(muted)
                        .child(SharedString::from(facts.join(" · "))),
                )
                .child(
                    h_flex()
                        .flex_wrap()
                        .gap_1()
                        .when(run.stage() == Stage::Failed, |el| {
                            el.child(
                                Button::new("ci-hand")
                                    .primary()
                                    .small()
                                    .icon(icon("bot"))
                                    .label(tr!("ci-hand"))
                                    .on_click(cx.listener(|this, _, _window, cx| {
                                        this.read_ci_log(true, cx)
                                    })),
                            )
                            .child(
                                Button::new("ci-log")
                                    .ghost()
                                    .small()
                                    .icon(icon("file-text"))
                                    .label(tr!("ci-log"))
                                    .on_click(cx.listener(|this, _, _window, cx| {
                                        this.read_ci_log(false, cx)
                                    })),
                            )
                        })
                        .when(run.is_active(), |el| {
                            el.child(
                                Button::new("ci-cancel")
                                    .ghost()
                                    .small()
                                    .icon(icon("circle-stop"))
                                    .label(tr!("ci-cancel"))
                                    .disabled(busy)
                                    .on_click(cx.listener(|this, _, window, cx| {
                                        this.confirm_run_gesture(Gesture::Cancel, window, cx)
                                    })),
                            )
                        })
                        .when(run.can_rerun(), |el| {
                            el.child(
                                Button::new("ci-rerun")
                                    .ghost()
                                    .small()
                                    .icon(icon("refresh-cw"))
                                    .label(tr!("ci-rerun"))
                                    .disabled(busy)
                                    .on_click(cx.listener(|this, _, window, cx| {
                                        this.confirm_run_gesture(Gesture::Rerun, window, cx)
                                    })),
                            )
                        })
                        .when(!run.url.is_empty(), |el| {
                            el.child(
                                Button::new("ci-web")
                                    .ghost()
                                    .small()
                                    .icon(icon("external-link"))
                                    .tooltip(tr!("ci-web"))
                                    .on_click(cx.listener(|this, _, _window, cx| {
                                        this.open_run_in_browser(cx)
                                    })),
                            )
                        }),
                )
                .child(jobs)
                .when_some(log, |el, log| {
                    el.child(
                        div()
                            .id("ci-log")
                            .max_h(gpui_kit::px(220.))
                            .overflow_scroll()
                            .p_2()
                            .rounded_md()
                            .bg(secondary)
                            .font_family(mono)
                            .text_xs()
                            .child(log),
                    )
                }),
        )
    }

    /// The chosen run's jobs, each with its steps under it when unfolded.
    ///
    /// Not virtualised: a run has tens of jobs, and only the running and the
    /// failed ones open by themselves — the list is bounded by what one
    /// unfolds, and it scrolls within its own height.
    fn render_jobs(&mut self, run: &Run, cx: &mut Context<Self>) -> AnyElement {
        let palette = Palette::of(cx);
        let hovered = cx.theme().secondary;
        if self.github.jobs_for.as_deref() != Some(run.id.as_str()) {
            return div().into_any_element();
        }
        if self.github.jobs.is_empty() {
            let note = if self.github.jobs_loading {
                tr!("ci-jobs-loading")
            } else {
                tr!("ci-jobs-empty")
            };
            return div()
                .text_xs()
                .text_color(palette.muted)
                .child(note)
                .into_any_element();
        }
        let now = now();
        let mut rows: Vec<AnyElement> = Vec::new();
        for job in &self.github.jobs {
            let open = self.github.job_open(job);
            let id = job.id;
            let stage = job.stage();
            let took = duration(&job.started_at, end_of(&job.status, &job.completed_at), now)
                .unwrap_or_default();
            rows.push(
                h_flex()
                    .id(("ci-job", id))
                    .gap_1()
                    .px_1()
                    .items_center()
                    .text_xs()
                    .cursor_pointer()
                    .hover(|el| el.bg(hovered))
                    .on_click(cx.listener(move |this, _, _window, cx| this.toggle_job(id, cx)))
                    .child(
                        icon(match (job.steps.is_empty(), open) {
                            (true, _) => "dash",
                            (false, true) => "chevron-down",
                            (false, false) => "chevron-right",
                        })
                        .xsmall(),
                    )
                    .child(
                        div()
                            .text_color(palette.stage(stage))
                            .child(icon(stage.glyph()).xsmall()),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .child(SharedString::from(job.name.clone())),
                    )
                    .child(
                        div()
                            .text_color(palette.muted)
                            .child(SharedString::from(took)),
                    )
                    .into_any_element(),
            );
            if !open {
                continue;
            }
            for step in &job.steps {
                let stage = step.stage();
                let took = duration(
                    &step.started_at,
                    end_of(&step.status, &step.completed_at),
                    now,
                )
                .unwrap_or_default();
                rows.push(
                    h_flex()
                        .gap_1()
                        .pl_6()
                        .pr_1()
                        .items_center()
                        .text_xs()
                        .child(
                            div()
                                .text_color(palette.stage(stage))
                                .child(icon(stage.glyph()).xsmall()),
                        )
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .truncate()
                                .when(stage == Stage::Skipped || stage == Stage::Waiting, |el| {
                                    el.text_color(palette.muted)
                                })
                                .child(SharedString::from(step.name.clone())),
                        )
                        .child(
                            div()
                                .text_color(palette.muted)
                                .child(SharedString::from(took)),
                        )
                        .into_any_element(),
                );
            }
        }
        div()
            .id("ci-jobs")
            .max_h(gpui_kit::px(260.))
            .overflow_y_scroll()
            .children(rows)
            .into_any_element()
    }

    fn render_github_bar(&mut self, cx: &mut Context<Self>) -> impl IntoElement {
        let mode = self.github.mode;
        let busy = self.github.loading || self.github.pr_loading || self.github.live_loading;
        let count = match mode {
            Mode::Prs => tr!("github-pr-count", { n: self.github.prs.len() }),
            Mode::Runs => tr!("ci-count", { n: self.github.runs.len() }),
            Mode::Live => tr!("ci-live-count", { n: self.github.live.len() }),
        };
        // The button is offered only where it would work: on a branch, and one
        // that has nothing open yet. A second pull request from the same head
        // is what `gh` refuses, and a refusal one could have foreseen is a
        // refusal one should not have been shown.
        let can_create = self.branch_here().is_some() && self.branch_pr().is_none();
        let mode_button = |id: &'static str,
                           glyph: &'static str,
                           tip: SharedString,
                           target: Mode| {
            Button::new(id)
                .ghost()
                .small()
                .icon(icon(glyph))
                .tooltip(tip)
                .selected(mode == target)
                .on_click(cx.listener(move |this, _, _window, cx| this.set_github_mode(target, cx)))
        };
        h_flex()
            .h(crate::ui::theme::bar_height(cx))
            .w_full()
            .px_2()
            .gap_1()
            .items_center()
            .border_b_1()
            .border_color(cx.theme().border)
            .child(icon("github").xsmall())
            .child(mode_button(
                "github-mode-prs",
                "git-pull-request",
                tr!("github-mode-prs"),
                Mode::Prs,
            ))
            .child(mode_button(
                "github-mode-runs",
                "play",
                tr!("github-mode-runs"),
                Mode::Runs,
            ))
            .child(mode_button(
                "github-mode-live",
                "zap",
                tr!("github-mode-live"),
                Mode::Live,
            ))
            .child(
                div()
                    .flex_1()
                    .truncate()
                    .text_xs()
                    .text_color(cx.theme().muted_foreground)
                    .child(count),
            )
            .when(can_create, |el| {
                el.child(
                    Button::new("github-pr-create")
                        .ghost()
                        .small()
                        .icon(icon("plus"))
                        .tooltip(tr!("github-pr-create"))
                        .disabled(self.github.creating)
                        .on_click(
                            cx.listener(|this, _, window, cx| this.prompt_new_pr(window, cx)),
                        ),
                )
            })
            .child(self.find_button(Pane::Github, cx))
            .child(
                Button::new("ci-refresh")
                    .ghost()
                    .small()
                    .icon(icon("refresh-cw"))
                    .tooltip(tr!("action-refresh"))
                    .disabled(busy)
                    .on_click(cx.listener(|this, _, _window, cx| this.load_github(cx))),
            )
    }
}

/// The pull request being written, while the dialog is open.
///
/// **An entity of its own and not a field of `ClaudhubApp`**: the closure
/// `open_dialog` keeps is called back on every frame, from the root view's
/// render, that is in the middle of a borrow of the application — touching it
/// there panics. It is `TagDraft`'s pattern.
pub struct PrDraft {
    pub title: Entity<InputState>,
    pub body: Entity<TextareaState>,
    /// Opened as a draft. **Unchecked by default**: what one opens from here is
    /// a branch one has just finished reviewing, and a draft is the exception
    /// worth a second click rather than the rule worth an extra one every time.
    pub draft: bool,
    /// The branch it targets, `None` when that is left to `gh` — the
    /// repository's default.
    pub base: Option<SharedString>,
    /// The branch has never been published, so opening the pull request pushes
    /// it. Said here because it is a second thing the button does.
    pub push: bool,
}

impl Render for PrDraft {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let muted = cx.theme().muted_foreground;
        v_flex()
            .w(gpui_kit::px(560.))
            .gap_2()
            .children(self.base.clone().map(|base| {
                div()
                    .text_xs()
                    .text_color(muted)
                    .child(tr!("github-new-base", { base: base }))
            }))
            .child(Input::new(&self.title))
            .child(Textarea::new(&self.body))
            .child(
                Checkbox::new("github-new-draft")
                    .label(tr!("github-new-draft"))
                    .checked(self.draft)
                    .on_click(cx.listener(|this, _, _window, cx| {
                        this.draft = !this.draft;
                        cx.notify();
                    })),
            )
            .when(self.push, |el| {
                el.child(
                    div()
                        .text_xs()
                        .text_color(muted)
                        .child(tr!("github-new-push")),
                )
            })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The answer `gh` actually gives, cut down to one pull request: two shapes
    /// of check under one array, and a skipped job among them.
    const ANSWER: &str = r#"[{
        "baseRefName": "master",
        "headRefName": "wt/add-multi-articles-offer",
        "isCrossRepository": false,
        "isDraft": false,
        "mergeable": "MERGEABLE",
        "number": 86,
        "reviewDecision": "",
        "statusCheckRollup": [
            {"__typename": "CheckRun", "conclusion": "SUCCESS", "name": "phplint", "status": "COMPLETED",
             "workflowName": "CI", "detailsUrl": "https://github.com/Acetics/Acetics/actions/runs/1/job/2"},
            {"__typename": "CheckRun", "conclusion": "FAILURE", "name": "phpstan", "status": "COMPLETED"},
            {"__typename": "CheckRun", "conclusion": "SKIPPED", "name": "browser", "status": "COMPLETED"},
            {"__typename": "CheckRun", "conclusion": "", "name": "tests", "status": "IN_PROGRESS"},
            {"__typename": "StatusContext", "context": "ci/legacy", "state": "PENDING",
             "targetUrl": "https://ci.example/1"}
        ],
        "author": {"login": "catvert"},
        "title": "Wt/add multi articles offer",
        "url": "https://github.com/Acetics/Acetics/pull/86"
    }]"#;

    #[test]
    fn the_pull_request_is_read_off_the_answer() {
        let pr = parse_prs(ANSWER).unwrap().pop().expect("a pull request");
        assert_eq!(pr.number, 86);
        assert_eq!(pr.base_ref_name, "master");
        assert!(!pr.is_draft);
        assert!(!pr.conflicts());
        // Empty is "nobody was asked", which has nothing to say.
        assert_eq!(pr.review_decision, "");
        let head = pr.head();
        assert_eq!(head.local_branch(), "wt/add-multi-articles-offer");
        assert_eq!(head.review_base(), "origin/master");
    }

    /// A skipped job is not a failure — this repository's own workflow skips
    /// its browser tests — and the two shapes of check are counted as one.
    #[test]
    fn the_checks_are_tallied_across_both_shapes() {
        let pr = parse_prs(ANSWER).unwrap().pop().unwrap();
        let checks = pr.checks();
        assert_eq!(checks.passed, 2);
        assert_eq!(checks.failed, 1);
        assert_eq!(checks.running, 2);
    }

    /// Unfolded, each check says its name and where it is read, whichever of
    /// the two shapes it came in — and a skipped one is not drawn as passed.
    #[test]
    fn a_check_names_itself_and_its_link_in_either_shape() {
        let pr = parse_prs(ANSWER).unwrap().pop().unwrap();
        let checks = &pr.status_check_rollup;
        assert_eq!(checks[0].label(), "phplint");
        assert_eq!(checks[0].workflow_name, "CI");
        assert!(checks[0].link().ends_with("/job/2"));
        assert_eq!(checks[0].stage(), Stage::Passed);
        assert_eq!(checks[2].stage(), Stage::Skipped);
        assert_eq!(checks[3].stage(), Stage::Running);
        assert_eq!(checks[4].label(), "ci/legacy");
        assert_eq!(checks[4].link(), "https://ci.example/1");
        assert_eq!(checks[4].stage(), Stage::Waiting);
    }

    /// `null` where a string is expected fails a whole struct — and with it
    /// the whole list — unless it is read as an empty string.
    #[test]
    fn a_null_does_not_empty_the_list() {
        let answer = r#"[{"number": 3, "title": "t", "url": "u", "isDraft": true,
            "baseRefName": "main", "headRefName": "fix", "isCrossRepository": true,
            "mergeable": null, "reviewDecision": null, "author": {"login": null},
            "statusCheckRollup": [{"__typename": "CheckRun", "name": "x", "status": "QUEUED",
                "conclusion": null, "detailsUrl": null}]}]"#;
        let pr = parse_prs(answer).expect("nulls read").pop().unwrap();
        assert!(pr.is_cross_repository);
        assert_eq!(pr.head().local_branch(), "pr/3");
        assert_eq!(pr.status_check_rollup[0].stage(), Stage::Waiting);
    }

    /// No pull request is an answer, and not a broken read.
    #[test]
    fn an_empty_answer_is_no_pull_request() {
        assert!(parse_prs("[]").unwrap().is_empty());
        assert!(parse_prs("  ").unwrap().is_empty());
        assert!(parse_prs("not json at all").is_err());
    }

    /// The list is the repository's and carries who opened each one: it is what
    /// the panel is for, and reading one branch at a time is what left it empty
    /// on every branch that has no pull request of its own. It also says which
    /// come from a fork, which is what opening one in a worktree reads.
    #[test]
    fn the_listing_asks_for_every_open_pull_request() {
        let command = pr_list_command();
        assert!(command.contains("--state open"), "{command}");
        assert!(command.contains("author"), "{command}");
        assert!(command.contains("isCrossRepository"), "{command}");
        assert!(!command.contains("--head"), "{command}");
    }

    #[test]
    fn the_filter_reads_the_title_the_branch_the_author_and_the_number() {
        let pr = parse_prs(ANSWER).unwrap().pop().unwrap();
        assert!(pr.matches("multi articles"));
        assert!(pr.matches("wt/add"));
        assert!(pr.matches("catvert"));
        assert!(pr.matches("86"));
        assert!(!pr.matches("nothing of the sort"));
    }

    /// The trap that would open a pull request against a branch that does not
    /// exist: the branches this window is built for carry slashes.
    #[test]
    fn only_the_remote_prefix_is_taken_off_the_base() {
        assert_eq!(base_for_gh("origin/dev"), "dev");
        assert_eq!(base_for_gh("wt/dev-2"), "wt/dev-2");
        assert_eq!(base_for_gh("dev"), "dev");
    }

    #[test]
    fn an_apostrophe_in_the_body_does_not_end_the_command() {
        let command = create_command(Some("dev"), "Fix it", "L'agent l'a écrit", false, false);
        assert!(command.contains(r"'L'\''agent l'\''a écrit'"), "{command}");
        assert!(command.starts_with("gh pr create --base 'dev'"));
        assert!(!command.contains("--draft"));
    }

    /// A branch that has never been published is pushed by the same gesture:
    /// `gh` would ask where to push it, and stdin is closed in a worker.
    #[test]
    fn an_unpublished_branch_is_pushed_first() {
        let command = create_command(None, "Title", "", true, true);
        assert!(command.starts_with("git push --set-upstream origin HEAD && gh pr create"));
        assert!(command.ends_with("--draft"));
        assert!(!command.contains("--base"));
    }

    #[test]
    fn the_dialog_opens_on_what_the_branch_carries() {
        // One commit says everything there is to say.
        assert_eq!(
            draft_from("Fix the thing\n", "wt/thing"),
            ("Fix the thing".into(), String::new())
        );
        // Several: the first names the branch, all of them describe it.
        assert_eq!(
            draft_from("Add the form\nFix its validation\n", "wt/form"),
            (
                "Add the form".into(),
                "- Add the form\n- Fix its validation".to_string()
            )
        );
        // Nothing to list — a branch with no commit of its own yet.
        assert_eq!(
            draft_from("", "wt/empty"),
            ("wt/empty".into(), String::new())
        );
    }

    /// The unfolded run is a copy, refreshed from every list that still has
    /// it — and kept when it leaves one: a run that finishes leaves the live
    /// list at the moment one was watching for.
    #[test]
    fn the_unfolded_run_outlives_the_live_list() {
        let mut state = GithubState {
            mode: Mode::Live,
            live: parse_runs("44\tNew\tCI\tin_progress\t\n"),
            ..Default::default()
        };
        state.chosen = state.live.first().cloned();
        assert!(state.chosen_going());
        // Read again: a newer run first, ours moved to the second rank.
        state.live = parse_runs("45\tNewer\tCI\tqueued\t\n44\tNew\tCI\tin_progress\t\n");
        state.refresh_chosen(Mode::Live);
        assert_eq!(state.chosen_id(), Some("44"));
        // Finished: gone from the list, still unfolded.
        state.live = parse_runs("45\tNewer\tCI\tin_progress\t\n");
        state.refresh_chosen(Mode::Live);
        assert_eq!(
            state.chosen_run().map(|run| run.title.as_str()),
            Some("New")
        );
        // Its own jobs' read says it is over — nothing left to read again.
        if let Some(run) = state.chosen.as_mut() {
            run.status = "completed".into();
            run.conclusion = "success".into();
        }
        assert!(!state.chosen_going());
    }

    /// A change of worktree forgets the readings, not the list one looks at nor
    /// a pull request on its way to its worktree.
    #[test]
    fn a_fresh_state_keeps_the_mode_and_the_landing() {
        let mut state = GithubState {
            mode: Mode::Live,
            chosen: parse_runs("1\tt\tw\tqueued\t\n").pop(),
            landing: Some(Landing {
                main: PathBuf::from("/p"),
                branch: "pr/3".into(),
                base: "origin/main".into(),
                until: Instant::now(),
            }),
            ..Default::default()
        };
        let fresh = state.fresh(Some(PathBuf::from("/p/wt")));
        assert_eq!(fresh.mode, Mode::Live);
        assert!(fresh.landing.is_some());
        assert!(fresh.chosen.is_none());
        assert_eq!(fresh.worktree, Some(PathBuf::from("/p/wt")));
    }
}
