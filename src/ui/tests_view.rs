//! The tests panels: a worktree's suites — Pest, Vitest, Jest — as one
//! folding tree, and the run being followed.
//!
//! The list is `crate::suite`'s merged reading of every runner the checkout
//! carries, asked on the background queue when a worktree is first looked at
//! and re-asked when a test file changes. A run goes through the tests
//! worker: its lines stream into the run panel while it goes, and the
//! account at the end is what colours each row — green, red, skipped — and
//! teaches the real descriptions Pest's listing only had mangled. The fates
//! are persisted per worktree, so the dots survive a restart. Everything the
//! rows decide — which rows a query leaves, where the folders open, what a
//! run paired with which test — is pure and tested at the bottom.

use std::collections::{HashMap, HashSet, VecDeque};
use std::path::{Path, PathBuf};
use std::rc::Rc;

use gpui_kit::component::{
    button::{Button, ButtonVariants},
    h_flex,
    menu::{ContextMenuExt, PopupMenu, PopupMenuItem},
    spinner::Spinner,
    v_flex, ActiveTheme, Disableable, Selectable, Sizable,
};
use gpui_kit::{div, img, prelude::*, uniform_list, App, Context, Entity, SharedString, Window};

use crate::runtime::Cmd;
use crate::suite::{Handoff, Outcome, Report, Run, Runner, Status, Target, Test};
use crate::tr;
use crate::ui::app::ClaudhubApp;
use crate::ui::find::Pane;
use crate::ui::icons::icon;
use crate::ui::store::{Store, TestMark, TestsRun};

/// What the slow-motion toggle asks for, in milliseconds between actions.
///
/// Playwright waits this long before each action it performs, and only before
/// what it counts as one — a click, a fill, a navigation. Half a second is
/// what makes a form being filled readable without turning a suite of thirty
/// tests into a quarter of an hour.
const SLOW_MO: u32 = 500;

/// Lines of a run kept for the panel. The tail: the failures and the summary
/// are at the end, and a runaway `dump()` loop can print millions.
const RUN_LINES_KEPT: usize = 4000;

/// What the panel knows of one worktree's suite.
#[derive(Default)]
pub struct PestState {
    /// Behind an `Rc`: the row closure runs for every visible row on every
    /// frame and cannot read the application back, so it has to capture the
    /// list. `None`: asked, not answered yet.
    pub report: Option<Rc<Report>>,
    /// A listing has gone out and has not come back. Without this guard, a
    /// burst of saves under `tests/` would boot one PHP per keystroke.
    pub pending: bool,
    /// The suite changed while a listing was out: ask again when it lands,
    /// rather than showing a list already known to be stale.
    pub stale: bool,
    /// Each test's last known fate, by (class, method). Loaded from the
    /// store, rewritten by every run.
    pub marks: HashMap<(String, String), Mark>,
    /// The last run's totals, for the bar.
    pub last_run: Option<TestsRun>,
    /// The folders open in the tree. Empty by default: the tree opens
    /// closed, like the explorer's, and remembers what one opened.
    pub expanded: HashSet<String>,
    /// Show only the tests whose last fate was a failure.
    pub only_failed: bool,
    /// Watch the browser a Pest run drives, streamed into the run panel.
    /// Exclusive with `parallel`: each parallel worker gets a browser of its
    /// own, and they would all be handed the same debugging port.
    ///
    /// Only the checkouts whose `BrowserTestCase` opens that port answer; the
    /// others simply show no picture, and their run is unaffected — which is
    /// why it is on by default.
    pub cast: bool,
    /// Put [`SLOW_MO`] milliseconds between the browser's actions, so the
    /// cast can be followed by eye. Like `headed`, a session choice: one
    /// watches a test, one does not run a suite like that.
    pub slow: bool,
    /// Run Pest with `--headed`: its browser tests then show the browser
    /// instead of running headless. A session choice, not persisted — one
    /// watches a test debug, one does not live like that.
    pub headed: bool,
    /// Run Pest with `--parallel`. Exclusive with `headed`: the browser
    /// plugin refuses the pair, so turning one on turns the other off.
    pub parallel: bool,
    /// Per-row caches aligned with the report's tests, rebuilt when the
    /// report or the marks change: reading two thousand marks through a
    /// `HashMap` on every frame is what this avoids.
    pub statuses: Rc<Vec<Option<Status>>>,
    pub labels: Rc<Vec<SharedString>>,
}

/// One test's last known fate, as the rows read it.
#[derive(Debug, Clone, PartialEq)]
pub struct Mark {
    pub status: Status,
    /// Unix seconds of the run that said so.
    pub at: i64,
    /// The real description, learned from the run's account.
    pub name: String,
}

impl PestState {
    /// Rebuilds the per-row caches. Called when the report or the marks
    /// change — never per frame.
    fn refresh_cache(&mut self) {
        let Some(Report::Tests(tests)) = self.report.as_deref() else {
            self.statuses = Rc::new(Vec::new());
            self.labels = Rc::new(Vec::new());
            return;
        };
        let mut statuses = Vec::with_capacity(tests.len());
        let mut labels = Vec::with_capacity(tests.len());
        for test in tests {
            let mark = self.marks.get(&(test.class.clone(), test.method.clone()));
            statuses.push(mark.map(|mark| mark.status));
            labels.push(SharedString::from(match mark {
                // The account's description is the real one; the listing's is
                // a mangled reading.
                Some(mark) if !mark.name.is_empty() => mark.name.clone(),
                _ => test.name.clone(),
            }));
        }
        self.statuses = Rc::new(statuses);
        self.labels = Rc::new(labels);
    }
}

/// Frees the texture a frame uploaded to the atlas. Nothing else does — see
/// [`TestsView::pest_frame`] — and the window has to be named: it is taken out
/// of the app's list while it updates, which is when every one of these runs.
fn drop_cast(cast: Option<Cast>, window: &mut Window, cx: &mut gpui_kit::App) {
    if let Some(cast) = cast {
        cx.drop_image(cast.image, Some(window));
    }
}

/// One frame of the browser a run drives, as the panel holds it.
pub struct Cast {
    /// Decoded, not the JPEG: see [`TestsView::pest_frame`].
    pub image: std::sync::Arc<gpui_kit::RenderImage>,
    /// The browser's own size, which sets the frame's shape: the panel is
    /// rarely that ratio, and a stretched page reads as a broken one.
    pub width: u32,
    pub height: u32,
}

/// A run being followed, or the last one followed.
///
/// "Tout lancer" on a checkout carrying several runners is a **campaign**:
/// one command per runner, queued on the same worker, followed as one — the
/// ids from `since` to `id` all belong to it, their lines append, their
/// accounts merge.
pub struct RunState {
    /// The first send id of the campaign.
    pub since: u64,
    /// The last send id: the campaign ends when its answer lands. A late
    /// answer from before `since` must not paint this panel.
    pub id: u64,
    /// What was launched, as the bar says it — "pest", "vitest src/x"…
    pub label: SharedString,
    /// The campaign's targets, kept for the tree: while the run goes, the
    /// rows a target covers show as loading instead of their last dot.
    pub targets: Vec<Target>,
    pub running: bool,
    /// Unix seconds.
    pub started_at: i64,
    /// The run's text so far, tail-capped at [`RUN_LINES_KEPT`].
    pub lines: VecDeque<SharedString>,
    /// The newest frame of the browser the run drives, when the target asked
    /// to watch it. **One image, replaced in place**: a screencast is watched,
    /// not replayed, and each frame kept would be a decoded texture kept.
    pub cast: Option<Cast>,
    /// Put away for this run: the cast's tab was crossed out. Cleared by the
    /// next run, never by the next frame — see [`TestsView::close_cast`].
    pub cast_hidden: bool,
    /// The same, for the run's own tab. A run outlives what one wanted of it:
    /// once it is read there is nothing to go back to, and the tab stayed
    /// until the next launch because the state it shows is still there. This
    /// is what crossing it out says, and the next run clears it — a launch
    /// wants its own output in front.
    pub hidden: bool,
    /// The rows this campaign has already settled, by `(class, method)`.
    /// A covered row spins until it lands here — which is what makes the tree
    /// resolve test by test rather than all at once at the end.
    pub settled: HashSet<(String, String)>,
    /// The tests the live channel has named, in the order it named them.
    ///
    /// **The run's only account of itself while it goes.** Pest prints per
    /// **file**: a file of browser tests says nothing for a minute, then
    /// everything at once — measured, thirty seconds of silence for three
    /// tests. These come from the TeamCity log `pump_steps` polls, the same
    /// channel that lights the tree's dots, and they are shown under whatever
    /// the runner has printed so far.
    ///
    /// Dropped when the run lands with an account, kept when it does not: the
    /// runner's own lines say the same tests over again, with their durations
    /// and their totals, so leaving both would show each test twice. A run
    /// stopped or refused has no such lines, and then this is all there is.
    pub live: Vec<LiveStep>,
    /// Why a suite never started, when one did not.
    pub error: Option<SharedString>,
    /// The finished accounts so far, merged.
    pub run: Option<Rc<Run>>,
}

/// One test the live channel has named, and what became of it.
///
/// `status` is `None` between `testStarted` and `testFinished` — the test
/// under way, and the only thing on screen that says which one takes the
/// minute one is waiting through.
pub struct LiveStep {
    pub key: (String, String),
    pub label: SharedString,
    pub status: Option<Status>,
}

/// Files what the live channel just said, in place if the test is already
/// there — `testStarted` writes the row, `testFinished` gives it its fate.
///
/// Pure, and the whole rule: a test is named twice per run, and the second
/// word must land on the first row rather than under it.
pub fn note_step(
    live: &mut Vec<LiveStep>,
    key: (String, String),
    label: SharedString,
    status: Option<Status>,
) {
    if let Some(step) = live.iter_mut().find(|step| step.key == key) {
        // A row collapsing dataset cases lands here once per case: within one
        // run, red stays red — the tree's own rule, for the same reason.
        if step.status != Some(Status::Failed) {
            step.status = status;
        }
        return;
    }
    live.push(LiveStep { key, label, status });
}

/// Does a change to this file call for re-reading the suites?
///
/// The tests themselves — `tests/**.php` for Pest, `*.test.*` and `*.spec.*`
/// wherever they live for JS — and the files that decide what a suite is:
/// `phpunit.xml`, the runners' configs, and the two manifests through which a
/// runner arrives and leaves. Not every file of the project: the watcher
/// fires on each save, and a listing boots PHP or node.
pub fn reloads(worktree: &Path, path: &Path) -> bool {
    let Ok(rel) = path.strip_prefix(worktree) else {
        return false;
    };
    if matches!(
        rel.to_str(),
        Some("phpunit.xml" | "phpunit.xml.dist" | "composer.json" | "package.json")
    ) {
        return true;
    }
    let name = rel
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or_default();
    // The runners' own configuration, whatever the extension variant.
    if name.starts_with("vitest.config.") || name.starts_with("jest.config.") {
        return true;
    }
    // Pest tests live under `tests/`; JS tests are named, wherever they live.
    if rel.starts_with("tests") && rel.extension().is_some_and(|ext| ext == "php") {
        return true;
    }
    name.contains(".test.") || name.contains(".spec.")
}

/// One row of the tree: a folder, or a test. Both carry an index into the
/// report's tests — the folder, that of the first kept test under it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Row {
    Dir {
        test: usize,
        depth: usize,
        expanded: bool,
        /// Kept tests under this folder, and how many of them are red.
        tests: u32,
        failed: u32,
        /// The folder's own verdict: red the moment one test under it is,
        /// green when every one has run green, nothing while any is still
        /// unknown — a folder must not claim more than its rows do.
        status: Option<Status>,
        /// A running campaign covers something under this folder: the header
        /// shows as loading, so a closed tree still says what runs.
        running: bool,
    },
    Test {
        test: usize,
        depth: usize,
    },
}

/// The folder a dir row names: the first `depth + 1` segments of the short
/// class. This string is also the key `expanded` holds.
pub fn dir_prefix(class: &str, depth: usize) -> String {
    crate::suite::group_prefix(class, depth).to_string()
}

