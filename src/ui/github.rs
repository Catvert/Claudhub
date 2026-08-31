//! The branch's pull request and its CI runs, read through `gh`.
//!
//! **`gh` and not GitHub's API**, which is the whole reason this view is cheap:
//! the CLI is already authenticated — it is a program the user installs, like
//! the agents in the terminal — and Claudhub has no token to hold, no OAuth
//! flow to walk and no host to ask about. What it costs is a process per read,
//! on the background queue, which is the `wt` sweep's profile.
//!
//! **Two shapes of reading, and the difference is not a taste.** The runs come
//! back as tab-separated lines, formatted by `gh --template`: a flat list is a
//! format that cannot drift, and the JSON one would parse is one more shape to
//! keep up with. A pull request is not flat — its checks are an array whose
//! entries come in two shapes of their own — and a Go template folding that
//! into a line would be a parser written inside a string. It is read as JSON.
//!
//! One trap is written into the run template and it is not a refinement —
//! `printf "%.0f"` on the id, because a Go template formats a JSON number as a
//! float, so `{{.databaseId}}` gives `3.2494024323e+10`, an id `gh run view`
//! does not recognise, and nothing says so before the first click.

use std::path::PathBuf;

use gpui::{div, prelude::*, AnyElement, Context, Entity, SharedString, Window};
use gpui_component::{
    button::{Button, ButtonVariants as _},
    checkbox::Checkbox,
    h_flex,
    input::{Input, InputState, Textarea, TextareaState},
    v_flex, ActiveTheme, Disableable as _, Selectable as _, Sizable as _, WindowExt as _,
};

use crate::runtime::protocol::Caller;
use crate::runtime::Cmd;
use crate::tr;
use crate::ui::app::ClaudhubApp;
use crate::ui::find::Pane;
use crate::ui::icons::icon;

/// How many runs the list asks for. Twenty is a branch's recent history: past
/// that one is reading the project's, which is what the web page is for.
const LIMIT: usize = 20;

/// How many lines of a log are kept on screen. The **end**, which is where the
/// failure is written.
const TAIL: usize = 80;

/// How many go to the agent — more than one reads, because the agent reads it
/// all and the line that explains is not always the last.
const TAIL_FOR_AGENT: usize = 120;

// — The pull request ————————————————————————————————————————————————

/// The fields of a pull request this panel reads.
///
/// Deserialised straight from `gh --json`, which is why the names are the
/// API's and not ours: a rename here is a `serde(rename)` there, and the pair
/// would drift on the first field added.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PullRequest {
    pub number: u64,
    pub title: String,
    pub url: String,
    pub is_draft: bool,
    pub base_ref_name: String,
    pub head_ref_name: String,
    /// `MERGEABLE`, `CONFLICTING`, or `UNKNOWN` while GitHub works it out.
    #[serde(default)]
    pub mergeable: String,
    /// `APPROVED`, `CHANGES_REQUESTED`, `REVIEW_REQUIRED`, and **empty** when
    /// no review has been asked for — which is not the same as "not reviewed"
    /// and must not be shown as one.
    #[serde(default)]
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
    #[serde(default)]
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
    #[serde(default)]
    pub status: String,
    #[serde(default)]
    pub conclusion: String,
    #[serde(default)]
    pub state: String,
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
         number,title,url,isDraft,baseRefName,headRefName,mergeable,reviewDecision,statusCheckRollup,author"
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

/// The command that opens it in the browser.
pub fn pr_web_command(number: u64) -> String {
    format!("gh pr view {number} --web")
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
fn tally_elements(checks: Checks, colors: [gpui::Hsla; 3]) -> Vec<AnyElement> {
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

/// Single-quotes a value for `sh -c`.
///
/// A pull request's body is written by hand and goes out on a command line:
/// one apostrophe in it, and the rest of the command is read as something else.
fn quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', r"'\''"))
}

// — The runs ————————————————————————————————————————————————————————

/// One run of GitHub Actions.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Run {
    pub id: String,
    pub title: String,
    pub workflow: String,
    /// `completed`, `in_progress`…
    pub status: String,
    /// `success`, `failure`, empty while it runs.
    pub conclusion: String,
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
        match self.conclusion.as_str() {
            "success" => "circle-check",
            "" => "loader-circle",
            _ => "circle-x",
        }
    }

    /// Whether the filter's word is in what the row shows.
    pub fn matches(&self, needle: &str) -> bool {
        let needle = needle.trim().to_lowercase();
        needle.is_empty()
            || self.title.to_lowercase().contains(&needle)
            || self.workflow.to_lowercase().contains(&needle)
    }
}

