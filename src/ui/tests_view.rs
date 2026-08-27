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

use gpui::{div, prelude::*, uniform_list, App, Context, Entity, SharedString, Window};
use gpui_component::{
    button::{Button, ButtonVariants},
    h_flex,
    menu::{ContextMenuExt, PopupMenu, PopupMenuItem},
    spinner::Spinner,
    v_flex, ActiveTheme, Disableable, Selectable, Sizable,
};

use crate::runtime::Cmd;
use crate::suite::{Outcome, Report, Run, Runner, Status, Target, Test};
use crate::tr;
use crate::ui::app::ClaudhubApp;
use crate::ui::find::Pane;
use crate::ui::icons::icon;
use crate::ui::store::{Store, TestMark, TestsRun};

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
    pub running: bool,
    /// Unix seconds.
    pub started_at: i64,
    /// The run's text so far, tail-capped at [`RUN_LINES_KEPT`].
    pub lines: VecDeque<SharedString>,
    /// Why a suite never started, when one did not.
    pub error: Option<SharedString>,
    /// The finished accounts so far, merged.
    pub run: Option<Rc<Run>>,
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

    // First pass: how many kept tests, and how many red, under each folder.
    let mut counts: HashMap<String, (u32, u32)> = HashMap::new();
    for (at, test) in tests.iter().enumerate() {
        if !kept(at, test) {
            continue;
        }
        let failed = statuses.get(at).copied().flatten() == Some(Status::Failed);
        let segments = crate::suite::segments(&test.class).len();
        for depth in 0..segments {
            let entry = counts.entry(dir_prefix(&test.class, depth)).or_default();
            entry.0 += 1;
            entry.1 += u32::from(failed);
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
                let (tests, failed) = counts.get(prefix).copied().unwrap_or_default();
                rows.push(Row::Dir {
                    test: at,
                    depth,
                    expanded: open,
                    tests,
                    failed,
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
        if self.workspace == crate::ui::workspace::Workspace::Tests {
            return true;
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
            .is_some_and(|worktree| self.pest_runs.contains_key(worktree))
    }

    /// Launches a followed run — one target, or a campaign of several: "run
    /// everything" on a checkout carrying two runners is two commands, queued
    /// on the same worker and followed as one. The run panel comes forward,
    /// and the runs themselves go through the tests worker, never a frame.
    fn launch_tests(
        &mut self,
        label: SharedString,
        targets: Vec<Target>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(worktree) = self.active.clone() else {
            return;
        };
        if targets.is_empty() {
            return;
        }
        // One campaign at a time per worktree: the worker would only queue a
        // second one, and two accounts racing for the same dots would paint
        // whichever finished last.
        if self.pest_runs.get(&worktree).is_some_and(|run| run.running) {
            return;
        }
        let since = self.pest_run_seq + 1;
        let mut id = since;
        for target in targets {
            id = self.pest_run_seq + 1;
            self.pest_run_seq = id;
            self.git.send(Cmd::TestsRun {
                worktree: worktree.clone(),
                target,
                id,
            });
        }
        self.pest_runs.insert(
            worktree.clone(),
            RunState {
                since,
                id,
                label,
                running: true,
                started_at: now(),
                lines: VecDeque::new(),
                error: None,
                run: None,
            },
        );
        self.travel_reveal(crate::ui::workspace::Workspace::Tests, window, cx);
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
            gpui::ScrollStrategy::Center,
        );
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
        self.open_terminal(
            &worktree,
            crate::ui::terminal_view::Launch {
                command: Some((
                    "sh".into(),
                    vec!["-lc".into(), crate::suite::terminal_command(target)],
                )),
                env: HashMap::new(),
                label,
                agent: false,
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
                    .child(self.render_pest_bar(0, pending, only_failed, cx))
                    .child(failed_pest(message, cx))
                    .into_any_element();
            }
            Some(Report::Missing) => {
                return v_flex()
                    .size_full()
                    .child(self.render_pest_bar(0, pending, only_failed, cx))
                    .child(missing_pest(pending, cx))
                    .into_any_element();
            }
            None => Rc::new(Vec::new()),
        };

        let query = self.query(Pane::Tests, cx);
        let find = self.render_find(Pane::Tests, cx);
        let bar = self.render_pest_bar(tests.len(), pending, only_failed, cx);
        let state = self.pest.get(&active);
        let statuses = state.map(|s| s.statuses.clone()).unwrap_or_default();
        let labels = state.map(|s| s.labels.clone()).unwrap_or_default();
        let empty_folds = HashSet::new();
        let expanded = state.map(|s| &s.expanded).unwrap_or(&empty_folds);
        let rows: Rc<Vec<Row>> = Rc::new(rows(
            &tests,
            &labels,
            &statuses,
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
                                        index, &tests, &labels, &statuses, row, &look, &entity,
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
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let last = self
            .active
            .as_deref()
            .and_then(|worktree| self.pest.get(worktree))
            .and_then(|state| state.last_run.clone());
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
        h_flex()
            .h(crate::ui::theme::bar_height(cx))
            .w_full()
            .px_2()
            .gap_1()
            .items_center()
            .border_b_1()
            .border_color(cx.theme().border)
            .child(icon("circle-check").xsmall())
            .child(summary)
            .child(
                Button::new("pest-only-failed")
                    .ghost()
                    .xsmall()
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
            .child(
                Button::new("pest-run-all")
                    .ghost()
                    .xsmall()
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
                    .xsmall()
                    .icon(icon("refresh-cw"))
                    .tooltip(tr!("action-refresh"))
                    .disabled(pending)
                    .on_click(cx.listener(|this, _, _window, cx| {
                        if let Some(active) = this.active.clone() {
                            this.reload_pest(&active, cx);
                        }
                        cx.notify();
                    })),
            )
    }
}

/// What the theme gives a row, read once per frame and not per row.
#[derive(Clone, Copy)]
struct Look {
    row: gpui::Pixels,
    muted: gpui::Hsla,
    accent: gpui::Hsla,
    success: gpui::Hsla,
    danger: gpui::Hsla,
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
) -> gpui::AnyElement {
    let Row::Dir {
        test,
        depth,
        expanded,
        tests: under,
        failed,
    } = row
    else {
        return div().into_any_element();
    };
    let Some(first) = tests.get(test) else {
        return div().into_any_element();
    };
    let prefix = dir_prefix(&first.class, depth);
    let segment = prefix.rsplit('\\').next().unwrap_or(&prefix).to_string();
    let toggle = entity.clone();
    let run = entity.clone();
    let for_run = tests.clone();
    h_flex()
        .id(("pest-dir", index))
        .h(look.row)
        .w_full()
        .pl(gpui::px(6. + 14. * depth as f32))
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
                .xsmall()
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
    row: Row,
    look: &Look,
    entity: &Entity<ClaudhubApp>,
) -> gpui::AnyElement {
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
    let run = entity.clone();
    let menu = entity.clone();
    let (for_click, for_menu) = (tests.clone(), tests.clone());
    let menu_label = label.clone();
    h_flex()
        .id(("pest-row", index))
        .h(look.row)
        .w_full()
        .pl(gpui::px(6. + 14. * depth as f32))
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
        .child(icon(dot).xsmall().text_color(colour))
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
            Some(test) => row_menu(popup, &menu, test, &menu_label),
            None => popup,
        })
        .into_any_element()
}

fn row_menu(
    popup: PopupMenu,
    entity: &Entity<ClaudhubApp>,
    test: &Test,
    label: &SharedString,
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
        let line = crate::suite::terminal_command(&crate::suite::test_target(test));
        // For the terminal one already has open: the panel's gesture, portable.
        PopupMenuItem::new(tr!("tests-copy-filter"))
            .icon(icon("copy"))
            .on_click(move |_, _window, cx| {
                cx.write_to_clipboard(gpui::ClipboardItem::new_string(line.clone()));
            })
    })
}

/// Pest refused to list — a parse error in a test file, most days. Its
/// message is the content: it names the file and the line.
fn failed_pest(message: SharedString, cx: &App) -> gpui::AnyElement {
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
fn missing_pest(pending: bool, cx: &App) -> gpui::AnyElement {
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
fn empty_pest(query: &str, pending: bool, only_failed: bool, cx: &App) -> gpui::AnyElement {
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

        let bar = render_run_bar(state, cx);
        let with_failures = state.run.clone().filter(|run| run.failed > 0);
        let lines: Vec<SharedString> = state.lines.iter().cloned().collect();
        let failures = with_failures.map(|run| self.render_run_failures(&active, run, window, cx));
        let mono = cx.theme().mono_font_family.clone();
        let scroll = self.pest_run_scroll.clone();
        let count = lines.len();
        let lines = Rc::new(lines);
        let row = crate::ui::theme::row_height(cx);
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
                                    div()
                                        .h(row)
                                        .w_full()
                                        .px_2()
                                        .whitespace_nowrap()
                                        .text_xs()
                                        .font_family(mono.clone())
                                        .child(lines.get(index).cloned().unwrap_or_default())
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
    ) -> gpui::AnyElement {
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
                    .then(|| (worktree.join(&outcome.file), outcome.line));
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
fn render_run_bar(state: &RunState, cx: &App) -> gpui::AnyElement {
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
                    .text_color(cx.theme().muted_foreground)
                    .child(tr!("tests-running")),
            )
            .into_any_element();
    }
    if let Some(error) = &state.error {
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
                .text_color(cx.theme().muted_foreground)
                .child(SharedString::from(format!(
                    "{secs:.1}s · {}",
                    clock(state.started_at)
                ))),
        )
        .into_any_element()
}

#[cfg(test)]
mod tests {
    use super::*;

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
            rows(&tests, &labels, &statuses, "", &HashSet::new(), false),
            [
                Row::Dir {
                    test: 0,
                    depth: 0,
                    expanded: false,
                    tests: 3,
                    failed: 0
                },
                Row::Dir {
                    test: 3,
                    depth: 0,
                    expanded: false,
                    tests: 1,
                    failed: 0
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
        let shown = rows(&tests, &labels, &statuses, "", &expanded, false);
        assert_eq!(
            shown,
            [
                Row::Dir {
                    test: 0,
                    depth: 0,
                    expanded: true,
                    tests: 3,
                    failed: 0
                },
                Row::Dir {
                    test: 0,
                    depth: 1,
                    expanded: false,
                    tests: 2,
                    failed: 0
                },
                Row::Dir {
                    test: 2,
                    depth: 1,
                    expanded: false,
                    tests: 1,
                    failed: 0
                },
                Row::Dir {
                    test: 3,
                    depth: 0,
                    expanded: false,
                    tests: 1,
                    failed: 0
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
                    failed: 0
                },
                Row::Dir {
                    test: 3,
                    depth: 1,
                    expanded: true,
                    tests: 1,
                    failed: 0
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
        let shown = rows(&tests, &labels, &statuses, "", &HashSet::new(), true);
        assert_eq!(
            shown,
            [
                Row::Dir {
                    test: 1,
                    depth: 0,
                    expanded: true,
                    tests: 1,
                    failed: 1
                },
                Row::Dir {
                    test: 1,
                    depth: 1,
                    expanded: true,
                    tests: 1,
                    failed: 1
                },
                Row::Test { test: 1, depth: 2 },
            ]
        );
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
            },
            Test {
                runner: Runner::Pest,
                class: "LegacyTest".into(),
                method: "testOldSchool".into(),
                name: "testOldSchool".into(),
                pattern: String::new(),
                datasets: 0,
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
}