/// Which rows are on screen: the tree, folded and filtered.
///
/// A test stays when the query matches its (real) name or its class, and,
/// with the failures filter on, when its last fate was red. While a query or
/// the filter narrows the list, the folds are ignored — what one asked for
/// must not hide behind a closed folder. A folder left with nothing under it
/// disappears, header included.
pub fn rows(
    tests: &[Test],
    labels: &[SharedString],
    statuses: &[Option<Status>],
    running: &[bool],
    query: &str,
    expanded: &HashSet<String>,
    only_failed: bool,
) -> Vec<Row> {
    let filtering = !query.trim().is_empty() || only_failed;
    let kept = |at: usize, test: &Test| -> bool {
        if only_failed && statuses.get(at).copied().flatten() != Some(Status::Failed) {
            return false;
        }
        let label: &str = labels.get(at).map(|l| l.as_ref()).unwrap_or(&test.name);
        crate::ui::find::matches(query, label)
            || crate::ui::find::matches(query, crate::suite::short_class(&test.class))
    };

    // First pass: how many kept tests under each folder, how many red, how
    // many carry a verdict at all, and whether one is being run right now.
    let mut counts: HashMap<String, (u32, u32, u32, bool)> = HashMap::new();
    for (at, test) in tests.iter().enumerate() {
        if !kept(at, test) {
            continue;
        }
        let status = statuses.get(at).copied().flatten();
        let segments = crate::suite::segments(&test.class).len();
        for depth in 0..segments {
            let entry = counts.entry(dir_prefix(&test.class, depth)).or_default();
            entry.0 += 1;
            entry.1 += u32::from(status == Some(Status::Failed));
            entry.2 += u32::from(status.is_some());
            entry.3 |= running.get(at).copied().unwrap_or(false);
        }
    }

    // Second pass: emit, folding. `chain` is the dir prefixes above the
    // current test, each with whether its content shows.
    let mut rows = Vec::new();
    let mut chain: Vec<(String, bool)> = Vec::new();
    for (at, test) in tests.iter().enumerate() {
        if !kept(at, test) {
            continue;
        }
        let segments = crate::suite::segments(&test.class);
        let prefixes: Vec<String> = (0..segments.len())
            .map(|depth| dir_prefix(&test.class, depth))
            .collect();
        let common = chain
            .iter()
            .zip(&prefixes)
            .take_while(|((held, _), wanted)| held == *wanted)
            .count();
        chain.truncate(common);
        for (depth, prefix) in prefixes.iter().enumerate().skip(common) {
            let above_open = chain.iter().all(|(_, open)| *open);
            let open = filtering || expanded.contains(prefix);
            if above_open {
                let (tests, failed, marked, running) =
                    counts.get(prefix).copied().unwrap_or_default();
                let status = if failed > 0 {
                    Some(Status::Failed)
                } else if tests > 0 && marked == tests {
                    Some(Status::Passed)
                } else {
                    None
                };
                rows.push(Row::Dir {
                    test: at,
                    depth,
                    expanded: open,
                    tests,
                    failed,
                    status,
                    running,
                });
            }
            chain.push((prefix.clone(), open));
        }
        if chain.iter().all(|(_, open)| *open) {
            rows.push(Row::Test {
                test: at,
                depth: segments.len(),
            });
        }
    }
    rows
}

/// Which outcome answers which listed test — (test index, outcome index).
///
/// Pest pairs by class then `same_test` (mangled name against real
/// description); Vitest by file and exact title; a Jest **file** row collects
/// every outcome of its file — several pairs per row, which `absorb_run`
/// folds. This pairing is what puts a dot on a row.
pub fn paired(tests: &[Test], outcomes: &[Outcome]) -> Vec<(usize, usize)> {
    let mut by_class: HashMap<&str, Vec<usize>> = HashMap::new();
    let mut by_file: HashMap<&str, usize> = HashMap::new();
    for (at, test) in tests.iter().enumerate() {
        match test.runner {
            Runner::Jest => {
                by_file.insert(test.method.as_str(), at);
            }
            _ => by_class.entry(test.class.as_str()).or_default().push(at),
        }
    }
    let mut pairs = Vec::new();
    for (o, outcome) in outcomes.iter().enumerate() {
        if let Some(candidates) = by_class.get(outcome.class.as_str()) {
            if let Some(&at) = candidates.iter().find(|&&at| match tests[at].runner {
                Runner::Pest => crate::suite::same_test(&tests[at].method, &outcome.name),
                Runner::Vitest => tests[at].method == outcome.name,
                Runner::Jest => false,
            }) {
                pairs.push((at, o));
                continue;
            }
        }
        if let Some(&at) = by_file.get(outcome.class.as_str()) {
            pairs.push((at, o));
        }
    }
    pairs
}

/// What a narrated line announces, read from the symbol it starts with — the
/// runners' own: `✓`/`PASS` (Pest, Vitest, Jest's `√` on some terminals),
/// `⨯`/`✗`/`✕`/`FAIL`/`●` for the red, `-`/`↓`/`○` for what was put aside.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LineKind {
    Pass,
    Fail,
    Skip,
    Plain,
}

/// One row of the run panel: a line the runner printed, or a test the live
/// channel named while it still runs.
enum RunRow {
    Said(SharedString),
    Live {
        status: Option<Status>,
        label: SharedString,
    },
}

/// The symbol a live row wears — the runner's own, so that one list reads as
/// one list: `line_kind` then colours it without knowing where it came from.
/// The test under way wears the ellipsis, having no fate yet.
fn live_glyph(status: Option<Status>) -> &'static str {
    match status {
        Some(Status::Passed) => "✓",
        Some(Status::Failed) => "⨯",
        Some(Status::Skipped) => "↓",
        None => "…",
    }
}

pub fn line_kind(line: &str) -> LineKind {
    let line = line.trim_start();
    if ["✓", "√", "PASS ", "PASS  "]
        .iter()
        .any(|mark| line.starts_with(mark))
    {
        return LineKind::Pass;
    }
    if ["⨯", "✗", "✕", "×", "FAIL", "●"]
        .iter()
        .any(|mark| line.starts_with(mark))
    {
        return LineKind::Fail;
    }
    // Pest writes `- it is skipped → later`; Vitest `↓ name`, Jest `○ name`.
    if line.starts_with('↓')
        || line.starts_with('○')
        || (line.starts_with("- ") && line.contains('→'))
    {
        return LineKind::Skip;
    }
    LineKind::Plain
}

/// What the copy button on a failure puts in the clipboard: where the test
/// lives and everything it said — the shape one pastes into a bug report or
/// an AI without editing it first.
pub fn outcome_text(outcome: &Outcome) -> String {
    let mut text = format!("{} :: {}", outcome.class, outcome.name);
    if !outcome.file.is_empty() {
        text.push('\n');
        text.push_str(&outcome.file);
        if let Some(line) = outcome.line {
            text.push_str(&format!(":{line}"));
        }
    }
    if !outcome.message.is_empty() {
        text.push_str("\n\n");
        text.push_str(&outcome.message);
    }
    text
}

/// `14:32`, in local time — or nothing readable for a timestamp that is not.
fn clock(at: i64) -> String {
    chrono::DateTime::from_timestamp(at, 0)
        .map(|utc| {
            utc.with_timezone(&chrono::Local)
                .format("%H:%M")
                .to_string()
        })
        .unwrap_or_default()
}

fn now() -> i64 {
    chrono::Local::now().timestamp()
}

// — Handing the red to an agent ————————————————————————————————————————

/// What one red row of the tree hands over: each failed outcome the run at
/// hand pairs with it — a Jest row is a file, and hands every red test in it
/// — or, when that run does not speak of it, the row itself with no text.
///
/// The second case is common and not an error: the dots survive a restart,
/// the accounts do not, and a campaign replaces the one before it.
pub fn row_failures(test: &Test, label: &str, run: Option<&Run>, sail: bool) -> Vec<Handoff> {
    let red: Vec<Handoff> = run
        .map(|run| {
            paired(std::slice::from_ref(test), &run.outcomes)
                .into_iter()
                .map(|(_, outcome)| &run.outcomes[outcome])
                .filter(|outcome| outcome.status == Status::Failed)
                .map(|outcome| crate::suite::handoff_of(Some(test), outcome, sail))
                .collect()
        })
        .unwrap_or_default();
    if red.is_empty() {
        return vec![crate::suite::handoff_unexplained(test, label, sail)];
    }
    red
}

/// Every red row of the tree, in the tree's order.
pub fn tree_failures(
    tests: &[Test],
    labels: &[SharedString],
    statuses: &[Option<Status>],
    run: Option<&Run>,
    sail: bool,
) -> Vec<Handoff> {
    tests
        .iter()
        .enumerate()
        .filter(|(at, _)| statuses.get(*at).copied().flatten() == Some(Status::Failed))
        .flat_map(|(at, test)| {
            let label: &str = labels.get(at).map(|l| l.as_ref()).unwrap_or(&test.name);
            row_failures(test, label, run, sail)
        })
        .collect()
}

/// A run's red, in the account's order — each paired back to its listed
/// test, which is what knows the command that runs it alone.
pub fn run_failures(tests: &[Test], outcomes: &[Outcome], sail: bool) -> Vec<Handoff> {
    let listed: HashMap<usize, usize> = paired(tests, outcomes)
        .into_iter()
        .map(|(test, outcome)| (outcome, test))
        .collect();
    outcomes
        .iter()
        .enumerate()
        .filter(|(_, outcome)| outcome.status == Status::Failed)
        .map(|(at, outcome)| {
            let test = listed.get(&at).and_then(|&test| tests.get(test));
            crate::suite::handoff_of(test, outcome, sail)
        })
        .collect()
}

/// Does the run's printed output speak of this failure alone? Only then does
/// its tail go along: the run of one test — what a click on a red row
/// launches — or a suite where it is the only red, whose runner writes the
/// failure's own report at the end. With a second red in the run, the tail
/// would be about the other one as often as not.
pub fn output_speaks_of(run: &Run, handed: &[Handoff]) -> bool {
    let [alone] = handed else {
        return false;
    };
    if alone.message.is_none() {
        return false;
    }
    let mut red = run
        .outcomes
        .iter()
        .filter(|outcome| outcome.status == Status::Failed);
    matches!(
        (red.next(), red.next()),
        (Some(outcome), None) if outcome.class == alone.class && outcome.name == alone.name
    )
}

impl ClaudhubApp {
    /// Hands failures to the worktree's agent, through the dialog Sentry and
    /// CI use — edited before it goes, delivered by a bracketed paste and a
    /// second send, an agent opened when none runs.
    fn hand_failures(
        &mut self,
        failures: Vec<Handoff>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(worktree) = self.active.clone() else {
            return;
        };
        if failures.is_empty() {
            return;
        }
        let state = self.pest_runs.get(&worktree);
        let output: Vec<String> = match state.and_then(|state| state.run.as_deref()) {
            Some(run) if output_speaks_of(run, &failures) => state
                .map(|state| state.lines.iter().map(|line| line.to_string()).collect())
                .unwrap_or_default(),
            _ => Vec::new(),
        };
        let intro = match failures.len() {
            1 => tr!("tests-agent-intro-one"),
            _ => tr!("tests-agent-intro-many"),
        };
        let text = crate::suite::failure_prompt(&intro, &failures, &output);
        self.confirm_agent_prompt(worktree, text, window, cx);
    }

    /// The listed tests and the last followed run of the worktree being
    /// looked at, and whether its Pest goes through Sail — what every
    /// handing gesture reads.
    fn handing_context(&self) -> Option<(Rc<Report>, Option<Rc<Run>>, bool)> {
        let worktree = self.active.as_deref()?;
        let report = self.pest.get(worktree)?.report.clone()?;
        let run = self
            .pest_runs
            .get(worktree)
            .and_then(|state| state.run.clone());
        // A look at the disk, like the terminal line the menu copies.
        Some((report, run, crate::suite::uses_sail(worktree)))
    }

    /// One red row of the tree.
    fn hand_test(&mut self, test: &Test, label: &str, window: &mut Window, cx: &mut Context<Self>) {
        let Some((_, run, sail)) = self.handing_context() else {
            return;
        };
        let failures = row_failures(test, label, run.as_deref(), sail);
        self.hand_failures(failures, window, cx);
    }

    /// Every red row of the tree.
    fn hand_tree_failures(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some((report, run, sail)) = self.handing_context() else {
            return;
        };
        let Report::Tests(tests) = report.as_ref() else {
            return;
        };
        let Some(state) = self.active.as_deref().and_then(|w| self.pest.get(w)) else {
            return;
        };
        let failures = tree_failures(tests, &state.labels, &state.statuses, run.as_deref(), sail);
        self.hand_failures(failures, window, cx);
    }