/// The command that lists them. Written here rather than in the settings: it is
/// a format this module parses, so the two cannot be allowed to drift apart.
///
/// **Filtered by branch**, which is what the panel says it shows and what it
/// did not do: a bare `gh run list` answers with the repository's last twenty
/// runs, so a branch whose own run was pushed out by somebody else's had none
/// on screen and nothing said why. A detached HEAD has no branch to filter on
/// and gets the repository's, which is better than an empty list.
pub fn list_command(branch: Option<&str>) -> String {
    let filter = branch
        .map(|branch| format!(" --branch {}", quote(branch)))
        .unwrap_or_default();
    format!(
        "gh run list{filter} --limit {LIMIT} \
         --json databaseId,displayTitle,workflowName,status,conclusion \
         --template '{{{{range .}}}}{{{{printf \"%.0f\" .databaseId}}}}{{{{\"\\t\"}}}}\
         {{{{.displayTitle}}}}{{{{\"\\t\"}}}}{{{{.workflowName}}}}{{{{\"\\t\"}}}}\
         {{{{.status}}}}{{{{\"\\t\"}}}}{{{{.conclusion}}}}{{{{\"\\n\"}}}}{{{{end}}}}'"
    )
}

/// The command that reads one run's failing log.
pub fn log_command(id: &str) -> String {
    format!("gh run view {id} --log-failed")
}

/// One line per run, five fields apart by a tab — it is `gh` that formats.
///
/// A short line is skipped rather than failing the read: `gh` writes its own
/// notices to stdout on occasion, and one of them must not empty the list.
pub fn parse(out: &str) -> Vec<Run> {
    out.lines()
        .filter_map(|line| {
            let fields: Vec<&str> = line.split('\t').collect();
            if fields.len() < 5 {
                return None;
            }
            Some(Run {
                id: fields[0].to_string(),
                title: fields[1].to_string(),
                workflow: fields[2].to_string(),
                status: fields[3].to_string(),
                conclusion: fields[4].to_string(),
            })
        })
        .collect()
}

/// The end of a log, which is the part the failure is written in.
pub fn tail(text: &str, lines: usize) -> String {
    let all: Vec<&str> = text.lines().collect();
    let start = all.len().saturating_sub(lines);
    all[start..].join("\n")
}