    /// One failure of the run panel, or all of them.
    fn hand_run_failures(
        &mut self,
        only: Option<usize>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some((report, Some(run), sail)) = self.handing_context() else {
            return;
        };
        let tests: &[Test] = match report.as_ref() {
            Report::Tests(tests) => tests,
            _ => &[],
        };
        let outcomes = match only {
            Some(at) => match run.outcomes.get(at) {
                Some(outcome) => std::slice::from_ref(outcome),
                None => return,
            },
            None => &run.outcomes[..],
        };
        let failures = run_failures(tests, outcomes, sail);
        self.hand_failures(failures, window, cx);
    }
}

// — Asking, running, landing ————————————————————————————————

impl ClaudhubApp {
    /// Asks for a worktree's suite, once. On the worktree being looked at,
    /// like the justfile and beside it: the panel's visibility depends on the
    /// answer, so waiting for a first paint would wait forever. The saved
    /// fates come back here too — the dots would otherwise be grey every
    /// morning.
    pub(super) fn ensure_pest(&mut self, worktree: &Path, cx: &App) {
        if self.pest.contains_key(worktree) {
            return;
        }
        let mut state = PestState {
            pending: true,
            // On unless one turns it off: a checkout that does not open the
            // port runs exactly as before, and a browser suite one cannot see
            // is what this panel is for.
            cast: true,
            ..Default::default()
        };
        if let Some(saved) = Store::global(cx).worktree(worktree) {
            for mark in &saved.tests {
                state.marks.insert(
                    (mark.class.clone(), mark.method.clone()),
                    Mark {
                        status: mark.status,
                        at: mark.at,
                        name: mark.name.clone(),
                    },
                );
            }
            state.last_run = saved.tests_run.clone();
        }
        self.pest.insert(worktree.to_path_buf(), state);
        self.git.send(Cmd::TestsLoad {
            worktree: worktree.to_path_buf(),
        });
    }

    /// Reads it again — the suite has changed, or the retry button was
    /// pressed. The list is **kept** while the answer travels: a panel that
    /// blinks empty on every save reads as broken.
    pub(super) fn reload_pest(&mut self, worktree: &Path, cx: &App) {
        let Some(state) = self.pest.get_mut(worktree) else {
            self.ensure_pest(worktree, cx);
            return;
        };
        if state.pending {
            state.stale = true;
            return;
        }
        state.pending = true;
        self.git.send(Cmd::TestsLoad {
            worktree: worktree.to_path_buf(),
        });
    }

    pub(super) fn pest_arrived(
        &mut self,
        worktree: PathBuf,
        report: Report,
        cx: &mut Context<Self>,
    ) {
        let state = self.pest.entry(worktree.clone()).or_default();
        state.report = Some(Rc::new(report));
        state.pending = false;
        state.refresh_cache();
        if state.stale {
            state.stale = false;
            state.pending = true;
            self.git.send(Cmd::TestsLoad { worktree });
        }
        cx.notify();
    }

    /// The tab exists only where there is a suite — on everything that is not
    /// a Pest project the honest panel is no panel — except on the tests
    /// screen itself, whose empty state *is* the screen. `Failed` counts as a
    /// suite: Pest is installed and its message is the content.
    pub(super) fn tests_visible(&self) -> bool {
        if !self.panel_visible(crate::ui::panels::TestsPanel::NAME) {
            return false;
        }
        let Some(active) = self.active.as_deref() else {
            return false;
        };
        matches!(
            self.pest
                .get(active)
                .and_then(|state| state.report.as_deref()),
            Some(Report::Tests(_) | Report::Failed(_))
        )
    }

    /// The home screen's centre only offers the run tab once a run exists —
    /// the SQL console's rule.
    pub(super) fn test_run_open(&self) -> bool {
        self.active
            .as_deref()
            .and_then(|worktree| self.pest_runs.get(worktree))
            .is_some_and(|run| !run.hidden)
    }

    /// Puts the followed run away, its tab crossed out.
    ///
    /// The state itself is kept, not dropped: the tree reads the same run for
    /// its dots, and a cross on one tab must not take the verdicts down with
    /// it. What goes is the tab, and it comes back on the next launch, which
    /// is the moment there is something to watch again.
    pub(super) fn close_test_run(&mut self, cx: &mut Context<Self>) {
        if let Some(active) = self.active.clone() {
            if let Some(run) = self.pest_runs.get_mut(&active) {
                run.hidden = true;
            }
        }
        cx.notify();
    }

    /// Is there a browser to watch — a frame arrived for the checkout being
    /// looked at, and its panel not put away.
    pub(super) fn cast_open(&self) -> bool {
        self.active
            .as_deref()
            .and_then(|worktree| self.pest_runs.get(worktree))
            .is_some_and(|run| run.cast.is_some() && !run.cast_hidden)
    }

    /// The cross on the cast's tab: done watching this run.
    ///
    /// The frame itself is kept — freeing its texture asks for the window,
    /// which a tab's cross does not carry — and goes when the next frame
    /// replaces it or the next run takes the slot. Both of those do hold one.
    /// The panel comes back with the next run, never mid-run: closing it is
    /// "not this one", and a run that reopened it two frames later would be
    /// refusing the gesture.
    pub(super) fn close_cast(&mut self, cx: &mut Context<Self>) {
        if let Some(active) = self.active.clone() {
            if let Some(run) = self.pest_runs.get_mut(&active) {
                run.cast_hidden = true;
            }
        }
        cx.notify();
    }

    /// The browser a run drives, at the size of the centre.
    pub(super) fn render_cast(
        &mut self,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) -> gpui_kit::AnyElement {
        let cast = self
            .active
            .as_deref()
            .and_then(|worktree| self.pest_runs.get(worktree))
            .and_then(|run| run.cast.as_ref())
            .map(|cast| (cast.image.clone(), cast.width, cast.height));
        let Some((image, width, height)) = cast else {
            return v_flex()
                .size_full()
                .items_center()
                .justify_center()
                .gap_2()
                .text_color(cx.theme().muted_foreground)
                .child(icon("monitor-play"))
                .child(div().text_sm().px_4().child(tr!("cast-none")))
                .into_any_element();
        };
        div()
            .relative()
            .size_full()
            .bg(cx.theme().secondary)
            .flex()
            .items_center()
            .justify_center()
            .child(
                // `Contain`: a frame is 1280 wide whatever the panel is, and
                // the browser's own ratio is what a responsive test is run at.
                img(image)
                    .w_full()
                    .h_full()
                    .object_fit(gpui_kit::ObjectFit::Contain),
            )
            .child(
                // The viewport the browser reports, which is what a responsive
                // test is really being run at.
                div()
                    .absolute()
                    .bottom_1()
                    .right_2()
                    .text_xs()
                    .text_color(cx.theme().muted_foreground)
                    .child(SharedString::from(format!("{width}×{height}"))),
            )
            .into_any_element()
    }

    /// The (cast, slow, headed, parallel) toggles of the worktree being
    /// looked at.
    fn pest_modes(&self) -> (bool, bool, bool, bool) {
        self.active
            .as_deref()
            .and_then(|worktree| self.pest.get(worktree))
            .map(|state| (state.cast, state.slow, state.headed, state.parallel))
            .unwrap_or((false, false, false, false))
    }

    /// Applies the toggles to a target — Pest only, the other runners never
    /// read the fields.
    fn apply_pest_modes(&self, target: &mut Target) {
        let (cast, slow, headed, parallel) = self.pest_modes();
        let is_pest = target.runner == Runner::Pest;
        target.cast = cast && is_pest;
        target.slow_mo = if slow && is_pest { SLOW_MO } else { 0 };
        target.headed = headed && is_pest;
        target.parallel = parallel && is_pest;
    }

    /// Launches a followed run — one target, or a campaign of several: "run
    /// everything" on a checkout carrying two runners is two commands, queued
    /// on the same worker and followed as one. The run panel comes forward,
    /// and the runs themselves go through the tests worker, never a frame.
    fn launch_tests(
        &mut self,
        label: SharedString,
        mut targets: Vec<Target>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(worktree) = self.active.clone() else {
            return;
        };
        if targets.is_empty() {
            return;
        }
        // The bar's toggles apply to every Pest launch, whatever the gesture
        // that built the target — modes, not per-run choices.
        for target in &mut targets {
            self.apply_pest_modes(target);
        }
        // One campaign at a time per worktree: the worker would only queue a
        // second one, and two accounts racing for the same dots would paint
        // whichever finished last.
        if self.pest_runs.get(&worktree).is_some_and(|run| run.running) {
            return;
        }
        let since = self.pest_run_seq + 1;
        let mut id = since;
        for target in &targets {
            id = self.pest_run_seq + 1;
            self.pest_run_seq = id;
            self.git.send(Cmd::TestsRun {
                worktree: worktree.clone(),
                target: target.clone(),
                id,
            });
        }
        let replaced = self.pest_runs.insert(
            worktree.clone(),
            RunState {
                since,
                id,
                label,
                targets,
                running: true,
                started_at: now(),
                lines: VecDeque::new(),
                cast: None,
                cast_hidden: false,
                hidden: false,
                settled: HashSet::new(),
                live: Vec::new(),
                error: None,
                run: None,
            },
        );
        drop_cast(replaced.and_then(|state| state.cast), window, cx);
        self.travel_to_panel(crate::ui::panels::TestRunPanel::NAME, window, cx);
        cx.notify();
    }

    pub(super) fn pest_line(
        &mut self,
        worktree: PathBuf,
        id: u64,
        line: String,
        cx: &mut Context<Self>,
    ) {
        let Some(state) = self.pest_runs.get_mut(&worktree) else {
            return;
        };
        if id < state.since || id > state.id {
            return;
        }
        if state.lines.len() == RUN_LINES_KEPT {
            state.lines.pop_front();
        }
        state.lines.push_back(SharedString::from(line));
        // The tail, as a terminal shows it: the line that matters is the one
        // just said.
        self.pest_run_scroll.scroll_to_item(
            state.lines.len().saturating_sub(1),
            gpui_kit::ScrollStrategy::Center,
        );
        cx.notify();
    }

    /// The newest picture of the browser a run drives.
    ///
    /// Decoded here, rather than handed to `img()` as JPEG bytes: that path
    /// goes through gpui's asset cache, which decodes off-thread and draws
    /// **nothing** until it is done. A stream of one-shot images therefore
    /// blinks — every frame starts as a hole. A `RenderImage` is drawn the
    /// moment it is set, so the previous frame stays up until this one is
    /// ready to replace it.
    ///
    /// The texture it leaves behind has to be dropped by hand: nothing else
    /// reaches the atlas — `remove_asset` only forgets a decode task, and
    /// `RenderImage` has no `Drop` — and a run is thousands of frames.
    pub(super) fn pest_frame(
        &mut self,
        worktree: PathBuf,
        id: u64,
        frame: crate::suite::Frame,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(state) = self.pest_runs.get_mut(&worktree) else {
            return;
        };
        if id < state.since || id > state.id {
            return;
        }
        let Ok(image) = gpui_kit::Image::from_bytes(gpui_kit::ImageFormat::Jpeg, frame.jpeg)
            .to_image_data(cx.svg_renderer())
        else {
            return;
        };
        let previous = state.cast.replace(Cast {
            image,
            width: frame.width,
            height: frame.height,
        });
        let first = previous.is_none();
        drop_cast(previous, window, cx);
        // The centre comes forward on the run's **first** frame and not on
        // each: the panel is asked for by turning the toggle on, and a tab
        // that took the centre back thirty times a second would be taking it
        // from whatever one moved to since.
        if first {
            self.travel_to_panel(crate::ui::panels::CastPanel::NAME, window, cx);
        }
        cx.notify();
    }

    /// The readable description of a test the live channel names by its
    /// mangled method: what a run's account taught, else what the listing
    /// read, else the method itself — the same fallback the tree's rows make.
    fn live_label(&self, worktree: &Path, key: &(String, String)) -> SharedString {
        let Some(state) = self.pest.get(worktree) else {
            return SharedString::from(key.1.clone());
        };
        if let Some(mark) = state.marks.get(key) {
            if !mark.name.is_empty() {
                return SharedString::from(mark.name.clone());
            }
        }
        if let Some(Report::Tests(tests)) = state.report.as_deref() {
            if let Some(test) = tests
                .iter()
                .find(|test| test.class == key.0 && test.method == key.1)
            {
                return SharedString::from(test.name.clone());
            }
        }
        SharedString::from(key.1.clone())
    }

    /// One test of the running suite, as it starts and as it ends: its dot
    /// the moment it is known, rather than at the end of the run, and its
    /// name in the run panel — which has nothing else to show until the
    /// runner finishes a whole file. See [`RunState::live`].
    ///
    /// The name is left empty on purpose — a live event only carries the
    /// mangled method, and [`PestState::refresh_cache`] falls back to the
    /// listing's reading of it. The run's account, at the end, is what brings
    /// the real description.
    pub(super) fn pest_step(
        &mut self,
        worktree: PathBuf,
        id: u64,
        step: crate::suite::Step,
        cx: &mut Context<Self>,
    ) {
        let key = (step.class, step.method);
        let status = step.status;
        // Read before the borrow that writes it: the readable description
        // lives in the other map, learned from a run's account or from the
        // listing, and a live event carries neither.
        let label = self.live_label(&worktree, &key);
        let follows = match self.pest_runs.get_mut(&worktree) {
            Some(state) if id >= state.since && id <= state.id => {
                let rows = state.lines.len();
                note_step(&mut state.live, key.clone(), label, status);
                Some(rows + state.live.len())
            }
            _ => return,
        };
        // The tail, as `pest_line` keeps it: what the run just said is what
        // one is watching for.
        if let Some(rows) = follows {
            self.pest_run_scroll
                .scroll_to_item(rows.saturating_sub(1), gpui_kit::ScrollStrategy::Center);
        }
        let Some(status) = status else {
            cx.notify();
            return;
        };
        let first = match self.pest_runs.get_mut(&worktree) {
            Some(state) if id >= state.since && id <= state.id => state.settled.insert(key.clone()),
            _ => return,
        };
        let Some(state) = self.pest.get_mut(&worktree) else {
            return;
        };
        let previous = state.marks.get(&key);
        // A row that collapses dataset cases lands here once per case: within
        // one run, red stays red. The account, at the end, says the same —
        // one red case makes the row red — and says it for good.
        if !first && previous.is_some_and(|mark| mark.status == Status::Failed) {
            return;
        }
        // Kept from the run before: a live event only carries the mangled
        // method, and a description already learned beats reading it back.
        let name = previous.map_or(String::new(), |mark| mark.name.clone());
        state.marks.insert(
            key,
            Mark {
                status,
                at: now(),
                name,
            },
        );
        state.refresh_cache();
        cx.notify();
    }

    pub(super) fn pest_ran(
        &mut self,
        worktree: PathBuf,
        id: u64,
        run: Result<Run, String>,
        cx: &mut Context<Self>,
    ) {
        let mut absorb = None;
        if let Some(state) = self.pest_runs.get_mut(&worktree) {
            if id >= state.since && id <= state.id {
                // The campaign ends with its last answer; the earlier ones
                // fold their accounts in as they land.
                if id == state.id {
                    state.running = false;
                }
                match &run {
                    Ok(run) => {
                        // The runner has printed its own reading of these
                        // tests, durations and totals included: keeping both
                        // would say every test twice. See `RunState::live`.
                        state.live.clear();
                        let merged = match state.run.take() {
                            Some(earlier) => Rc::new(merge_runs(&earlier, run)),
                            None => Rc::new(run.clone()),
                        };
                        // The bar's totals are the campaign's, not the last
                        // command's: two runners, one answer.
                        let totals = TestsRun {
                            at: now(),
                            passed: merged.passed,
                            failed: merged.failed,
                            skipped: merged.skipped,
                            duration_ms: merged.duration_ms,
                        };
                        state.run = Some(merged);
                        absorb = Some((run.clone(), totals));
                    }
                    Err(message) => state.error = Some(SharedString::from(message.clone())),
                }
            }
        }
        if let Some((run, totals)) = absorb {
            self.absorb_run(&worktree, &run, totals, cx);
        }
        cx.notify();
    }

    /// What a finished run teaches: each covered test's fate and real name,
    /// the campaign's totals, and all of it into the store — the dots survive
    /// the restart.
    fn absorb_run(&mut self, worktree: &Path, run: &Run, totals: TestsRun, cx: &mut App) {
        let at = totals.at;
        let Some(state) = self.pest.get_mut(worktree) else {
            return;
        };
        if let Some(Report::Tests(tests)) = state.report.as_deref() {
            // Fold the pairs per row first: a Jest file row collects every
            // outcome of its file, and one red case makes the file red.
            let mut fates: HashMap<usize, (Status, String)> = HashMap::new();
            for (test, outcome) in paired(tests, &run.outcomes) {
                let outcome = &run.outcomes[outcome];
                let fate = fates
                    .entry(test)
                    .or_insert((outcome.status, outcome.name.clone()));
                match outcome.status {
                    Status::Failed => fate.0 = Status::Failed,
                    Status::Passed if fate.0 == Status::Skipped => fate.0 = Status::Passed,
                    _ => {}
                }
            }
            for (test, (status, name)) in fates {
                let test = &tests[test];
                // A Jest row is a file: an outcome's title must not rename it.
                let name = match test.runner {
                    Runner::Jest => test.name.clone(),
                    _ => name,
                };
                state.marks.insert(
                    (test.class.clone(), test.method.clone()),
                    Mark { status, at, name },
                );
            }
        }
        state.last_run = Some(totals.clone());
        state.refresh_cache();

        let mut marks: Vec<TestMark> = state
            .marks
            .iter()
            .map(|((class, method), mark)| TestMark {
                class: class.clone(),
                method: method.clone(),
                name: mark.name.clone(),
                status: mark.status,
                at: mark.at,
            })
            .collect();
        // Sorted before writing: a map serialised in a different order makes
        // a file that changes with nothing having changed.
        marks.sort_by(|a, b| (&a.class, &a.method).cmp(&(&b.class, &b.method)));
        let Some(main) = self.main_of(worktree) else {
            return;
        };
        Store::update_global(cx, |store| {
            let saved = store.worktree_mut(worktree, &main);
            saved.tests = marks;
            saved.tests_run = Some(totals);
        });
    }

    /// Forgets every verdict of the worktree — the dots, the bar's totals,
    /// the run panel — here and in the store. What a fresh look asks for
    /// after a big rebase, when yesterday's reds say nothing about today.
    fn reset_tests(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(active) = self.active.clone() else {
            return;
        };
        // Not while a run goes: its account would repaint half of what was
        // just wiped, and the half would read as the whole.
        if self
            .pest_runs
            .get(&active)
            .is_some_and(|state| state.running)
        {
            return;
        }
        drop_cast(
            self.pest_runs.remove(&active).and_then(|state| state.cast),
            window,
            cx,
        );
        if let Some(state) = self.pest.get_mut(&active) {
            state.marks.clear();
            state.last_run = None;
            state.refresh_cache();
        }
        if let Some(main) = self.main_of(&active) {
            Store::update_global(cx, |store| {
                let saved = store.worktree_mut(&active, &main);
                saved.tests = Vec::new();
                saved.tests_run = None;
            });
        }
        cx.notify();
    }

    fn toggle_pest_dir(&mut self, prefix: String, cx: &mut Context<Self>) {
        let Some(active) = self.active.as_deref() else {
            return;
        };
        let Some(state) = self.pest.get_mut(active) else {
            return;
        };
        if !state.expanded.remove(&prefix) {
            state.expanded.insert(prefix);
        }
        cx.notify();
    }

    /// The escape hatch the context menu keeps: the same run, in a terminal
    /// tab — where Ctrl+C exists, and where an interactive suite can ask.
    /// Through a login shell, like a recipe: `php` and `node` are looked up
    /// on the `PATH` the user's shell builds.
    fn run_tests_in_terminal(
        &mut self,
        label: SharedString,
        target: &Target,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(worktree) = self.active.clone() else {
            return;
        };
        let mut target = target.clone();
        self.apply_pest_modes(&mut target);
        self.open_terminal(
            &worktree,
            crate::ui::terminal_view::Launch {
                command: Some((
                    "sh".into(),
                    vec![
                        "-lc".into(),
                        crate::suite::terminal_command(&worktree, &target),
                    ],
                )),
                env: HashMap::new(),
                label,
                agent: false,
                placement: None,
            },
            window,
            cx,
        );
    }
}

/// Two accounts of one campaign, read as one.
fn merge_runs(earlier: &Run, later: &Run) -> Run {
    let mut outcomes = earlier.outcomes.clone();
    outcomes.extend(later.outcomes.iter().cloned());
    Run {
        passed: earlier.passed + later.passed,
        failed: earlier.failed + later.failed,
        skipped: earlier.skipped + later.skipped,
        duration_ms: earlier.duration_ms + later.duration_ms,
        outcomes,
    }
}

// — The tree panel ————————————————————————————————————————

impl ClaudhubApp {
    pub(super) fn render_pest(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let Some(active) = self.active.clone() else {
            return v_flex()
                .size_full()
                .items_center()
                .justify_center()
                .text_color(cx.theme().muted_foreground)
                .child(div().text_sm().child(tr!("no-worktree")))
                .into_any_element();
        };
        self.ensure_pest(&active, cx);
        let state = self.pest.get(&active);
        let pending = state.is_some_and(|state| state.pending);
        let only_failed = state.is_some_and(|state| state.only_failed);
        let report = state.and_then(|state| state.report.clone());
        let tests: Rc<Vec<Test>> = match report.as_deref() {
            Some(Report::Tests(tests)) => Rc::new(tests.clone()),
            Some(Report::Failed(message)) => {
                let message = SharedString::from(message.clone());
                return v_flex()
                    .size_full()
                    .child(self.render_pest_bar(0, pending, only_failed, None, cx))
                    .child(failed_pest(message, cx))
                    .into_any_element();
            }
            Some(Report::Missing) => {
                return v_flex()
                    .size_full()
                    .child(self.render_pest_bar(0, pending, only_failed, None, cx))
                    .child(missing_pest(pending, cx))
                    .into_any_element();
            }
            None => Rc::new(Vec::new()),
        };

        let query = self.query(Pane::Tests, cx);
        let find = self.render_find(Pane::Tests, cx);
        // The mode toggles only show where Pest is: `--headed` and
        // `--parallel` are its words, and the other runners never read them.
        let modes = tests
            .iter()
            .any(|test| test.runner == Runner::Pest)
            .then(|| self.pest_modes());
        let bar = self.render_pest_bar(tests.len(), pending, only_failed, modes, cx);
        // What the campaign under way covers: those rows show as loading —
        // the left panel says what runs, not only the run panel.
        let running: Rc<Vec<bool>> = Rc::new(match self.pest_runs.get(&active) {
            Some(run) if run.running => tests
                .iter()
                .map(|test| {
                    run.targets
                        .iter()
                        .any(|target| crate::suite::covers(target, test))
                        && !run
                            .settled
                            .contains(&(test.class.clone(), test.method.clone()))
                })
                .collect(),
            _ => vec![false; tests.len()],
        });
        let state = self.pest.get(&active);
        let statuses = state.map(|s| s.statuses.clone()).unwrap_or_default();
        let labels = state.map(|s| s.labels.clone()).unwrap_or_default();
        let empty_folds = HashSet::new();
        let expanded = state.map(|s| &s.expanded).unwrap_or(&empty_folds);
        let rows: Rc<Vec<Row>> = Rc::new(rows(
            &tests,
            &labels,
            &statuses,
            &running,
            &query,
            expanded,
            only_failed,
        ));
        if rows.is_empty() {
            return v_flex()
                .size_full()
                .child(bar)
                .children(find)
                .child(empty_pest(&query, pending, only_failed, cx))
                .into_any_element();
        }

        let look = Look::of(cx);
        let entity = cx.entity();
        let scroll = self.pest_scroll.clone();
        let count = rows.len();
        v_flex()
            .size_full()
            .child(bar)
            .children(find)
            .child(
                div().flex_1().min_h_0().child(
                    self.scrolled(
                        "pest-bar",
                        &scroll,
                        crate::ui::motion::Axes::Vertical,
                        window,
                        uniform_list("pest-rows", count, move |visible, _window, _cx| {
                            visible
                                .map(|index| match rows.get(index).copied() {
                                    Some(row @ Row::Dir { .. }) => {
                                        render_dir(index, &tests, row, &look, &entity)
                                    }
                                    Some(row @ Row::Test { .. }) => render_test(
                                        index, &tests, &labels, &statuses, &running, row, &look,
                                        &entity,
                                    ),
                                    None => div().into_any_element(),
                                })
                                .collect::<Vec<_>>()
                        })
                        .size_full()
                        .track_scroll(&scroll.clone()),
                        cx,
                    ),
                ),
            )
            .into_any_element()
    }