/// Which list the panel shows.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum Mode {
    #[default]
    Prs,
    Runs,
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
    /// Which of the two lists the panel shows. Pull requests: that is what the
    /// panel is for, and the runs are what one goes to **from** one.
    pub mode: Mode,
    pub pr_scroll: gpui::UniformListScrollHandle,
    pub runs: Vec<Run>,
    pub chosen: Option<usize>,
    /// The log of the chosen run, once asked for: choosing a row calls nothing,
    /// the button is what asks.
    pub log: Option<SharedString>,
    /// The worktree this reading was made for. `None` is "never read".
    ///
    /// The same device as `SentryState::read_for`, and simpler for the same
    /// job: what a run list depends on is one checkout and nothing else.
    pub read_for: Option<PathBuf>,
    pub loading: bool,
    pub error: Option<SharedString>,
    /// The sends in flight — a late answer is dropped rather than shown.
    pub pr_call: u64,
    /// The browser opening. Its answer is nothing to show — but a `gh` that
    /// cannot find a browser says so, and an untracked call would swallow it.
    pub web_call: u64,
    pub list_call: u64,
    pub log_call: u64,
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
    pub scroll: gpui::UniformListScrollHandle,
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

    /// Reads the pull request and the runs **the first time the panel is
    /// drawn**, and never before.
    ///
    /// Each is a process and a network round trip of its own, and a checkout
    /// one passes through on the way to another is not a reason for either. See
    /// `ClaudhubApp::ensure_sentry` for the whole of the rule.
    pub(super) fn ensure_github(&mut self, cx: &mut Context<Self>) {
        if self.github.loading || self.github.pr_loading || self.github.read_for == self.active {
            return;
        }
        self.load_github(cx);
    }

    /// Reads them both, replacing whatever was there.
    pub(super) fn load_github(&mut self, cx: &mut Context<Self>) {
        let worktree = self.active.clone();
        let branch = self.branch_here();
        self.github = GithubState {
            worktree: worktree.clone(),
            read_for: worktree.clone(),
            ..Default::default()
        };
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

    /// Shows one list or the other.
    pub(super) fn set_github_mode(&mut self, mode: Mode, cx: &mut Context<Self>) {
        self.github.mode = mode;
        cx.notify();
    }

    /// Opens a pull request in the browser.
    pub(super) fn open_pr_in_browser(&mut self, rank: usize, cx: &mut Context<Self>) {
        let (Some(worktree), Some(pr)) = (self.active.clone(), self.github.prs.get(rank)) else {
            return;
        };
        let command = pr_web_command(pr.number);
        self.github.web_call = self.ask_gh(command, worktree);
        cx.notify();
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

    /// The CI view follows the worktree, like every other panel.
    ///
    /// It **forgets** rather than reads: the next paint of the panel is what
    /// asks for the arriving worktree's runs.
    pub(super) fn github_follows_worktree(&mut self, cx: &mut Context<Self>) {
        if self.github.worktree == self.active {
            return;
        }
        self.github = GithubState {
            worktree: self.active.clone(),
            ..Default::default()
        };
        cx.notify();
    }

    /// Chooses a run. It asks for nothing: a log is a second process, and one
    /// browsing the list would start one per row.
    pub(super) fn choose_ci_run(&mut self, rank: usize, cx: &mut Context<Self>) {
        if self.github.chosen != Some(rank) {
            self.github.log = None;
        }
        self.github.chosen = Some(rank);
        cx.notify();
    }

    /// Asks for the chosen run's failing log — to read, or to hand over.
    pub(super) fn read_ci_log(&mut self, for_agent: bool, cx: &mut Context<Self>) {
        let (Some(worktree), Some(rank)) = (self.active.clone(), self.github.chosen) else {
            return;
        };
        let Some(run) = self.github.runs.get(rank) else {
            return;
        };
        let command = log_command(&run.id);
        self.github.log_for_agent = for_agent;
        self.github.log_call = self.ask_gh(command, worktree);
        cx.notify();
    }

    /// One of the two answers, back from the worker.
    pub(super) fn github_answered(
        &mut self,
        call: u64,
        result: Result<String, String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if call == self.github.list_call {
            self.github.loading = false;
            match result {
                Ok(out) => {
                    self.github.runs = parse(&out);
                    self.github.error = None;
                }
                Err(why) => {
                    self.github.runs = Vec::new();
                    self.github.error = Some(SharedString::from(why));
                }
            }
        } else if call == self.github.pr_call {
            self.github.pr_loading = false;
            match result.and_then(|out| parse_prs(&out)) {
                Ok(prs) => self.github.prs = prs,
                Err(why) => {
                    self.github.prs = Vec::new();
                    self.github.error = Some(SharedString::from(why));
                }
            }
        } else if call == self.github.web_call {
            // Nothing to show on success: what it asked for was a browser.
            if let Err(why) = result {
                self.github.error = Some(SharedString::from(why));
            }
        } else if call == self.github.draft_call {
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
        } else if call == self.github.create_call {
            self.github.creating = false;
            match result {
                // Everything is read back: the pull request that has just been
                // opened, and the runs its push has started.
                Ok(_) => self.load_github(cx),
                Err(why) => self.github.error = Some(SharedString::from(why)),
            }
        } else if call == self.github.log_call {
            match result {
                Ok(text) if self.github.log_for_agent => {
                    self.hand_ci_run(&text, window, cx);
                    self.github.log = Some(SharedString::from(tail(&text, TAIL)));
                }
                Ok(text) => self.github.log = Some(SharedString::from(tail(&text, TAIL))),
                Err(why) => self.github.log = Some(SharedString::from(why)),
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
        let (Some(worktree), Some(rank)) = (self.active.clone(), self.github.chosen) else {
            return;
        };
        let Some(run) = self.github.runs.get(rank).cloned() else {
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
                gpui::uniform_list("github-prs", count, {
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
        let theme = cx.theme();
        let (muted, danger, border) = (theme.muted_foreground, theme.danger, theme.border);
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
                    h_flex().gap_1().child(
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
                .into_any_element(),
        )
    }

    /// The branch's CI runs.
    fn render_run_list(&mut self, cx: &mut Context<Self>) -> AnyElement {
        // **An empty list is not a suspicion of `gh`.** It said so for as long
        // as the list was the repository's, where empty really did mean
        // something was wrong; filtered by branch, empty is the ordinary answer
        // on a project whose workflows only run on a tag. What `gh` missing
        // looks like is a failure, and a failure has its own strip.
        let note = if self.active.is_none() {
            Some(tr!("no-worktree"))
        } else if self.github.loading && self.github.runs.is_empty() {
            Some(tr!("ci-loading"))
        } else if !self.github.runs.is_empty() {
            None
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
        let rows: std::rc::Rc<Vec<usize>> = std::rc::Rc::new(
            self.github
                .runs
                .iter()
                .enumerate()
                .filter(|(_, run)| run.matches(&query))
                .map(|(rank, _)| rank)
                .collect(),
        );
        let runs = std::rc::Rc::new(self.github.runs.clone());
        let chosen = self.github.chosen;
        let entity = cx.entity();
        let theme = cx.theme();
        let (muted, selected, hovered) = (theme.muted_foreground, theme.accent, theme.secondary);
        let row_height = crate::ui::theme::row_height(cx) * 2.;
        let count = rows.len();
        v_flex()
            .size_full()
            .child(
                gpui::uniform_list("ci-runs", count, {
                    let rows = rows.clone();
                    move |range, _window, _cx| {
                        range
                            .map(|row| {
                                let rank = rows[row];
                                let run = &runs[rank];
                                let app = entity.clone();
                                h_flex()
                                    .id(("ci-run", rank))
                                    .w_full()
                                    .px_2()
                                    .gap_2()
                                    .items_center()
                                    .h(row_height)
                                    .when(chosen == Some(rank), |el| el.bg(selected))
                                    .hover(|el| el.bg(hovered))
                                    .child(icon(run.glyph()).xsmall())
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
                                                div().truncate().text_xs().text_color(muted).child(
                                                    SharedString::from(run.workflow.clone()),
                                                ),
                                            ),
                                    )
                                    .child(
                                        div()
                                            .text_xs()
                                            .text_color(muted)
                                            .child(SharedString::from(run.tally().to_string())),
                                    )
                                    .on_click(move |_, _window, cx| {
                                        app.update(cx, |this, cx| this.choose_ci_run(rank, cx));
                                    })
                                    .into_any_element()
                            })
                            .collect()
                    }
                })
                .track_scroll(&self.github.scroll)
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
            Mode::Runs => self.render_run_list(cx),
        };
        v_flex()
            .size_full()
            .child(bar)
            .children(find)
            .children(error)
            .child(body)
    }

    /// What the chosen run offers. It only appears once a row is chosen: an
    /// empty section would push the list out of sight to say nothing.
    fn render_run_detail(&mut self, cx: &mut Context<Self>) -> Option<impl IntoElement> {
        let run = self.github.runs.get(self.github.chosen?)?.clone();
        let log = self.github.log.clone();
        let mono = cx.theme().mono_font_family.clone();
        Some(
            v_flex()
                .gap_1()
                .p_2()
                .border_t_1()
                .border_color(cx.theme().border)
                .child(
                    div()
                        .text_sm()
                        .truncate()
                        .child(SharedString::from(run.title.clone())),
                )
                .child(
                    div()
                        .text_xs()
                        .text_color(cx.theme().muted_foreground)
                        .child(SharedString::from(format!(
                            "{} — {}",
                            run.workflow,
                            run.tally()
                        ))),
                )
                .child(
                    h_flex()
                        .gap_1()
                        .child(
                            Button::new("ci-hand")
                                .primary()
                                .small()
                                .icon(icon("bot"))
                                .label(tr!("ci-hand"))
                                .on_click(
                                    cx.listener(|this, _, _window, cx| this.read_ci_log(true, cx)),
                                ),
                        )
                        .child(
                            Button::new("ci-log")
                                .ghost()
                                .small()
                                .icon(icon("file-text"))
                                .label(tr!("ci-log"))
                                .on_click(
                                    cx.listener(|this, _, _window, cx| this.read_ci_log(false, cx)),
                                ),
                        ),
                )
                .when_some(log, |el, log| {
                    el.child(
                        div()
                            .id("ci-log")
                            .max_h(gpui::px(220.))
                            .overflow_scroll()
                            .p_2()
                            .rounded_md()
                            .bg(cx.theme().secondary)
                            .font_family(mono)
                            .text_xs()
                            .child(log),
                    )
                }),
        )
    }

    fn render_github_bar(&mut self, cx: &mut Context<Self>) -> impl IntoElement {
        let mode = self.github.mode;
        let busy = self.github.loading || self.github.pr_loading;
        let count = match mode {
            Mode::Prs => tr!("github-pr-count", { n: self.github.prs.len() }),
            Mode::Runs => tr!("ci-count", { n: self.github.runs.len() }),
        };
        // The button is offered only where it would work: on a branch, and one
        // that has nothing open yet. A second pull request from the same head
        // is what `gh` refuses, and a refusal one could have foreseen is a
        // refusal one should not have been shown.
        let can_create = self.branch_here().is_some() && self.branch_pr().is_none();
        h_flex()
            .h(crate::ui::theme::bar_height(cx))
            .w_full()
            .px_2()
            .gap_1()
            .items_center()
            .border_b_1()
            .border_color(cx.theme().border)
            .child(icon("github").xsmall())
            .child(
                Button::new("github-mode-prs")
                    .ghost()
                    .small()
                    .icon(icon("git-pull-request"))
                    .tooltip(tr!("github-mode-prs"))
                    .selected(mode == Mode::Prs)
                    .on_click(
                        cx.listener(|this, _, _window, cx| this.set_github_mode(Mode::Prs, cx)),
                    ),
            )
            .child(
                Button::new("github-mode-runs")
                    .ghost()
                    .small()
                    .icon(icon("play"))
                    .tooltip(tr!("github-mode-runs"))
                    .selected(mode == Mode::Runs)
                    .on_click(
                        cx.listener(|this, _, _window, cx| this.set_github_mode(Mode::Runs, cx)),
                    ),
            )
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
            .w(gpui::px(560.))
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

    #[test]
    fn a_short_line_is_skipped_rather_than_failing_the_read() {
        let out = "42\tFix the thing\tCI\tcompleted\tsuccess\n\
                   oops, a notice\n\
                   43\tAnother\tRelease\tin_progress\t\n";
        let runs = parse(out);
        assert_eq!(runs.len(), 2);
        assert_eq!(runs[0].id, "42");
        assert_eq!(runs[0].tally(), "success");
        assert_eq!(runs[0].glyph(), "circle-check");
        // In flight: the status stands in, and the glyph says "not yet" rather
        // than "not well".
        assert_eq!(runs[1].tally(), "in_progress");
        assert_eq!(runs[1].glyph(), "loader-circle");
    }

    #[test]
    fn a_failure_draws_the_cross() {
        let runs = parse("7\tt\tw\tcompleted\tfailure\n");
        assert_eq!(runs[0].glyph(), "circle-x");
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
        let command = list_command(None);
        assert!(
            command.contains(r#"printf "%.0f" .databaseId"#),
            "{command}"
        );
        assert!(log_command("42").contains("--log-failed"));
    }

    /// The panel says it shows the branch's runs; a bare `gh run list` shows
    /// the repository's.
    #[test]
    fn the_listing_is_filtered_by_branch_when_there_is_one() {
        assert!(list_command(Some("wt/thing")).contains("--branch 'wt/thing'"));
        assert!(!list_command(None).contains("--branch"));
    }

    /// The answer `gh` actually gives, cut down to one pull request: two shapes
    /// of check under one array, and a skipped job among them.
    const ANSWER: &str = r#"[{
        "baseRefName": "master",
        "headRefName": "wt/add-multi-articles-offer",
        "isDraft": false,
        "mergeable": "MERGEABLE",
        "number": 86,
        "reviewDecision": "",
        "statusCheckRollup": [
            {"__typename": "CheckRun", "conclusion": "SUCCESS", "name": "phplint", "status": "COMPLETED"},
            {"__typename": "CheckRun", "conclusion": "FAILURE", "name": "phpstan", "status": "COMPLETED"},
            {"__typename": "CheckRun", "conclusion": "SKIPPED", "name": "browser", "status": "COMPLETED"},
            {"__typename": "CheckRun", "conclusion": "", "name": "tests", "status": "IN_PROGRESS"},
            {"__typename": "StatusContext", "context": "ci/legacy", "state": "PENDING"}
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

    /// No pull request is an answer, and not a broken read.
    #[test]
    fn an_empty_answer_is_no_pull_request() {
        assert!(parse_prs("[]").unwrap().is_empty());
        assert!(parse_prs("  ").unwrap().is_empty());
        assert!(parse_prs("not json at all").is_err());
    }

    /// The list is the repository's and carries who opened each one: it is what
    /// the panel is for, and reading one branch at a time is what left it empty
    /// on every branch that has no pull request of its own.
    #[test]
    fn the_listing_asks_for_every_open_pull_request() {
        let command = pr_list_command();
        assert!(command.contains("--state open"), "{command}");
        assert!(command.contains("author"), "{command}");
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

    #[test]
    fn the_filter_reads_the_title_and_the_workflow() {
        let runs = parse("42\tFix the thing\tCI\tcompleted\tsuccess\n");
        assert!(runs[0].matches("thing"));
        assert!(runs[0].matches("ci"));
        assert!(!runs[0].matches("42"));
        assert!(runs[0].matches(" "));
    }
}