    fn render_pest_bar(
        &mut self,
        count: usize,
        pending: bool,
        only_failed: bool,
        modes: Option<(bool, bool, bool, bool)>,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let last = self
            .active
            .as_deref()
            .and_then(|worktree| self.pest.get(worktree))
            .and_then(|state| state.last_run.clone());
        // The red rows, read from the cache the tree itself reads: the
        // button that hands them over is there only when there are some.
        let failing = self
            .active
            .as_deref()
            .and_then(|worktree| self.pest.get(worktree))
            .map_or(0, |state| {
                state
                    .statuses
                    .iter()
                    .filter(|status| **status == Some(Status::Failed))
                    .count()
            });
        let summary = h_flex()
            .flex_1()
            .min_w_0()
            .gap_1()
            .items_center()
            .text_xs()
            .child(
                div()
                    .text_color(cx.theme().muted_foreground)
                    .child(tr!("tests-count", { n: count })),
            )
            .when_some(last, |el, last| {
                // The last run, and when: the question the bar answers is
                // "does it pass, and is that answer fresh".
                el.child(
                    div()
                        .text_color(cx.theme().success)
                        .child(SharedString::from(format!("✓{}", last.passed))),
                )
                .when(last.failed > 0, |el| {
                    el.child(
                        div()
                            .text_color(cx.theme().danger)
                            .child(SharedString::from(format!("⨯{}", last.failed))),
                    )
                })
                .child(
                    div()
                        .text_color(cx.theme().muted_foreground)
                        .child(SharedString::from(clock(last.at))),
                )
            });
        let top = h_flex()
            .h(crate::ui::theme::bar_height(cx))
            .w_full()
            .px_2()
            .gap_1()
            .items_center()
            .border_b_1()
            .border_color(cx.theme().border)
            .child(icon("circle-check").xsmall())
            .child(summary)
            .child(self.find_button(Pane::Tests, cx))
            .child(
                Button::new("pest-only-failed")
                    .ghost()
                    .small()
                    .icon(icon("circle-x"))
                    .tooltip(tr!("tests-only-failed"))
                    .selected(only_failed)
                    .on_click(cx.listener(|this, _, _window, cx| {
                        if let Some(active) = this.active.clone() {
                            if let Some(state) = this.pest.get_mut(&active) {
                                state.only_failed = !state.only_failed;
                            }
                        }
                        cx.notify();
                    })),
            )
            .when(failing > 0, |el| {
                el.child(
                    Button::new("tests-hand-all")
                        .ghost()
                        .small()
                        .icon(icon("bot"))
                        .tooltip(tr!("tests-hand-all", { n: failing }))
                        .on_click(cx.listener(|this, _, window, cx| {
                            this.hand_tree_failures(window, cx);
                        })),
                )
            })
            .child(
                Button::new("tests-reset")
                    .ghost()
                    .small()
                    .icon(icon("eraser"))
                    .tooltip(tr!("tests-reset"))
                    .on_click(cx.listener(|this, _, window, cx| {
                        this.reset_tests(window, cx);
                    })),
            )
            .child(
                Button::new("pest-run-all")
                    .ghost()
                    .small()
                    .icon(icon("play"))
                    .tooltip(tr!("tests-run-all"))
                    .disabled(count == 0)
                    .on_click(cx.listener(|this, _, window, cx| {
                        // Every runner the listing found, one command each —
                        // a campaign, followed as one run.
                        let runners: Vec<Runner> = this
                            .active
                            .as_deref()
                            .and_then(|worktree| this.pest.get(worktree))
                            .and_then(|state| match state.report.as_deref() {
                                Some(Report::Tests(tests)) => Some(tests),
                                _ => None,
                            })
                            .map(|tests| {
                                let mut runners: Vec<Runner> =
                                    tests.iter().map(|test| test.runner).collect();
                                runners.dedup();
                                runners.sort_by_key(|runner| runner.label());
                                runners.dedup();
                                runners
                            })
                            .unwrap_or_default();
                        let label = SharedString::from(
                            runners
                                .iter()
                                .map(|runner| runner.label())
                                .collect::<Vec<_>>()
                                .join(" + "),
                        );
                        let targets = runners.into_iter().map(Target::everything).collect();
                        this.launch_tests(label, targets, window, cx);
                    })),
            )
            .child(
                Button::new("pest-refresh")
                    .ghost()
                    .small()
                    .icon(icon("refresh-cw"))
                    .tooltip(tr!("action-refresh"))
                    .disabled(pending)
                    .on_click(cx.listener(|this, _, _window, cx| {
                        if let Some(active) = this.active.clone() {
                            this.reload_pest(&active, cx);
                        }
                        cx.notify();
                    })),
            );
        // Pest's run modes, on their own line: labelled toggles read better
        // than one more icon squeezed into the bar. `--parallel` excludes the
        // other two — the browser plugin refuses it with `--headed`, and a
        // worker per browser leaves nothing single to watch — so a toggle
        // turns it off rather than launching a run that only errors.
        v_flex()
            .w_full()
            .child(top)
            .when_some(modes, |el, (cast, slow, headed, parallel)| {
                el.child(
                    h_flex()
                        .h(crate::ui::theme::bar_height(cx))
                        .w_full()
                        .px_2()
                        .gap_1()
                        .items_center()
                        .border_b_1()
                        .border_color(cx.theme().border)
                        .child(
                            Button::new("pest-cast")
                                .ghost()
                                .small()
                                .icon(icon("monitor-play"))
                                .label(tr!("tests-cast-label"))
                                .tooltip(tr!("tests-cast"))
                                .selected(cast)
                                .on_click(cx.listener(|this, _, _window, cx| {
                                    if let Some(active) = this.active.clone() {
                                        if let Some(state) = this.pest.get_mut(&active) {
                                            state.cast = !state.cast;
                                            if state.cast {
                                                state.parallel = false;
                                            }
                                        }
                                    }
                                    cx.notify();
                                })),
                        )
                        .child(
                            Button::new("pest-slow")
                                .ghost()
                                .small()
                                .icon(icon("clock"))
                                .label(tr!("tests-slow-label"))
                                .tooltip(tr!("tests-slow"))
                                .selected(slow)
                                .on_click(cx.listener(|this, _, _window, cx| {
                                    if let Some(active) = this.active.clone() {
                                        if let Some(state) = this.pest.get_mut(&active) {
                                            state.slow = !state.slow;
                                        }
                                    }
                                    cx.notify();
                                })),
                        )
                        .child(
                            Button::new("pest-headed")
                                .ghost()
                                .small()
                                .icon(icon("eye"))
                                .label(tr!("tests-headed-label"))
                                .tooltip(tr!("tests-headed"))
                                .selected(headed)
                                .on_click(cx.listener(|this, _, _window, cx| {
                                    if let Some(active) = this.active.clone() {
                                        if let Some(state) = this.pest.get_mut(&active) {
                                            state.headed = !state.headed;
                                            if state.headed {
                                                state.parallel = false;
                                            }
                                        }
                                    }
                                    cx.notify();
                                })),
                        )
                        .child(
                            Button::new("pest-parallel")
                                .ghost()
                                .small()
                                .icon(icon("zap"))
                                .label(tr!("tests-parallel-label"))
                                .tooltip(tr!("tests-parallel"))
                                .selected(parallel)
                                .on_click(cx.listener(|this, _, _window, cx| {
                                    if let Some(active) = this.active.clone() {
                                        if let Some(state) = this.pest.get_mut(&active) {
                                            state.parallel = !state.parallel;
                                            if state.parallel {
                                                state.headed = false;
                                                state.cast = false;
                                            }
                                        }
                                    }
                                    cx.notify();
                                })),
                        ),
                )
            })
    }
}

/// What the theme gives a row, read once per frame and not per row.
#[derive(Clone, Copy)]
struct Look {
    row: gpui_kit::Pixels,
    muted: gpui_kit::Hsla,
    accent: gpui_kit::Hsla,
    success: gpui_kit::Hsla,
    danger: gpui_kit::Hsla,
}

impl Look {
    fn of(cx: &App) -> Self {
        Self {
            row: crate::ui::theme::row_height(cx),
            muted: cx.theme().muted_foreground,
            accent: cx.theme().accent,
            success: cx.theme().success,
            danger: cx.theme().danger,
        }
    }
}

/// A folder: the chevron, the segment, what is under it, and the button that
/// runs all of it.
fn render_dir(
    index: usize,
    tests: &Rc<Vec<Test>>,
    row: Row,
    look: &Look,
    entity: &Entity<ClaudhubApp>,
) -> gpui_kit::AnyElement {
    let Row::Dir {
        test,
        depth,
        expanded,
        tests: under,
        failed,
        status,
        running,
    } = row
    else {
        return div().into_any_element();
    };
    let Some(first) = tests.get(test) else {
        return div().into_any_element();
    };
    let prefix = dir_prefix(&first.class, depth);
    let segment = prefix
        .rsplit(['\\', '/'])
        .next()
        .unwrap_or(&prefix)
        .to_string();
    // The folder's own verdict, before its name: red the moment one test
    // under it is, green when every one ran green, hollow while any is
    // still unknown.
    let (dot, dot_colour) = match status {
        Some(Status::Failed) => ("circle-x", look.danger),
        Some(_) => ("circle-check", look.success),
        None => ("circle-dashed", look.muted),
    };
    let toggle = entity.clone();
    let run = entity.clone();
    let for_run = tests.clone();
    h_flex()
        .id(("pest-dir", index))
        .h(look.row)
        .w_full()
        .pl(gpui_kit::px(6. + 14. * depth as f32))
        .pr(crate::ui::theme::scroll_gutter())
        .gap_1()
        .items_center()
        .cursor_pointer()
        .hover(|s| s.bg(look.accent.opacity(0.4)))
        .on_click(move |_, _window, cx| {
            let prefix = prefix.clone();
            toggle.update(cx, |this, cx| this.toggle_pest_dir(prefix, cx));
        })
        .child(
            icon(if expanded {
                "chevron-down"
            } else {
                "chevron-right"
            })
            .xsmall()
            .text_color(look.muted),
        )
        // While a run covers something under here, the header spins: a
        // closed tree still says what runs.
        .child(if running {
            Spinner::new()
                .xsmall()
                .icon(icon("loader-circle"))
                .color(look.muted)
                .into_any_element()
        } else {
            icon(dot).xsmall().text_color(dot_colour).into_any_element()
        })
        .child(
            div()
                .flex_1()
                .min_w_0()
                .truncate()
                .text_sm()
                .child(SharedString::from(segment)),
        )
        .when(failed > 0, |el| {
            el.child(
                div()
                    .flex_none()
                    .text_xs()
                    .text_color(look.danger)
                    .child(SharedString::from(format!("⨯{failed}"))),
            )
        })
        .child(
            div()
                .flex_none()
                .text_xs()
                .text_color(look.muted)
                .child(SharedString::from(under.to_string())),
        )
        .child(
            Button::new(("pest-dir-run", index))
                .ghost()
                .small()
                .icon(icon("play"))
                .tooltip(tr!("tests-run-class"))
                .on_click(move |_, window, cx| {
                    let Some(first) = for_run.get(test) else {
                        return;
                    };
                    // Named by the first test under it: a folder mixing two
                    // runners runs the first one's scope, and the runners'
                    // roots never really share a name.
                    let label = SharedString::from(format!(
                        "{} {}",
                        first.runner.label(),
                        dir_prefix(&first.class, depth)
                    ));
                    let target = crate::suite::scope_target(first, depth);
                    run.update(cx, |this, cx| {
                        this.launch_tests(label, vec![target], window, cx);
                    });
                    // The row underneath folds the chevron: launching must
                    // not also flip it.
                    cx.stop_propagation();
                }),
        )
        .into_any_element()
}

/// One test: its dot, its (real) name, and the click that runs it.
#[allow(clippy::too_many_arguments)]
fn render_test(
    index: usize,
    tests: &Rc<Vec<Test>>,
    labels: &Rc<Vec<SharedString>>,
    statuses: &Rc<Vec<Option<Status>>>,
    running: &Rc<Vec<bool>>,
    row: Row,
    look: &Look,
    entity: &Entity<ClaudhubApp>,
) -> gpui_kit::AnyElement {
    let Row::Test { test: at, depth } = row else {
        return div().into_any_element();
    };
    let Some(test) = tests.get(at) else {
        return div().into_any_element();
    };
    let label = labels
        .get(at)
        .cloned()
        .unwrap_or_else(|| SharedString::from(test.name.clone()));
    let status = statuses.get(at).copied().flatten();
    let (dot, colour) = match status {
        Some(Status::Passed) => ("circle-check", look.success),
        Some(Status::Failed) => ("circle-x", look.danger),
        Some(Status::Skipped) => ("circle-stop", look.muted),
        // Never ran: a hollow dot, not a claim.
        None => ("circle-dashed", look.muted),
    };
    let spinning = running.get(at).copied().unwrap_or(false);
    let run = entity.clone();
    let menu = entity.clone();
    let (for_click, for_menu) = (tests.clone(), tests.clone());
    let menu_label = label.clone();
    h_flex()
        .id(("pest-row", index))
        .h(look.row)
        .w_full()
        .pl(gpui_kit::px(6. + 14. * depth as f32))
        .pr(crate::ui::theme::scroll_gutter())
        .gap_1()
        .items_center()
        .cursor_pointer()
        .hover(|s| s.bg(look.accent.opacity(0.4)))
        .on_click({
            let label = label.clone();
            move |_, window, cx| {
                let Some(test) = for_click.get(at) else {
                    return;
                };
                let run_label = SharedString::from(format!("{} {label}", test.runner.label()));
                let target = crate::suite::test_target(test);
                run.update(cx, |this, cx| {
                    this.launch_tests(run_label, vec![target], window, cx);
                });
            }
        })
        .child(if spinning {
            Spinner::new()
                .xsmall()
                .icon(icon("loader-circle"))
                .color(look.muted)
                .into_any_element()
        } else {
            icon(dot).xsmall().text_color(colour).into_any_element()
        })
        .child(div().flex_1().min_w_0().truncate().text_sm().child(label))
        .when(test.datasets > 0, |el| {
            el.child(
                div()
                    .flex_none()
                    .text_xs()
                    .text_color(look.muted)
                    .child(SharedString::from(format!("×{}", test.datasets))),
            )
        })
        .context_menu(move |popup, _window, _cx| match for_menu.get(at) {
            Some(test) => row_menu(
                popup,
                &menu,
                test,
                &menu_label,
                status == Some(Status::Failed),
            ),
            None => popup,
        })
        .into_any_element()
}

fn row_menu(
    popup: PopupMenu,
    entity: &Entity<ClaudhubApp>,
    test: &Test,
    label: &SharedString,
    failed: bool,
) -> PopupMenu {
    let popup = popup.item({
        let entity = entity.clone();
        let target = crate::suite::test_target(test);
        let label = SharedString::from(format!("{} {label}", test.runner.label()));
        PopupMenuItem::new(tr!("tests-run-test"))
            .icon(icon("play"))
            .on_click(move |_, window, cx| {
                let (label, target) = (label.clone(), target.clone());
                entity.update(cx, |this, cx| {
                    this.launch_tests(label, vec![target], window, cx);
                });
            })
    });
    // Red rows only: a green test has nothing to hand over, and an entry
    // that would send "this passes" is one nobody means to press.
    let popup = match failed {
        false => popup,
        true => popup.item({
            let entity = entity.clone();
            let test = test.clone();
            let label = label.to_string();
            PopupMenuItem::new(tr!("tests-hand"))
                .icon(icon("bot"))
                .on_click(move |_, window, cx| {
                    entity.update(cx, |this, cx| this.hand_test(&test, &label, window, cx));
                })
        }),
    };
    // Only when the file is known — a Pest class outside the autoloader has
    // none, and an entry that opens nothing is worse than no entry.
    let popup = match test.file.is_empty() {
        true => popup,
        false => popup.item({
            let entity = entity.clone();
            let file = test.file.clone();
            // The line is looked for in the text rather than carried: a
            // listing names a test, never the line it sits on. See
            // `Landing::Text`.
            //
            // The row's label and not `test.name`: a run's account teaches the
            // real description, where the listing only had the mangled name
            // read back — `it sums 2 2` for a test the file writes as
            // `it('sums 2 + 2')`, which no search would find.
            let needles = crate::suite::test_needles(label);
            PopupMenuItem::new(tr!("tests-open-file"))
                .icon(icon("file-pen"))
                .on_click(move |_, _window, cx| {
                    let (file, needles) = (file.clone(), needles.clone());
                    entity.update(cx, |this, cx| {
                        let Some(worktree) = this.active.clone() else {
                            return;
                        };
                        let landing = (!needles.is_empty())
                            .then_some(crate::ui::explorer::Landing::Text(needles));
                        // `wslpath::join` and not `Path::join`: the worktree
                        // is a path on the server, and under Windows the
                        // separator this machine writes is the wrong one.
                        this.open_at(crate::wslpath::join(&worktree, file), landing, cx);
                    });
                })
        }),
    };
    let popup = popup.item({
        let entity = entity.clone();
        let target = crate::suite::test_target(test);
        let label = SharedString::from(format!("{} {label}", test.runner.label()));
        // Where Ctrl+C exists, and where an interactive suite can ask.
        PopupMenuItem::new(tr!("tests-run-terminal"))
            .icon(icon("terminal"))
            .on_click(move |_, window, cx| {
                let (label, target) = (label.clone(), target.clone());
                entity.update(cx, |this, cx| {
                    this.run_tests_in_terminal(label, &target, window, cx);
                });
            })
    });
    popup.item({
        let entity = entity.clone();
        let target = crate::suite::test_target(test);
        // For the terminal one already has open: the panel's gesture, portable.
        PopupMenuItem::new(tr!("tests-copy-filter"))
            .icon(icon("copy"))
            .on_click(move |_, _window, cx| {
                let mut target = target.clone();
                let mut line = None;
                entity.update(cx, |this, _| {
                    this.apply_pest_modes(&mut target);
                    line = this
                        .active
                        .as_deref()
                        .map(|worktree| crate::suite::terminal_command(worktree, &target));
                });
                if let Some(line) = line {
                    cx.write_to_clipboard(gpui_kit::ClipboardItem::new_string(line));
                }
            })
    })
}

/// Pest refused to list — a parse error in a test file, most days. Its
/// message is the content: it names the file and the line.
fn failed_pest(message: SharedString, cx: &App) -> gpui_kit::AnyElement {
    v_flex()
        .size_full()
        .p_4()
        .gap_2()
        .child(
            h_flex()
                .gap_1()
                .items_center()
                .text_color(cx.theme().danger)
                .child(icon("alert-circle").xsmall())
                .child(div().text_sm().child(tr!("tests-failed"))),
        )
        .child(
            div()
                .flex_1()
                .min_h_0()
                .overflow_hidden()
                .text_xs()
                .font_family(cx.theme().mono_font_family.clone())
                .text_color(cx.theme().muted_foreground)
                .child(message),
        )
        .into_any_element()
}

/// No suite here. Painted on the tests screen, whose empty state is the
/// screen; elsewhere the tab simply is not there.
fn missing_pest(pending: bool, cx: &App) -> gpui_kit::AnyElement {
    let message = if pending {
        tr!("tests-loading")
    } else {
        tr!("tests-missing")
    };
    v_flex()
        .size_full()
        .items_center()
        .justify_center()
        .gap_2()
        .text_color(cx.theme().muted_foreground)
        .child(icon("circle-check"))
        .child(div().text_sm().px_4().child(message))
        .into_any_element()
}

/// Nothing to show: a listing under way, a search or the failures filter
/// that found nothing, or a suite with no test at all — different things,
/// and saying the wrong one is how a panel reads as broken.
fn empty_pest(query: &str, pending: bool, only_failed: bool, cx: &App) -> gpui_kit::AnyElement {
    let message = if pending {
        tr!("tests-loading")
    } else if only_failed {
        tr!("tests-none-failing")
    } else if query.trim().is_empty() {
        tr!("tests-empty")
    } else {
        tr!("find-no-match")
    };
    v_flex()
        .size_full()
        .items_center()
        .justify_center()
        .gap_2()
        .text_color(cx.theme().muted_foreground)
        .child(icon("circle-check"))
        .child(div().text_sm().px_4().child(message))
        .into_any_element()
}

// — The run panel ————————————————————————————————————————

impl ClaudhubApp {
    pub(super) fn render_test_run(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let Some(active) = self.active.clone() else {
            return v_flex()
                .size_full()
                .items_center()
                .justify_center()
                .text_color(cx.theme().muted_foreground)
                .child(div().text_sm().child(tr!("no-worktree")))
                .into_any_element();
        };
        let Some(state) = self.pest_runs.get(&active) else {
            return v_flex()
                .size_full()
                .items_center()
                .justify_center()
                .gap_2()
                .text_color(cx.theme().muted_foreground)
                .child(icon("play"))
                .child(div().text_sm().px_4().child(tr!("tests-no-run")))
                .into_any_element();
        };

        let with_failures = state.run.clone().filter(|run| run.failed > 0);
        // The runner's own lines, then what the live channel has said since:
        // the tail is the newest, and while a file runs the newest is the
        // test under way.
        let mut rows: Vec<RunRow> = state.lines.iter().cloned().map(RunRow::Said).collect();
        rows.extend(state.live.iter().map(|step| RunRow::Live {
            status: step.status,
            label: step.label.clone(),
        }));
        let bar = self.render_run_bar(&active, cx);
        let failures = with_failures.map(|run| self.render_run_failures(&active, run, window, cx));
        let mono = cx.theme().mono_font_family.clone();
        let scroll = self.pest_run_scroll.clone();
        let count = rows.len();
        let rows = Rc::new(rows);
        let row = crate::ui::theme::row_height(cx);
        let look = Look::of(cx);
        v_flex()
            .size_full()
            .child(bar)
            .children(failures)
            .child(
                div().flex_1().min_h_0().child(
                    self.scrolled(
                        "pest-run",
                        &scroll,
                        crate::ui::motion::Axes::Vertical,
                        window,
                        uniform_list("pest-run-lines", count, move |visible, _window, _cx| {
                            visible
                                .map(|index| {
                                    let line = match rows.get(index) {
                                        Some(RunRow::Said(line)) => line.clone(),
                                        // The live channel carries a fate, not
                                        // a symbol: the one the runner would
                                        // have written is put in front, so
                                        // `line_kind` colours both the same
                                        // way and a copied panel reads alike.
                                        Some(RunRow::Live { status, label }) => SharedString::from(
                                            format!("{} {label}", live_glyph(*status)),
                                        ),
                                        None => SharedString::default(),
                                    };
                                    // The colours the runner painted were
                                    // stripped with the rest of its ANSI;
                                    // what its symbols say is put back.
                                    let colour = match line_kind(&line) {
                                        LineKind::Pass => Some(look.success),
                                        LineKind::Fail => Some(look.danger),
                                        LineKind::Skip => Some(look.muted),
                                        LineKind::Plain => None,
                                    };
                                    div()
                                        .h(row)
                                        .w_full()
                                        .px_2()
                                        .whitespace_nowrap()
                                        .text_xs()
                                        .font_family(mono.clone())
                                        .when_some(colour, |el, colour| el.text_color(colour))
                                        .child(line)
                                        .into_any_element()
                                })
                                .collect::<Vec<_>>()
                        })
                        .size_full()
                        .track_scroll(&scroll.clone()),
                        cx,
                    ),
                ),
            )
            .into_any_element()
    }

    /// The red part of the account, each line a jump to the code: the file
    /// and line come from the run itself.
    fn render_run_failures(
        &mut self,
        worktree: &Path,
        run: Rc<Run>,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) -> gpui_kit::AnyElement {
        let failed: Vec<usize> = run
            .outcomes
            .iter()
            .enumerate()
            .filter(|(_, outcome)| outcome.status == Status::Failed)
            .map(|(index, _)| index)
            .collect();
        let row = crate::ui::theme::row_height(cx);
        let look = Look::of(cx);
        let entity = cx.entity();
        let worktree = worktree.to_path_buf();
        let shown = failed.len().min(8);
        v_flex()
            .flex_none()
            .w_full()
            .border_b_1()
            .border_color(cx.theme().border)
            .children(failed.iter().take(shown).map(|&index| {
                let Some(outcome) = run.outcomes.get(index) else {
                    return div().into_any_element();
                };
                let open = entity.clone();
                let target = (!outcome.file.is_empty())
                    .then(|| (crate::wslpath::join(&worktree, &outcome.file), outcome.line));
                let name = SharedString::from(outcome.name.clone());
                let place =
                    SharedString::from(crate::suite::short_class(&outcome.class).to_string());
                let said = SharedString::from(
                    outcome
                        .message
                        .lines()
                        .next()
                        .unwrap_or_default()
                        .to_string(),
                );
                h_flex()
                    .id(("pest-failure", index))
                    .h(row)
                    .w_full()
                    .px_2()
                    .gap_1()
                    .items_center()
                    .cursor_pointer()
                    .hover(|s| s.bg(look.accent.opacity(0.4)))
                    .on_click(move |_, _window, cx| {
                        let Some((path, line)) = target.clone() else {
                            return;
                        };
                        open.update(cx, |this, cx| {
                            let landing = line.map(|line| crate::ui::explorer::Landing::Position {
                                // A failure counts from one, a landing from zero.
                                line: line.saturating_sub(1),
                                character: 0,
                            });
                            this.open_at(path, landing, cx);
                        });
                    })
                    .child(icon("circle-x").xsmall().text_color(look.danger))
                    .child(div().flex_none().text_sm().child(name))
                    .child(
                        div()
                            .flex_none()
                            .text_xs()
                            .text_color(look.muted)
                            .child(place),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .text_xs()
                            .text_color(look.muted)
                            .child(said),
                    )
                    .child(
                        // One click, ready to paste elsewhere — a bug
                        // report, an AI — with the file and the whole
                        // message, not the row's truncated first line.
                        Button::new(("pest-failure-copy", index))
                            .ghost()
                            .small()
                            .icon(icon("copy"))
                            .label(tr!("tests-copy"))
                            .tooltip(tr!("tests-copy-result"))
                            .on_click({
                                let text = outcome_text(outcome);
                                move |_, _window, cx| {
                                    cx.write_to_clipboard(gpui_kit::ClipboardItem::new_string(
                                        text.clone(),
                                    ));
                                    // The row underneath opens the file:
                                    // copying must not travel there.
                                    cx.stop_propagation();
                                }
                            }),
                    )
                    .child({
                        let hand = entity.clone();
                        Button::new(("pest-failure-hand", index))
                            .ghost()
                            .small()
                            .icon(icon("bot"))
                            .tooltip(tr!("tests-hand"))
                            .on_click(move |_, window, cx| {
                                // Same as the copy beside it: the row
                                // underneath opens the file.
                                cx.stop_propagation();
                                hand.update(cx, |this, cx| {
                                    this.hand_run_failures(Some(index), window, cx);
                                });
                            })
                    })
                    .into_any_element()
            }))
            .when(failed.len() > shown, |el| {
                el.child(
                    div()
                        .px_2()
                        .py_0p5()
                        .text_xs()
                        .text_color(look.muted)
                        .child(tr!("tests-more-failures", { n: failed.len() - shown })),
                )
            })
            .into_any_element()
    }
}

/// The run's headline: running with a spinner, failed to start, or the
/// totals with when and how long.
impl ClaudhubApp {
    /// The run's headline: running with a spinner and the button that stops
    /// it, stopped, failed to start, or the totals with when and how long.
    fn render_run_bar(&self, worktree: &Path, cx: &mut Context<Self>) -> gpui_kit::AnyElement {
        let Some(state) = self.pest_runs.get(worktree) else {
            return div().into_any_element();
        };
        let bar = h_flex()
            .h(crate::ui::theme::bar_height(cx))
            .w_full()
            .px_2()
            .gap_2()
            .items_center()
            .border_b_1()
            .border_color(cx.theme().border)
            .text_xs();
        if state.running {
            // Every id of this campaign, and none other: another worktree's
            // campaign may queue on the same worker, and a stop is by id.
            let stop = state.since..=state.id;
            return bar
                .child(
                    Spinner::new()
                        .xsmall()
                        .icon(icon("loader-circle"))
                        .color(cx.theme().muted_foreground),
                )
                .child(div().flex_none().child(state.label.clone()))
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .truncate()
                        .text_color(cx.theme().muted_foreground)
                        .child(tr!("tests-running")),
                )
                .child(
                    Button::new("tests-stop")
                        .ghost()
                        .small()
                        .icon(icon("circle-stop"))
                        .tooltip(tr!("tests-stop"))
                        .on_click(cx.listener(move |this, _, _window, cx| {
                            for id in stop.clone() {
                                this.git.send(Cmd::TestsStop { id });
                            }
                            cx.notify();
                        })),
                )
                .into_any_element();
        }
        if let Some(error) = &state.error {
            // The stop the user asked for is not a failure: it is said in
            // grey, with the button that asked it.
            if error.as_ref() == crate::suite::STOPPED {
                return bar
                    .child(
                        icon("circle-stop")
                            .xsmall()
                            .text_color(cx.theme().muted_foreground),
                    )
                    .child(div().flex_none().child(state.label.clone()))
                    .child(
                        div()
                            .text_color(cx.theme().muted_foreground)
                            .child(tr!("tests-stopped")),
                    )
                    .into_any_element();
            }
            return bar
                .child(icon("alert-circle").xsmall().text_color(cx.theme().danger))
                .child(div().flex_none().child(state.label.clone()))
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .truncate()
                        .text_color(cx.theme().danger)
                        .child(error.clone()),
                )
                .into_any_element();
        }
        let Some(run) = &state.run else {
            return bar.child(state.label.clone()).into_any_element();
        };
        let secs = run.duration_ms as f64 / 1000.;
        bar.child(div().flex_none().child(state.label.clone()))
            .child(
                div()
                    .text_color(cx.theme().success)
                    .child(SharedString::from(format!("✓{}", run.passed))),
            )
            .when(run.failed > 0, |el| {
                el.child(
                    div()
                        .text_color(cx.theme().danger)
                        .child(SharedString::from(format!("⨯{}", run.failed))),
                )
            })
            .when(run.skipped > 0, |el| {
                el.child(
                    div()
                        .text_color(cx.theme().muted_foreground)
                        .child(SharedString::from(format!("−{}", run.skipped))),
                )
            })
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .text_color(cx.theme().muted_foreground)
                    .child(SharedString::from(format!(
                        "{secs:.1}s · {}",
                        clock(state.started_at)
                    ))),
            )
            .when(run.failed > 0, |el| {
                el.child(
                    Button::new("tests-run-hand-all")
                        .ghost()
                        .small()
                        .icon(icon("bot"))
                        .label(tr!("tests-hand-label"))
                        .tooltip(tr!("tests-hand-all", { n: run.failed }))
                        .on_click(cx.listener(|this, _, window, cx| {
                            this.hand_run_failures(None, window, cx);
                        })),
                )
            })
            .into_any_element()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_test_named_twice_takes_one_row() {
        let key = ("Tests\\Unit\\MathTest".to_string(), "it_sums".to_string());
        let mut live = Vec::new();
        note_step(&mut live, key.clone(), "it sums".into(), None);
        note_step(
            &mut live,
            key.clone(),
            "it sums".into(),
            Some(Status::Passed),
        );
        assert_eq!(live.len(), 1);
        assert_eq!(live[0].status, Some(Status::Passed));
        assert_eq!(live[0].label, "it sums");
    }

    #[test]
    fn within_one_run_a_red_row_stays_red() {
        // A row collapsing dataset cases is named once per case, and the
        // panel must not lose the case that failed to the ones that passed.
        let key = (
            "Tests\\Unit\\MathTest".to_string(),
            "it_divides".to_string(),
        );
        let mut live = Vec::new();
        note_step(
            &mut live,
            key.clone(),
            "it divides".into(),
            Some(Status::Failed),
        );
        note_step(
            &mut live,
            key.clone(),
            "it divides".into(),
            Some(Status::Passed),
        );
        assert_eq!(live[0].status, Some(Status::Failed));
    }

    #[test]
    fn the_glyph_is_the_one_the_runner_would_have_written() {
        // Which is what lets one list read as one list: `line_kind` colours a
        // live row without knowing it is one.
        assert_eq!(
            line_kind(&format!("{} it sums", live_glyph(Some(Status::Passed)))),
            LineKind::Pass
        );
        assert_eq!(
            line_kind(&format!("{} it sums", live_glyph(Some(Status::Failed)))),
            LineKind::Fail
        );
        assert_eq!(
            line_kind(&format!("{} it sums", live_glyph(Some(Status::Skipped)))),
            LineKind::Skip
        );
        assert_eq!(
            line_kind(&format!("{} it sums", live_glyph(None))),
            LineKind::Plain
        );
    }

    fn test(class: &str, name: &str) -> Test {
        Test {
            runner: Runner::Pest,
            class: class.to_string(),
            method: format!(
                "__pest_evaluable_{}",
                name.replace(['+', ':', '-'], "_").replace(' ', "_")
            ),
            name: name.to_string(),
            pattern: String::new(),
            datasets: 0,
            file: String::new(),
        }
    }

    fn suite() -> Vec<Test> {
        vec![
            test("Tests\\Unit\\MathTest", "it sums"),
            test("Tests\\Unit\\MathTest", "it divides"),
            test("Tests\\Unit\\Forms\\BillTest", "it bills"),
            test("Tests\\Feature\\HttpTest", "it answers"),
        ]
    }

    fn plain(tests: &[Test]) -> (Vec<SharedString>, Vec<Option<Status>>) {
        (
            tests
                .iter()
                .map(|t| SharedString::from(t.name.clone()))
                .collect(),
            vec![None; tests.len()],
        )
    }

    /// The tree opens closed: top folders only, each saying what it holds.
    #[test]
    fn the_tree_opens_closed() {
        let tests = suite();
        let (labels, statuses) = plain(&tests);
        assert_eq!(
            rows(
                &tests,
                &labels,
                &statuses,
                &vec![false; tests.len()],
                "",
                &HashSet::new(),
                false
            ),
            [
                Row::Dir {
                    test: 0,
                    depth: 0,
                    expanded: false,
                    tests: 3,
                    failed: 0,
                    status: None,
                    running: false,
                },
                Row::Dir {
                    test: 3,
                    depth: 0,
                    expanded: false,
                    tests: 1,
                    failed: 0,
                    status: None,
                    running: false,
                },
            ]
        );
    }

    /// Opening a folder shows what is directly under it — and only that:
    /// its subfolders stay closed until asked.
    #[test]
    fn a_folder_opens_one_level() {
        let tests = suite();
        let (labels, statuses) = plain(&tests);
        let expanded: HashSet<String> = [String::from("Unit")].into();
        let shown = rows(
            &tests,
            &labels,
            &statuses,
            &vec![false; tests.len()],
            "",
            &expanded,
            false,
        );
        assert_eq!(
            shown,
            [
                Row::Dir {
                    test: 0,
                    depth: 0,
                    expanded: true,
                    tests: 3,
                    failed: 0,
                    status: None,
                    running: false,
                },
                Row::Dir {
                    test: 0,
                    depth: 1,
                    expanded: false,
                    tests: 2,
                    failed: 0,
                    status: None,
                    running: false,
                },
                Row::Dir {
                    test: 2,
                    depth: 1,
                    expanded: false,
                    tests: 1,
                    failed: 0,
                    status: None,
                    running: false,
                },
                Row::Dir {
                    test: 3,
                    depth: 0,
                    expanded: false,
                    tests: 1,
                    failed: 0,
                    status: None,
                    running: false,
                },
            ]
        );
    }

    /// A query ignores the folds — what one asked for must not hide behind a
    /// closed folder — and takes the empty folders with it.
    #[test]
    fn a_query_ignores_the_folds() {
        let tests = suite();
        let (labels, statuses) = plain(&tests);
        let shown = rows(
            &tests,
            &labels,
            &statuses,
            &vec![false; tests.len()],
            "answers",
            &HashSet::new(),
            false,
        );
        assert_eq!(
            shown,
            [
                Row::Dir {
                    test: 3,
                    depth: 0,
                    expanded: true,
                    tests: 1,
                    failed: 0,
                    status: None,
                    running: false,
                },
                Row::Dir {
                    test: 3,
                    depth: 1,
                    expanded: true,
                    tests: 1,
                    failed: 0,
                    status: None,
                    running: false,
                },
                Row::Test { test: 3, depth: 2 },
            ]
        );
    }

    /// The failures filter keeps the red rows and counts them on the way up.
    #[test]
    fn the_failures_filter_keeps_the_red() {
        let tests = suite();
        let (labels, _) = plain(&tests);
        let statuses = vec![
            Some(Status::Passed),
            Some(Status::Failed),
            None,
            Some(Status::Passed),
        ];
        let shown = rows(
            &tests,
            &labels,
            &statuses,
            &vec![false; tests.len()],
            "",
            &HashSet::new(),
            true,
        );
        assert_eq!(
            shown,
            [
                Row::Dir {
                    test: 1,
                    depth: 0,
                    expanded: true,
                    tests: 1,
                    failed: 1,
                    status: Some(Status::Failed),
                    running: false,
                },
                Row::Dir {
                    test: 1,
                    depth: 1,
                    expanded: true,
                    tests: 1,
                    failed: 1,
                    status: Some(Status::Failed),
                    running: false,
                },
                Row::Test { test: 1, depth: 2 },
            ]
        );
    }

    /// A folder claims green only when every test under it has run green —
    /// one unknown keeps it hollow, one red makes it red.
    #[test]
    fn a_folder_only_claims_what_its_rows_ran() {
        let tests = suite();
        let (labels, _) = plain(&tests);
        // Unit\MathTest both green, Unit\Forms\BillTest never ran.
        let statuses = vec![Some(Status::Passed), Some(Status::Passed), None, None];
        let shown = rows(
            &tests,
            &labels,
            &statuses,
            &vec![false; tests.len()],
            "",
            &HashSet::new(),
            false,
        );
        let Some(Row::Dir { status, .. }) = shown.first() else {
            panic!("a folder first");
        };
        // `Unit` holds an unknown: hollow, not green.
        assert_eq!(*status, None);
        // Open it: MathTest is all green, Forms still hollow.
        let expanded: HashSet<String> = [String::from("Unit")].into();
        let shown = rows(
            &tests,
            &labels,
            &statuses,
            &vec![false; tests.len()],
            "",
            &expanded,
            false,
        );
        let dirs: Vec<Option<Status>> = shown
            .iter()
            .filter_map(|row| match row {
                Row::Dir {
                    status, depth: 1, ..
                } => Some(*status),
                _ => None,
            })
            .collect();
        assert_eq!(dirs, [Some(Status::Passed), None]);
    }

    /// The copied failure is paste-ready: the test, its place, the whole
    /// message — and nothing empty leaves stray lines behind.
    #[test]
    fn a_copied_failure_says_where_and_what() {
        let outcome = Outcome {
            class: "Tests\\Feature\\HttpTest".into(),
            name: "it will fail on purpose".into(),
            status: Status::Failed,
            message: "Failed asserting that 1 is identical to 2.\nat tests/Feature/HttpTest.php:3"
                .into(),
            file: "tests/Feature/HttpTest.php".into(),
            line: Some(3),
            cases: 1,
            time_ms: 0,
        };
        assert_eq!(
            outcome_text(&outcome),
            "Tests\\Feature\\HttpTest :: it will fail on purpose\n\
             tests/Feature/HttpTest.php:3\n\n\
             Failed asserting that 1 is identical to 2.\nat tests/Feature/HttpTest.php:3"
        );
        let bare = Outcome {
            file: String::new(),
            line: None,
            message: String::new(),
            ..outcome
        };
        assert_eq!(
            outcome_text(&bare),
            "Tests\\Feature\\HttpTest :: it will fail on purpose"
        );
    }

    /// While a campaign runs, the covered rows show as loading — and a
    /// closed folder says so for what it hides.
    #[test]
    fn a_running_campaign_spins_its_folders() {
        let tests = suite();
        let (labels, statuses) = plain(&tests);
        // The campaign runs Unit\MathTest only.
        let running = vec![true, true, false, false];
        let shown = rows(
            &tests,
            &labels,
            &statuses,
            &running,
            "",
            &HashSet::new(),
            false,
        );
        let dirs: Vec<bool> = shown
            .iter()
            .filter_map(|row| match row {
                Row::Dir { running, .. } => Some(*running),
                _ => None,
            })
            .collect();
        // `Unit` spins, `Feature` does not.
        assert_eq!(dirs, [true, false]);
    }

    /// A narrated line is coloured by the symbol it starts with — the
    /// runners' own vocabulary, Pest, Vitest and Jest alike.
    #[test]
    fn a_line_is_coloured_by_its_symbol() {
        assert_eq!(line_kind("  ✓ it sums 2 + 2"), LineKind::Pass);
        assert_eq!(
            line_kind("   PASS  Tests\\Unit\\ContractTest"),
            LineKind::Pass
        );
        assert_eq!(line_kind("  ⨯ it will fail on purpose"), LineKind::Fail);
        assert_eq!(
            line_kind("   FAILED  Tests\\Feature\\HttpTest"),
            LineKind::Fail
        );
        assert_eq!(line_kind("  ✕ answers"), LineKind::Fail);
        assert_eq!(line_kind("  - it is skipped → later"), LineKind::Skip);
        assert_eq!(line_kind("  ↓ name"), LineKind::Skip);
        assert_eq!(line_kind("  Tests:    1 failed, 6 passed"), LineKind::Plain);
        // A list dash without a reason arrow is prose, not a skip.
        assert_eq!(line_kind("- some note"), LineKind::Plain);
    }

    /// The account's outcomes land on the listed tests they name — real
    /// description against mangled method.
    #[test]
    fn a_run_lands_on_its_rows() {
        let tests = vec![
            Test {
                runner: Runner::Pest,
                class: "Tests\\Unit\\MathTest".into(),
                method: "__pest_evaluable_it_sums_2___2".into(),
                name: "it sums 2 2".into(),
                pattern: String::new(),
                datasets: 0,
                file: String::new(),
            },
            Test {
                runner: Runner::Pest,
                class: "LegacyTest".into(),
                method: "testOldSchool".into(),
                name: "testOldSchool".into(),
                pattern: String::new(),
                datasets: 0,
                file: String::new(),
            },
        ];
        let outcome = |class: &str, name: &str| Outcome {
            class: class.into(),
            name: name.into(),
            status: Status::Passed,
            message: String::new(),
            file: String::new(),
            line: None,
            cases: 1,
            time_ms: 0,
        };
        let outcomes = vec![
            outcome("Tests\\Unit\\MathTest", "it sums 2 + 2"),
            outcome("LegacyTest", "Old school"),
            outcome("Tests\\Unit\\MathTest", "it never was listed"),
        ];
        assert_eq!(paired(&tests, &outcomes), [(0, 0), (1, 1)]);
    }

    #[test]
    fn a_dir_prefix_is_the_short_classes_head() {
        assert_eq!(dir_prefix("Tests\\Unit\\Forms\\BillTest", 0), "Unit");
        assert_eq!(dir_prefix("Tests\\Unit\\Forms\\BillTest", 1), "Unit\\Forms");
        assert_eq!(dir_prefix("LegacyTest", 0), "LegacyTest");
    }

    /// The suite is re-read for what changes it: the tests, and the two files
    /// that decide what a suite is. Not the application code — a listing
    /// boots PHP, and the watcher fires on every save.
    #[test]
    fn only_the_suites_own_files_reload_it() {
        let wt = Path::new("/p/site");
        assert!(reloads(wt, Path::new("/p/site/tests/Unit/MathTest.php")));
        assert!(reloads(wt, Path::new("/p/site/tests/Pest.php")));
        assert!(reloads(wt, Path::new("/p/site/phpunit.xml")));
        assert!(reloads(wt, Path::new("/p/site/composer.json")));
        assert!(!reloads(wt, Path::new("/p/site/app/Models/User.php")));
        assert!(!reloads(wt, Path::new("/p/site/tests/fixtures/data.json")));
        assert!(!reloads(wt, Path::new("/elsewhere/tests/T.php")));
    }

    fn fate(class: &str, name: &str, status: Status, message: &str) -> Outcome {
        Outcome {
            class: class.into(),
            name: name.into(),
            status,
            message: message.into(),
            file: class.into(),
            line: None,
            cases: 1,
            time_ms: 0,
        }
    }

    fn jest_file(path: &str) -> Test {
        Test {
            runner: Runner::Jest,
            class: "src".into(),
            method: path.into(),
            name: path.rsplit('/').next().unwrap_or(path).into(),
            pattern: String::new(),
            datasets: 0,
            file: path.into(),
        }
    }

    fn account(outcomes: Vec<Outcome>) -> Run {
        Run {
            passed: 0,
            failed: 0,
            skipped: 0,
            duration_ms: 0,
            outcomes,
        }
    }

    #[test]
    fn a_red_row_hands_what_the_run_said_of_it() {
        let file = jest_file("src/http.test.js");
        let run = account(vec![
            fate("src/http.test.js", "answers", Status::Passed, ""),
            fate("src/http.test.js", "inside fails", Status::Failed, "boom"),
            fate("src/other.test.js", "elsewhere", Status::Failed, "other"),
        ]);
        // A Jest row is a file: the red **in it**, each run again alone.
        let handed = row_failures(&file, "http.test.js", Some(&run), false);
        assert_eq!(handed.len(), 1);
        assert_eq!(handed[0].name, "inside fails");
        assert_eq!(handed[0].message.as_deref(), Some("boom"));
        let command = handed[0].command.as_deref().unwrap_or_default();
        assert!(command.contains("--runTestsByPath src/http.test.js"));
        assert!(command.contains(r#"-t "^inside fails\$""#));

        // No account at hand — a restart, a later campaign: the row itself,
        // with nothing claimed about why.
        let handed = row_failures(&file, "http.test.js", None, false);
        assert_eq!(handed.len(), 1);
        assert_eq!(handed[0].message, None);
        assert_eq!(handed[0].name, "http.test.js");
        let passed = account(vec![fate("src/other.test.js", "x", Status::Failed, "")]);
        assert_eq!(
            row_failures(&file, "http.test.js", Some(&passed), false)[0].message,
            None
        );
    }

    #[test]
    fn the_tree_hands_its_red_rows_only() {
        let tests = suite();
        let (labels, mut statuses) = plain(&tests);
        statuses[1] = Some(Status::Failed);
        statuses[2] = Some(Status::Passed);
        statuses[3] = Some(Status::Failed);
        let handed = tree_failures(&tests, &labels, &statuses, None, false);
        let names: Vec<&str> = handed.iter().map(|h| h.name.as_str()).collect();
        assert_eq!(names, ["it divides", "it answers"]);
    }

    #[test]
    fn a_runs_red_keeps_its_order_and_its_commands() {
        let tests = vec![jest_file("src/http.test.js")];
        let outcomes = vec![
            fate("src/gone.test.js", "unlisted", Status::Failed, "x"),
            fate("src/http.test.js", "fine", Status::Passed, ""),
            fate("src/http.test.js", "broken", Status::Failed, "y"),
        ];
        let handed = run_failures(&tests, &outcomes, false);
        assert_eq!(handed.len(), 2);
        // Unpaired: no listed test to say the runner, so no command either —
        // an invented one would run something else.
        assert_eq!(handed[0].command, None);
        assert_eq!(handed[0].runner, None);
        assert_eq!(handed[1].runner, Some(Runner::Jest));
        assert!(handed[1].command.is_some());
    }

    #[test]
    fn the_output_goes_along_only_when_it_speaks_of_that_test_alone() {
        let one = account(vec![
            fate("src/a.test.js", "fine", Status::Passed, ""),
            fate("src/a.test.js", "broken", Status::Failed, "y"),
        ]);
        let handed = run_failures(&[], &one.outcomes, false);
        assert!(output_speaks_of(&one, &handed));

        let two = account(vec![
            fate("src/a.test.js", "broken", Status::Failed, "y"),
            fate("src/b.test.js", "also", Status::Failed, "z"),
        ]);
        let first = run_failures(&[], &two.outcomes[..1], false);
        assert!(!output_speaks_of(&two, &first));
        assert!(!output_speaks_of(
            &two,
            &run_failures(&[], &two.outcomes, false)
        ));

        // A verdict whose account is gone speaks of nothing in this run.
        let test = jest_file("src/a.test.js");
        let blind = vec![crate::suite::handoff_unexplained(&test, "a", false)];
        assert!(!output_speaks_of(&one, &blind));
    }
}
