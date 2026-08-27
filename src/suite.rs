//! What a worktree's test suites add to Claudhub: their tests, and the panels
//! that run one.
//!
//! Same bargain as the `justfile`, one domain over: a project already declares
//! its tests, and Claudhub lists them **without parsing its code** — the list
//! is each runner's own answer. Three runners, found by their binary in the
//! checkout: **Pest** (`vendor/bin/pest --list-tests`), **Vitest**
//! (`node_modules/.bin/vitest list --json`, one entry per test with the full
//! `describe > it` title and the file), and **Jest**
//! (`node_modules/.bin/jest --listTests`, files only — Jest cannot name its
//! tests without running them, so its rows are files and a run teaches the
//! rest). A checkout may carry several at once — a Laravel app with a Vitest
//! front — and the report merges them. The price is that listing boots PHP or
//! node — a second or two — which is why it runs on the background queue and
//! never on a frame.
//!
//! Running is `--filter` for Pest, `-t` (a regex against the space-joined
//! full title, the others reported "skipped") plus a path for Vitest and
//! Jest; the account is JUnit for Pest and Vitest, Jest's own `--json` for
//! Jest. Every shape was verified against the real tools — Pest 4, Vitest 4,
//! Jest 30.
//!
//! **What `--list-tests` prints is the mangled method name**, not the
//! description: `it('sums 2 + 2')` comes out as
//! `P\Tests\Unit\MathTest::__pest_evaluable_it_sums_2___2`. Pest's mangling
//! (`Str::evaluable`) is byte-wise: `_` doubles to `__`, then every byte
//! outside `[a-zA-Z0-9_\x80-\xff]` becomes one `_` — so UTF-8 survives and
//! each single `_` stands for exactly one lost byte. That is what makes the
//! name recoverable enough: `__` reads back as a literal underscore, a run of
//! `k` underscores as "between ⌈k/2⌉ and k bytes".
//!
//! **Running one test goes through `--filter`, which Pest matches against the
//! description**, not the method name: its `NameFilterIterator` override
//! builds `Class::description with data set "…"` and applies the filter as a
//! case-insensitive, byte-wise regex (`/…/i`, no `/u`). The mangled method
//! name never matches. So the filter is rebuilt from the mangled name with the
//! rule above — `.` for one lost byte, `.{⌈k/2⌉,k}` for a run — anchored on
//! `^Class::` and closed by an optional dataset suffix. Every shape here was
//! verified against Pest 4: plain tests, `describe` blocks (whose composed
//! description carries backticks the mangling swallowed), datasets, UTF-8, and
//! PHPUnit-style classes, which keep their real method name and match it
//! verbatim.
//!
//! Nothing here may be called from the interface thread: it launches a
//! process. It goes through a worker, on the background queue.

use std::path::Path;
use std::process::{Command, Stdio};
use std::time::Duration;

use anyhow::Result;

/// Beyond this, `pest` is killed. Listing loads every test file and boots the
/// framework — a second or two on a big Laravel suite — and this ceiling
/// exists for the one that never answers: a bootstrap waiting on a database
/// that is not up.
const TIMEOUT: Duration = Duration::from_secs(60);

/// The prefix Pest's `Str::evaluable` puts on every generated method name.
/// A method without it is a plain PHPUnit method living in the same suite.
const MANGLE_PREFIX: &str = "__pest_evaluable_";

/// A test runner a checkout may carry, found by its binary being there.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub enum Runner {
    Pest,
    Vitest,
    Jest,
}

impl Runner {
    pub const ALL: [Runner; 3] = [Runner::Pest, Runner::Vitest, Runner::Jest];

    /// Where the runner lives, relative to the checkout. Its being there is
    /// the whole detection: a devDependency not installed cannot run either.
    pub fn binary(self) -> &'static str {
        match self {
            Runner::Pest => "vendor/bin/pest",
            Runner::Vitest => "node_modules/.bin/vitest",
            Runner::Jest => "node_modules/.bin/jest",
        }
    }

    /// The word a run's label starts with.
    pub fn label(self) -> &'static str {
        match self {
            Runner::Pest => "pest",
            Runner::Vitest => "vitest",
            Runner::Jest => "jest",
        }
    }
}

/// What a run covers: everything, a folder, a file, or one test — said in
/// each runner's own words. Built by the panel's gestures, carried by the
/// command, turned into arguments here.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Target {
    pub runner: Runner,
    /// Pest: the whole `--filter` regex. JS: the `-t` regex, anchored on the
    /// space-joined full title.
    pub filter: Option<String>,
    /// JS: a file or folder that narrows the run, relative to the checkout.
    pub path: Option<String>,
    /// The path names one exact file — `--runTestsByPath` for Jest, where a
    /// bare path is a pattern whose dots match anything.
    pub exact: bool,
    /// Pest only: `--headed`, the flag its browser plugin reads to show the
    /// browser instead of running it headless. Off for the other runners —
    /// they never see the field.
    pub headed: bool,
    /// Pest only: `--parallel`. Never together with `headed` — the browser
    /// plugin refuses the pair — and the panel's toggles enforce it.
    pub parallel: bool,
}

impl Target {
    pub fn everything(runner: Runner) -> Self {
        Self {
            runner,
            filter: None,
            path: None,
            exact: false,
            headed: false,
            parallel: false,
        }
    }
}

/// One test, as the panel lists and runs it. Pest dataset entries are
/// collapsed: the row is the test, and running it runs every case.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Test {
    pub runner: Runner,
    /// Where the test sits in the tree. Pest: the printable class,
    /// `Tests\Unit\MathTest`, the `P\` prefix stripped because the filter is
    /// matched against the printable name. Vitest: the file's relative path —
    /// the file is the folder its tests sit in. Jest: the file's parent
    /// folder, since the row *is* the file; empty at the root.
    pub class: String,
    /// The key that pairs this test with a run's outcome. Pest: the mangled
    /// method name (`same_test`). Vitest: the full title as listed. Jest: the
    /// file's relative path.
    pub method: String,
    /// What the row shows. For a Pest test, the description read back from the
    /// mangled name — lost bytes become single spaces, so `it sums 2 + 2`
    /// shows as "it sums 2 2"; honest, and a run's account then supplies the
    /// real one. For Vitest, the full title; for Jest, the file's name.
    pub name: String,
    /// The regex core that narrows a run to this test, without anchors —
    /// Pest's `--filter` middle, or the escaped `-t` title for Vitest. Empty
    /// for a Jest file, whose path is the narrowing.
    pub pattern: String,
    /// How many dataset entries were collapsed into this row. Zero for a test
    /// without datasets.
    pub datasets: u32,
}

/// What asking a worktree for its tests answered.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum Report {
    /// No runner at all — no `vendor/bin/pest`, no `node_modules/.bin`
    /// vitest or jest. The panel disappears, which is the truth: there is
    /// nothing to run either.
    Missing,
    /// At least one runner is there and none could list — a parse error in a
    /// test file, a bootstrap that failed. The messages are the tools' own,
    /// shown in the panel with a way to retry.
    Failed(String),
    Tests(Vec<Test>),
}

/// What a worktree's suites declare, every present runner merged.
///
/// Never an error: a missing binary and a listing that failed are both
/// answers the panel shows, not conditions to unwrap. A runner failing while
/// another lists is logged and the listing shown — half a suite beats an
/// error page over the half that works.
pub fn report(worktree: &Path) -> Report {
    let mut found = false;
    let mut tests = Vec::new();
    let mut troubles = Vec::new();
    for runner in Runner::ALL {
        let bin = worktree.join(runner.binary());
        if !bin.is_file() {
            continue;
        }
        found = true;
        let listed = match runner {
            Runner::Pest => pest_list(worktree, &bin).map(|out| pest_parse(&out)),
            Runner::Vitest => vitest_list(worktree, &bin),
            Runner::Jest => jest_list(worktree, &bin),
        };
        match listed {
            Ok(mut listed) => tests.append(&mut listed),
            Err(e) => {
                log::warn!(
                    "{} listing in {}: {e:#}",
                    runner.label(),
                    worktree.display()
                );
                troubles.push(format!("{}: {e:#}", runner.label()));
            }
        }
    }
    if !found {
        return Report::Missing;
    }
    if tests.is_empty() && !troubles.is_empty() {
        return Report::Failed(troubles.join("\n\n"));
    }
    Report::Tests(tests)
}

/// One listing subprocess, whatever the runner: closed stdin, both streams
/// read, the ceiling applied — and the complaint built from both streams on
/// refusal.
fn listing(mut cmd: Command, what: &str) -> Result<String> {
    cmd
        // Closed, like git's and just's: a bootstrap deciding to read from
        // its input would hold the worker for good.
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        // Error messages we quote are read in English, for the same reason
        // the git layer reads them there. `NO_COLOR` is the convention node
        // tools follow.
        .env("LC_ALL", "C")
        .env("NO_COLOR", "1");
    crate::wsl::no_console(&mut cmd);
    let out = crate::git::wait_with_timeout(cmd, TIMEOUT, || what.to_string())?;
    if !out.status.success() {
        anyhow::bail!(
            "{}",
            complaint(
                &String::from_utf8_lossy(&out.stdout),
                &String::from_utf8_lossy(&out.stderr),
                &out.status.to_string(),
            )
        );
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

/// Laravel Sail, when the checkout carries it: PHP then lives in Sail's
/// containers, and the host's `vendor/bin/pest` may have no PHP — or the
/// wrong one — behind it. Every Pest command goes through `sail pest` there,
/// which proxies to `docker compose exec … php vendor/bin/pest`. Containers
/// down is an honest failure: the script answers "Sail is not running." and
/// the panel shows it.
const SAIL: &str = "vendor/bin/sail";

fn sail_of(worktree: &Path) -> Option<std::path::PathBuf> {
    let sail = worktree.join(SAIL);
    sail.is_file().then_some(sail)
}

/// The start of a Pest invocation for this checkout: `sail pest`, or the
/// binary bare.
fn pest_command(worktree: &Path, bin: &Path) -> Command {
    match sail_of(worktree) {
        Some(sail) => {
            let mut cmd = Command::new(sail);
            cmd.arg("pest");
            cmd
        }
        None => Command::new(bin),
    }
}

/// Asks Pest what the suite declares.
fn pest_list(worktree: &Path, bin: &Path) -> Result<String> {
    let mut cmd = pest_command(worktree, bin);
    cmd.arg("--list-tests")
        // Without it the `INFO` badge arrives wrapped in escape codes; the
        // test lines themselves happen to be clean, but parsing should not
        // depend on which lines Pest decides to decorate.
        .arg("--colors=never")
        .current_dir(worktree);
    listing(cmd, "pest --list-tests")
}

/// Asks Vitest: `vitest list --json` gives one entry per test, the full
/// `describe > it` title and the absolute file, datasets already unrolled
/// into their interpolated names. The file, made relative, is the folder the
/// test sits under in the tree.
fn vitest_list(worktree: &Path, bin: &Path) -> Result<Vec<Test>> {
    let mut cmd = Command::new(bin);
    cmd.arg("list").arg("--json").current_dir(worktree);
    let out = listing(cmd, "vitest list")?;
    Ok(vitest_rows(&out, worktree))
}

fn vitest_rows(json: &str, worktree: &Path) -> Vec<Test> {
    let entries: Vec<serde_json::Value> = serde_json::from_str(json.trim()).unwrap_or_default();
    entries
        .iter()
        .filter_map(|entry| {
            let name = entry.get("name")?.as_str()?.to_string();
            let file = entry.get("file")?.as_str()?;
            let file = Path::new(file)
                .strip_prefix(worktree)
                .ok()
                .and_then(|rel| rel.to_str())
                .unwrap_or(file)
                .to_string();
            Some(Test {
                runner: Runner::Vitest,
                class: file,
                method: name.clone(),
                pattern: title_pattern(&name),
                name,
                datasets: 0,
            })
        })
        .collect()
}

/// Asks Jest. `--listTests` only names files — Jest cannot enumerate tests
/// without running them — so a row is a file, and the first run teaches what
/// is inside.
fn jest_list(worktree: &Path, bin: &Path) -> Result<Vec<Test>> {
    let mut cmd = Command::new(bin);
    cmd.arg("--listTests").current_dir(worktree);
    let out = listing(cmd, "jest --listTests")?;
    Ok(jest_rows(&out, worktree))
}

fn jest_rows(output: &str, worktree: &Path) -> Vec<Test> {
    let mut tests: Vec<Test> = output
        .lines()
        .filter(|line| !line.trim().is_empty())
        .map(|line| {
            let rel = Path::new(line.trim())
                .strip_prefix(worktree)
                .ok()
                .and_then(|rel| rel.to_str())
                .unwrap_or(line.trim())
                .to_string();
            let (dir, file) = match rel.rsplit_once('/') {
                Some((dir, file)) => (dir.to_string(), file.to_string()),
                None => (String::new(), rel.clone()),
            };
            Test {
                runner: Runner::Jest,
                class: dir,
                method: rel,
                name: file,
                pattern: String::new(),
                datasets: 0,
            }
        })
        .collect();
    // Jest lists in its own scheduling order; the tree wants the file system's.
    tests.sort_by(|a, b| a.method.cmp(&b.method));
    tests
}

/// The `-t` regex core for a listed title: the display joins describes with
/// ` > `, the matched full name joins them with a single space, and the rest
/// is the title verbatim, regex characters escaped.
fn title_pattern(title: &str) -> String {
    let joined = title.replace(" > ", " ");
    let mut pattern = String::with_capacity(joined.len());
    for c in joined.chars() {
        if ".^$*+?()[]{}|\\/".contains(c) {
            pattern.push('\\');
        }
        pattern.push(c);
    }
    pattern
}

/// What a failed listing has to say, both streams read.
///
/// Pest writes its own failures to **stdout** (a test file that does not
/// parse, a bootstrap that threw), PHP writes to stderr — and stderr also
/// carries noise that is there on every run, like an ini loading Xdebug
/// twice. Showing only one stream picked the noise over the explanation:
/// that is exactly what happened, so both speak, stdout first. The exit
/// status stands in when neither said anything.
fn complaint(stdout: &str, stderr: &str, status: &str) -> String {
    let said = [stdout.trim(), stderr.trim()]
        .iter()
        .filter(|part| !part.is_empty())
        .copied()
        .collect::<Vec<_>>()
        .join("\n");
    if said.is_empty() {
        return status.to_string();
    }
    said
}

/// Reads the listing: one `Test` per test, dataset entries collapsed.
///
/// A line is ` - Class::method`, where `method` may carry a dataset suffix —
/// quoted (`"('App\X')"`, and the quoted name is free text: Pest truncates
/// long ones with `…`, and they may contain ` - ` or `::`) or PHPUnit's
/// unquoted ` with data set #0`. The mangled and PHPUnit method names
/// themselves contain neither space nor quote, so the suffix starts at the
/// first of the two. Pest ends the whole listing with a period glued to the
/// last entry; method names cannot contain `.`, so it is stripped without
/// risk.
fn pest_parse(output: &str) -> Vec<Test> {
    let entries: Vec<&str> = output
        .lines()
        .filter_map(|line| line.strip_prefix(" - "))
        .collect();
    let last = entries.len().saturating_sub(1);
    let mut tests: Vec<Test> = Vec::new();
    // (class, method) of each row already made, to fold dataset entries in.
    let mut seen: std::collections::HashMap<(String, String), usize> =
        std::collections::HashMap::new();
    for (at, mut entry) in entries.iter().copied().enumerate() {
        if at == last {
            if let Some(bare) = entry.strip_suffix('.') {
                entry = bare;
            }
        }
        let Some((class, method)) = entry.split_once("::") else {
            continue;
        };
        let class = class.strip_prefix("P\\").unwrap_or(class).to_string();
        let cut = method.find(['"', ' ']).unwrap_or(method.len());
        let (method, dataset) = (&method[..cut], cut < method.len());
        let key = (class.clone(), method.to_string());
        if let Some(&row) = seen.get(&key) {
            if dataset {
                tests[row].datasets += 1;
            }
            continue;
        }
        seen.insert(key, tests.len());
        tests.push(Test {
            runner: Runner::Pest,
            class,
            method: method.to_string(),
            name: display(method),
            pattern: pattern_of(method),
            datasets: u32::from(dataset),
        });
    }
    tests
}

/// The description read back from a mangled name, for the row.
///
/// Every underscore run becomes one space: a run mixes literal underscores
/// (doubled by the mangling) with lost bytes, and the two cannot be told
/// apart, so the display takes the reading that is right for descriptions —
/// which are sentences — and lets `it sums 2 + 2` show as "it sums 2 2". A
/// PHPUnit method name is shown as it is.
fn display(method: &str) -> String {
    let Some(desc) = method.strip_prefix(MANGLE_PREFIX) else {
        return method.to_string();
    };
    let name = desc
        .split('_')
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>()
        .join(" ");
    if name.is_empty() {
        // A description made only of swallowed bytes; show the raw name
        // rather than an empty row.
        return method.to_string();
    }
    name
}

/// The regex core matching this test's description, rebuilt from the mangled
/// name.
///
/// A run of `k` underscores stands for between ⌈k/2⌉ bytes (all literal
/// underscores, each doubled) and `k` bytes (all lost): `.{⌈k/2⌉,k}`, and a
/// bare `.` for the run of one. Everything else survived the mangling, and
/// what survives — `[a-zA-Z0-9\x80-\xff]` — contains no regex metacharacter,
/// so it passes verbatim; the match is byte-wise (no `/u`), which is exactly
/// how the mangling counted. A PHPUnit method name matches itself verbatim
/// for the same reason.
fn pattern_of(method: &str) -> String {
    let Some(desc) = method.strip_prefix(MANGLE_PREFIX) else {
        return method.to_string();
    };
    let mut pattern = String::with_capacity(desc.len());
    let bytes = desc.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'_' {
            let start = i;
            while i < bytes.len() && bytes[i] == b'_' {
                i += 1;
            }
            let k = i - start;
            if k == 1 {
                pattern.push('.');
            } else {
                pattern.push_str(&format!(".{{{},{k}}}", k.div_ceil(2)));
            }
        } else {
            let start = i;
            while i < bytes.len() && bytes[i] != b'_' {
                i += 1;
            }
            // `_` is ASCII, so these are char boundaries.
            pattern.push_str(&desc[start..i]);
        }
    }
    pattern
}

// — Running ————————————————————————————————————————————————
//
// A run is a subprocess too, but a **followed** one: alongside the text a
// human reads, `--log-junit` writes the machine's account — real
// descriptions, one entry per dataset case, the failure message with its
// `file:line`. That file is what turns "a terminal scrolled by" into a green
// or red dot per test, and it is Pest's own format, not a parsing of its
// screen.

/// Beyond this, a run is killed. A suite is measured in minutes; this ceiling
/// is for the one wedged on a dead database.
const RUN_TIMEOUT: Duration = Duration::from_secs(30 * 60);

/// The lines kept for a failure message. The end is where Pest writes the
/// failures and the summary; a runaway `dump()` loop can print millions.
const COMPLAINT_KEPT: usize = 200;

/// How one test ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum Status {
    Passed,
    Failed,
    Skipped,
}

/// One test's result, dataset cases folded — the row's dot, and what its
/// failure said.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Outcome {
    /// The class as the listing knows it (`Tests\Unit\MathTest`) — the JUnit
    /// carries it without the `P\` prefix, which is what makes the two match.
    pub class: String,
    /// The **real** description, read from the JUnit — where the listing only
    /// had the mangled name. "it sums 2 + 2", at last.
    pub name: String,
    pub status: Status,
    /// The first failure's message, empty otherwise.
    pub message: String,
    /// The test's file, relative to the worktree; empty when the JUnit had
    /// nothing that reads as a path.
    pub file: String,
    /// The line the failure points at, read from its message.
    pub line: Option<u32>,
    /// Dataset cases folded into this row; 1 for a plain test.
    pub cases: u32,
    pub time_ms: u64,
}

/// One run, as the panel shows it: the counts and each test's outcome. The
/// text a human reads travelled already, line by line, while the run went.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Run {
    pub passed: u32,
    pub failed: u32,
    pub skipped: u32,
    pub duration_ms: u64,
    pub outcomes: Vec<Outcome>,
}

/// Names the temporary JUnit file: the worker is single-threaded, but two
/// windows are two processes, and a second Claudhub must not read the first
/// one's half-written file.
static RUN_SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// The highest run id asked to stop. **Outside the queues**, like the watcher
/// and the language servers, and for their reason: a stop has to reach a
/// worker already busy with the very run it names — queued behind it, it
/// would arrive after the death it asks for. Send ids only grow, so "stop
/// everything up to this id" also empties the campaign's queued remainder,
/// which each refuses on arrival.
static STOP_BELOW: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// Asks every run up to `id` to stop — the one in flight is killed at the
/// next poll, the queued ones refuse as they arrive.
pub fn request_stop(id: u64) {
    STOP_BELOW.fetch_max(id, std::sync::atomic::Ordering::Relaxed);
}

fn stop_requested(id: u64) -> bool {
    STOP_BELOW.load(std::sync::atomic::Ordering::Relaxed) >= id
}

/// What a stopped run answers: recognisable, and shown as it is.
pub const STOPPED: &str = "interrupted";

/// Runs what the target names, saying each line as it comes, and reads the
/// account at the end.
///
/// A failing test is a **successful run**: every runner exits non-zero the
/// moment one test fails, and that is the result, not an error. The error
/// case is the run that produced no account at all — the interpreter
/// missing, a suite that will not boot — and then the streams speak, as for
/// the listing.
///
/// The lines go out through `progress` while the process runs, the shape
/// `wt`'s hooks already have: a suite is measured in minutes, and a panel
/// saying nothing for minutes reads as hung. Both streams are followed —
/// Jest narrates on **stderr** — and stripped of their colours.
pub fn run(
    worktree: &Path,
    target: &Target,
    id: u64,
    progress: &(dyn Fn(String) + Sync),
) -> std::result::Result<Run, String> {
    // A campaign stopped while this command still queued: refuse on arrival
    // rather than boot a suite nobody is waiting for.
    if stop_requested(id) {
        return Err(STOPPED.to_string());
    }
    let bin = worktree.join(target.runner.binary());
    if !bin.is_file() {
        return Err(format!("{} is missing", target.runner.binary()));
    }
    let account_name = format!(
        ".claudhub-tests-{}-{}.{}",
        std::process::id(),
        RUN_SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed),
        match target.runner {
            Runner::Jest => "json",
            _ => "xml",
        }
    );
    // Through Sail the suite runs in a container whose /tmp is not the
    // host's: the one directory both sides share is the project itself, so
    // the account lands there, dot-named and passed **relative** — the
    // container's working directory is the project's mount. Removed right
    // after, and `reloads` ignores it, so the watcher never re-lists over it.
    let sail = target.runner == Runner::Pest && sail_of(worktree).is_some();
    let (account_file, account_arg) = if sail {
        (worktree.join(&account_name), account_name)
    } else {
        let file = std::env::temp_dir().join(&account_name);
        let arg = file.display().to_string();
        (file, arg)
    };
    let mut cmd = match target.runner {
        Runner::Pest => pest_command(worktree, &bin),
        _ => Command::new(&bin),
    };
    cmd.current_dir(worktree);
    match target.runner {
        Runner::Pest => {
            cmd.arg("--colors=never")
                .arg("--log-junit")
                .arg(&account_arg);
            if target.headed {
                cmd.arg("--headed");
            }
            if target.parallel {
                cmd.arg("--parallel");
            }
            if let Some(filter) = &target.filter {
                cmd.arg("--filter").arg(filter);
            }
        }
        Runner::Vitest => {
            cmd.arg("run")
                .arg("--reporter=default")
                .arg("--reporter=junit")
                .arg(format!("--outputFile.junit={account_arg}"));
            if let Some(path) = &target.path {
                cmd.arg(path);
            }
            if let Some(filter) = &target.filter {
                cmd.arg("-t").arg(filter);
            }
        }
        Runner::Jest => {
            cmd.arg("--json").arg("--outputFile").arg(&account_arg);
            if let Some(path) = &target.path {
                if target.exact {
                    cmd.arg("--runTestsByPath");
                }
                cmd.arg(path);
            }
            if let Some(filter) = &target.filter {
                cmd.arg("-t").arg(filter);
            }
        }
    }
    let result = follow(cmd, target.runner.label(), id, progress);
    let account = std::fs::read_to_string(&account_file).unwrap_or_default();
    let _ = std::fs::remove_file(&account_file);
    let (status, kept, stderr, duration_ms) = result?;
    let mut outcomes = match target.runner {
        Runner::Pest | Runner::Vitest => parse_junit(&account),
        Runner::Jest => parse_jest(&account, worktree),
    };
    // A JS `-t` run reports every non-matching test as "skipped": those are
    // not fates, and keeping them would grey out — and outnumber — the one
    // test actually run. Pest's account only carries what matched.
    if target.runner != Runner::Pest && target.filter.is_some() {
        outcomes.retain(|outcome| outcome.status != Status::Skipped);
    }
    if outcomes.is_empty() {
        // No account: the suite never ran. A filter finding nothing is the
        // one benign shape — the runner then reports nothing and says "no
        // tests found" on its streams, which the message passes on.
        return Err(complaint(&kept.join("\n"), &stderr, &status));
    }
    let (mut passed, mut failed, mut skipped) = (0, 0, 0);
    for outcome in &outcomes {
        match outcome.status {
            Status::Passed => passed += 1,
            Status::Failed => failed += 1,
            Status::Skipped => skipped += 1,
        }
    }
    Ok(Run {
        passed,
        failed,
        skipped,
        duration_ms,
        outcomes,
    })
}

/// The subprocess itself: spawned, followed line by line on **both** streams,
/// killed at the ceiling. Returns the status, the last narrated lines (for
/// the message when there is no account), stderr's tail, and the wall-clock
/// time.
///
/// The lines travel through a channel rather than being read in place — the
/// reason `git`'s streaming does the same: a read blocked on a command that
/// says nothing would have no ceiling, and the ceiling is the point.
#[allow(clippy::type_complexity)]
fn follow(
    mut cmd: Command,
    what: &str,
    id: u64,
    progress: &(dyn Fn(String) + Sync),
) -> std::result::Result<(String, Vec<String>, String, u64), String> {
    use std::io::BufRead;

    let started = std::time::Instant::now();
    cmd.stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .env("LC_ALL", "C")
        // The convention node tools follow; Jest still colours a piped
        // stderr without it.
        .env("NO_COLOR", "1");
    crate::wsl::no_console(&mut cmd);
    let mut child = cmd.spawn().map_err(|e| format!("{what}: {e}"))?;

    let (lines, incoming) = std::sync::mpsc::sync_channel::<(bool, String)>(256);
    let mut readers = Vec::new();
    let stdout = child.stdout.take().expect("stdout requested as piped");
    let stderr = child.stderr.take().expect("stderr requested as piped");
    for (is_err, stream) in [
        (false, Box::new(stdout) as Box<dyn std::io::Read + Send>),
        (true, Box::new(stderr) as Box<dyn std::io::Read + Send>),
    ] {
        let lines = lines.clone();
        readers.push(std::thread::spawn(move || {
            for line in std::io::BufReader::new(stream).split(b'\n') {
                let Ok(line) = line else { break };
                let mut line = String::from_utf8_lossy(&line).into_owned();
                if line.ends_with('\r') {
                    line.pop();
                }
                if lines.send((is_err, line)).is_err() {
                    break;
                }
            }
        }));
    }
    drop(lines);

    let deadline = started + RUN_TIMEOUT;
    let mut kept: std::collections::VecDeque<String> = std::collections::VecDeque::new();
    let mut err_tail: std::collections::VecDeque<String> = std::collections::VecDeque::new();
    // The wait wakes at least four times a second even when the suite says
    // nothing: that poll is what lets the stop button reach a worker whose
    // whole queue is this very run.
    const POLL: Duration = Duration::from_millis(250);
    let ended = loop {
        if stop_requested(id) {
            break Some(STOPPED.to_string());
        }
        let now = std::time::Instant::now();
        if now >= deadline {
            break Some(format!(
                "{what} did not answer within {RUN_TIMEOUT:?} and was interrupted"
            ));
        }
        match incoming.recv_timeout((deadline - now).min(POLL)) {
            Ok((is_err, line)) => {
                let line = strip_ansi(&line);
                let tail = if is_err { &mut err_tail } else { &mut kept };
                if tail.len() == COMPLAINT_KEPT {
                    tail.pop_front();
                }
                tail.push_back(line.clone());
                progress(line);
            }
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => break None,
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => continue,
        }
    };
    if let Some(why) = ended {
        let _ = child.kill();
        let _ = child.wait();
        for reader in readers {
            let _ = reader.join();
        }
        return Err(why);
    }
    for reader in readers {
        let _ = reader.join();
    }
    // The streams are closed: the process is exiting. The bounded wait is for
    // the one that closed its outputs and wedged anyway.
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) => {
                if std::time::Instant::now() >= deadline {
                    let _ = child.kill();
                    let _ = child.wait();
                    return Err(format!(
                        "{what} did not answer within {RUN_TIMEOUT:?} and was interrupted"
                    ));
                }
                std::thread::sleep(Duration::from_millis(10));
            }
            Err(e) => return Err(format!("waiting on {what}: {e}")),
        }
    };
    Ok((
        status.to_string(),
        kept.into(),
        Vec::from(err_tail).join("\n"),
        started.elapsed().as_millis() as u64,
    ))
}

/// Colours out of a narrated line: the run is followed through a pipe, and
/// Jest paints its stderr even there.
fn strip_ansi(line: &str) -> String {
    let mut out = String::with_capacity(line.len());
    let mut chars = line.chars().peekable();
    while let Some(c) = chars.next() {
        if c != '\u{1b}' {
            out.push(c);
            continue;
        }
        // ESC [ … final-byte, or a lone two-byte escape.
        if chars.peek() == Some(&'[') {
            chars.next();
            for c in chars.by_ref() {
                if ('\u{40}'..='\u{7e}').contains(&c) {
                    break;
                }
            }
        } else {
            chars.next();
        }
    }
    out
}

/// Reads the JUnit account: one `Outcome` per test, dataset cases folded.
///
/// Every `<testcase>` is one case; its dataset suffix (` with data set …`) is
/// PHPUnit's, and stripping it is what folds a dataset back into its test.
/// A case that failed makes the test failed; skipped only counts when
/// nothing ran.
fn parse_junit(xml: &str) -> Vec<Outcome> {
    let Ok(doc) = roxmltree::Document::parse(xml) else {
        return Vec::new();
    };
    let mut outcomes: Vec<Outcome> = Vec::new();
    let mut seen: std::collections::HashMap<(String, String), usize> =
        std::collections::HashMap::new();
    for case in doc
        .descendants()
        .filter(|node| node.has_tag_name("testcase"))
    {
        let Some(full_name) = case.attribute("name") else {
            continue;
        };
        // Pest writes `class` (and a dotted `classname` beside it); Vitest
        // only writes `classname`, which is the file's relative path.
        let Some(class) = case
            .attribute("class")
            .or_else(|| case.attribute("classname"))
        else {
            continue;
        };
        let name = full_name
            .split(" with data set ")
            .next()
            .unwrap_or(full_name)
            .to_string();
        let mut status = Status::Passed;
        let mut message = String::new();
        for child in case.children() {
            match child.tag_name().name() {
                "failure" | "error" => {
                    status = Status::Failed;
                    message = child.text().unwrap_or_default().trim().to_string();
                    break;
                }
                "skipped" => status = Status::Skipped,
                _ => {}
            }
        }
        let time_ms = case
            .attribute("time")
            .and_then(|time| time.parse::<f64>().ok())
            .map(|secs| (secs * 1000.) as u64)
            .unwrap_or(0);
        // `file` reads `tests/Unit/MathTest.php::it sums 2 + 2` for a Pest
        // test — and `Legacy::Old school` for a PHPUnit class, which is no
        // path and is dropped. Vitest writes no `file` at all: its class is
        // the file, told apart by its separators.
        let file = case
            .attribute("file")
            .map(|file| file.split("::").next().unwrap_or(file))
            .filter(|file| file.ends_with(".php"))
            .or_else(|| Some(class).filter(|class| class.contains('/')))
            .unwrap_or_default()
            .to_string();
        let key = (class.to_string(), name.clone());
        if let Some(&row) = seen.get(&key) {
            let test = &mut outcomes[row];
            test.cases += 1;
            test.time_ms += time_ms;
            // A failing case fails the test; a skipped one only says so when
            // nothing else ran.
            match status {
                Status::Failed if test.status != Status::Failed => {
                    test.status = Status::Failed;
                    test.line = failure_line(&message);
                    test.message = message;
                }
                Status::Passed if test.status == Status::Skipped => {
                    test.status = Status::Passed;
                }
                _ => {}
            }
            continue;
        }
        seen.insert(key, outcomes.len());
        let line = failure_line(&message);
        outcomes.push(Outcome {
            class: class.to_string(),
            name,
            status,
            message,
            file,
            line,
            cases: 1,
            time_ms,
        });
    }
    outcomes
}

/// The line a failure points at, read from its message — the JUnit has no
/// attribute for it, but Pest writes `at tests/Feature/HttpTest.php:3` and
/// Vitest `❯ src/http.test.js:3:46` in the text.
fn failure_line(message: &str) -> Option<u32> {
    message.lines().find_map(|line| {
        let line = line.trim();
        let rest = line
            .strip_prefix("at ")
            .or_else(|| line.strip_prefix("❯ "))?;
        let mut parts = rest.rsplitn(3, ':');
        let last = parts.next()?;
        if let Some(middle) = parts.next() {
            // `path:line:col` — the line is the middle number.
            if last.bytes().all(|b| b.is_ascii_digit()) {
                if let Ok(line) = middle.parse() {
                    return Some(line);
                }
            }
        }
        last.parse().ok()
    })
}

/// Reads Jest's `--json` account: one `Outcome` per test, real titles and
/// the failure's own message. The file paths come back absolute and are made
/// relative, since the file is also the row the fates land on.
fn parse_jest(json: &str, worktree: &Path) -> Vec<Outcome> {
    let Ok(root) = serde_json::from_str::<serde_json::Value>(json) else {
        return Vec::new();
    };
    let Some(files) = root.get("testResults").and_then(|list| list.as_array()) else {
        return Vec::new();
    };
    let mut outcomes = Vec::new();
    for file in files {
        let Some(path) = file.get("name").and_then(|name| name.as_str()) else {
            continue;
        };
        let rel = Path::new(path)
            .strip_prefix(worktree)
            .ok()
            .and_then(|rel| rel.to_str())
            .unwrap_or(path)
            .to_string();
        let Some(cases) = file
            .get("assertionResults")
            .and_then(|list| list.as_array())
        else {
            continue;
        };
        for case in cases {
            let Some(name) = case.get("fullName").and_then(|name| name.as_str()) else {
                continue;
            };
            let status = match case.get("status").and_then(|status| status.as_str()) {
                Some("passed") => Status::Passed,
                Some("failed") => Status::Failed,
                // "pending", "todo", "skipped", "disabled" — not run.
                _ => Status::Skipped,
            };
            let message = case
                .get("failureMessages")
                .and_then(|list| list.as_array())
                .and_then(|list| list.first())
                .and_then(|message| message.as_str())
                .map(|message| strip_ansi(message.trim()))
                .unwrap_or_default();
            // The stack ends `(…/src/http.test.js:2:53)`: the line follows
            // the file's own name.
            let line = message
                .find(&rel)
                .and_then(|at| message[at + rel.len()..].strip_prefix(':'))
                .map(|rest| {
                    rest.bytes()
                        .take_while(|b| b.is_ascii_digit())
                        .fold(0u32, |n, b| n * 10 + u32::from(b - b'0'))
                })
                .filter(|line| *line > 0);
            outcomes.push(Outcome {
                class: rel.clone(),
                name: name.to_string(),
                status,
                message,
                file: rel.clone(),
                line,
                cases: 1,
                time_ms: case
                    .get("duration")
                    .and_then(|duration| duration.as_u64())
                    .unwrap_or(0),
            });
        }
    }
    outcomes
}

/// Pest's `Str::evaluable`, without its prefix: what a description becomes as
/// a method name. Byte-wise, like the original — underscores double first,
/// then spaces and every other lost byte become one `_` each.
fn mangled(description: &str) -> String {
    let doubled = description.replace('_', "__");
    let bytes = doubled
        .bytes()
        .map(|byte| match byte {
            b'_' | b'0'..=b'9' | b'a'..=b'z' | b'A'..=b'Z' | 0x80.. => byte,
            _ => b'_',
        })
        .collect::<Vec<u8>>();
    // Only ASCII bytes were replaced, so the UTF-8 sequences are intact.
    String::from_utf8(bytes).expect("ASCII-only replacement keeps UTF-8 valid")
}

/// Is this outcome the listed test? The listing has the mangled method name,
/// the account the real description: mangling the description back is exact
/// for a Pest test. A PHPUnit method is compared loosely — its account name
/// is prettified (`testOldSchool` reads "Old school") and only the letters
/// survive that.
pub fn same_test(method: &str, account_name: &str) -> bool {
    match method.strip_prefix(MANGLE_PREFIX) {
        Some(rest) => mangled(account_name) == rest,
        None => loose(method.strip_prefix("test").unwrap_or(method)) == loose(account_name),
    }
}

/// Lowercase alphanumerics, everything else dropped.
fn loose(name: &str) -> String {
    name.chars()
        .filter(|c| c.is_alphanumeric())
        .flat_map(char::to_lowercase)
        .collect()
}

/// The `--filter` argument that runs every test of one class — one file, in
/// Pest's world.
///
/// `^` pins the class: unanchored, `MathTest::` would also run
/// `OtherMathTest`. Pest wraps the filter in `/…/i` itself; `^` cannot be
/// taken for a regex delimiter since the pattern holds no second one.
pub fn class_filter(class: &str) -> String {
    format!("^{}::", escape_class(class))
}

/// The `--filter` argument that runs one test, dataset cases included.
///
/// Anchored on both sides: without `$`, "it saves" would also run "it saves
/// twice". The dataset suffix is optional because the compared name carries
/// one exactly when the test has datasets.
pub fn test_filter(test: &Test) -> String {
    format!(
        "^{}::{}( with data set .*)?$",
        escape_class(&test.class),
        test.pattern
    )
}

/// The `--filter` argument that runs everything under one folder of the tree
/// — `Unit`, or `TenantFeature\Forms`. `short_depth` names the folder as a
/// segment count of the **short** class (`Tests\` dropped), and `class` is
/// any class under it, whose full name restores the hidden prefix.
pub fn scope_filter(class: &str, short_depth: usize) -> String {
    let segments: Vec<&str> = class.split('\\').collect();
    let offset = segments.len() - short_class(class).split('\\').count();
    let end = (offset + short_depth + 1).min(segments.len());
    let prefix = segments[..end].join("\\");
    if prefix == class {
        return class_filter(class);
    }
    format!("^{}\\\\", escape_class(&prefix))
}

/// A PHP class name in a regex: only `\` needs escaping — the rest of what a
/// class name may hold is not special.
fn escape_class(class: &str) -> String {
    class.replace('\\', "\\\\")
}

/// The class as the panel's group header shows it: the `Tests\` every Pest
/// suite starts with says nothing, `Unit\MathTest` is what one scans for. A
/// JS path passes through.
pub fn short_class(class: &str) -> &str {
    class.strip_prefix("Tests\\").unwrap_or(class)
}

/// The tree's segments of a group: Pest namespaces split on `\`, JS paths on
/// `/`. Empty for a Jest file at the checkout's root, which sits under no
/// folder at all.
pub fn segments(group: &str) -> Vec<&str> {
    let short = short_class(group);
    if short.is_empty() {
        return Vec::new();
    }
    short.split(['\\', '/']).collect()
}

/// The folder a depth names, written as the (short) group writes it —
/// separators kept, so a JS prefix is a real path.
pub fn group_prefix(group: &str, depth: usize) -> &str {
    let short = short_class(group);
    let mut seen = 0;
    for (i, c) in short.char_indices() {
        if c == '\\' || c == '/' {
            if seen == depth {
                return &short[..i];
            }
            seen += 1;
        }
    }
    short
}

/// Does a run's target cover this listed test? What the tree shows as
/// loading while the run goes. The claim is structural — undone from the
/// same builders that made the target — rather than the regexes played
/// back: a filter is either the one `test_filter` writes for this very
/// test, or a `^prefix` of the escaped `Class::`, which is what
/// `class_filter` and `scope_filter` both emit.
pub fn covers(target: &Target, test: &Test) -> bool {
    if target.runner != test.runner {
        return false;
    }
    match target.runner {
        Runner::Pest => match &target.filter {
            None => true,
            Some(filter) => {
                filter == &test_filter(test)
                    || filter.strip_prefix('^').is_some_and(|body| {
                        format!("{}::", escape_class(&test.class)).starts_with(body)
                    })
            }
        },
        Runner::Vitest => {
            let in_path = match &target.path {
                None => true,
                Some(path) => test.class == *path || test.class.starts_with(&format!("{path}/")),
            };
            in_path
                && match &target.filter {
                    None => true,
                    Some(filter) => filter == &format!("^{}$", test.pattern),
                }
        }
        // A Jest row is a file; a target narrows by path or takes all.
        Runner::Jest => match &target.path {
            None => true,
            Some(path) => test.method == *path || test.method.starts_with(&format!("{path}/")),
        },
    }
}

/// What runs one listed test, in its runner's words.
pub fn test_target(test: &Test) -> Target {
    match test.runner {
        Runner::Pest => Target {
            runner: Runner::Pest,
            filter: Some(test_filter(test)),
            path: None,
            exact: false,
            headed: false,
            parallel: false,
        },
        // The file narrows the collection, the anchored title picks the test.
        Runner::Vitest => Target {
            runner: Runner::Vitest,
            filter: Some(format!("^{}$", test.pattern)),
            path: Some(test.class.clone()),
            exact: true,
            headed: false,
            parallel: false,
        },
        // A Jest row is a file: the path is the whole narrowing.
        Runner::Jest => Target {
            runner: Runner::Jest,
            filter: None,
            path: Some(test.method.clone()),
            exact: true,
            headed: false,
            parallel: false,
        },
    }
}

/// What runs everything under one folder of the tree, named by any test
/// under it and the folder's depth.
pub fn scope_target(test: &Test, depth: usize) -> Target {
    match test.runner {
        Runner::Pest => Target {
            runner: Runner::Pest,
            filter: Some(scope_filter(&test.class, depth)),
            path: None,
            exact: false,
            headed: false,
            parallel: false,
        },
        Runner::Vitest => {
            let prefix = group_prefix(&test.class, depth).to_string();
            // The deepest folder is the file itself.
            let exact = prefix == test.class;
            Target {
                runner: Runner::Vitest,
                filter: None,
                path: Some(prefix),
                exact,
                headed: false,
                parallel: false,
            }
        }
        Runner::Jest => Target {
            runner: Runner::Jest,
            filter: None,
            path: Some(group_prefix(&test.class, depth).to_string()),
            exact: false,
            headed: false,
            parallel: false,
        },
    }
}

/// The line a terminal runs for the same target — where Ctrl+C exists, and
/// where an interactive suite can ask. The Sail detour is decided here too,
/// on the same file the worker reads.
pub fn terminal_command(worktree: &Path, target: &Target) -> String {
    command_line(sail_of(worktree).is_some(), target)
}

fn command_line(sail: bool, target: &Target) -> String {
    let mut parts: Vec<String> = if sail && target.runner == Runner::Pest {
        vec![SAIL.to_string(), "pest".to_string()]
    } else {
        vec![target.runner.binary().to_string()]
    };
    match target.runner {
        Runner::Pest => {
            if target.headed {
                parts.push("--headed".into());
            }
            if target.parallel {
                parts.push("--parallel".into());
            }
            if let Some(filter) = &target.filter {
                parts.push("--filter".into());
                parts.push(filter.clone());
            }
        }
        Runner::Vitest => {
            parts.push("run".into());
            if let Some(path) = &target.path {
                parts.push(path.clone());
            }
            if let Some(filter) = &target.filter {
                parts.push("-t".into());
                parts.push(filter.clone());
            }
        }
        Runner::Jest => {
            if let Some(path) = &target.path {
                if target.exact {
                    parts.push("--runTestsByPath".into());
                }
                parts.push(path.clone());
            }
            if let Some(filter) = &target.filter {
                parts.push("-t".into());
                parts.push(filter.clone());
            }
        }
    }
    crate::cmdline::join_command(parts)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `pest --list-tests --colors=never` on a suite carrying every shape met
    /// in the wild: a plain test, UTF-8, a `describe` block, datasets, a
    /// PHPUnit class, and the period Pest glues to the last entry.
    const LISTING: &str = "\n   INFO  Available tests:\n\n - LegacyTest::testOldSchool\n - P\\Tests\\Unit\\MathTest::__pest_evaluable_it_sums_2___2\n - P\\Tests\\Unit\\MathTest::__pest_evaluable_la_casse_UTF_8___éléphant\n - P\\Tests\\Unit\\MathTest::__pest_evaluable__inside_a_describe__→_it_nests_fine\n - P\\Tests\\Unit\\MathTest::__pest_evaluable_it_runs_on_datasets\"(1)\"\n - P\\Tests\\Unit\\MathTest::__pest_evaluable_it_runs_on_datasets\"(2)\"\n - P\\Tests\\Feature\\HttpTest::__pest_evaluable_it_answers.\n";

    #[test]
    fn the_listing_gives_one_row_per_test() {
        let tests = pest_parse(LISTING);
        let names: Vec<&str> = tests.iter().map(|t| t.name.as_str()).collect();
        assert_eq!(
            names,
            [
                "testOldSchool",
                "it sums 2 2",
                "la casse UTF 8 éléphant",
                "inside a describe → it nests fine",
                "it runs on datasets",
                "it answers",
            ]
        );
        // The `P\` prefix is stripped, the PHPUnit class kept as it is.
        assert_eq!(tests[0].class, "LegacyTest");
        assert_eq!(tests[1].class, "Tests\\Unit\\MathTest");
        // Two dataset entries folded into one row.
        assert_eq!(tests[4].datasets, 2);
        assert_eq!(tests[1].datasets, 0);
        // The final period belongs to Pest's sentence, not to the last test.
        assert_eq!(tests[5].name, "it answers");
    }

    /// Each filter was run against Pest 4 itself; these pin the exact strings.
    #[test]
    fn a_test_is_run_by_an_anchored_description_regex() {
        let tests = pest_parse(LISTING);
        // ` + ` was mangled to three underscores: two to three bytes.
        assert_eq!(
            test_filter(&tests[1]),
            "^Tests\\\\Unit\\\\MathTest::it.sums.2.{2,3}2( with data set .*)?$"
        );
        // A `describe` composes the description with backticks the mangling
        // swallowed: one byte before, two between, one after the arrow.
        assert_eq!(
            test_filter(&tests[3]),
            "^Tests\\\\Unit\\\\MathTest::.inside.a.describe.{1,2}→.it.nests.fine( with data set .*)?$"
        );
        // A PHPUnit method matches itself verbatim.
        assert_eq!(
            test_filter(&tests[0]),
            "^LegacyTest::testOldSchool( with data set .*)?$"
        );
        // Datasets ride the optional suffix.
        assert_eq!(
            test_filter(&tests[4]),
            "^Tests\\\\Unit\\\\MathTest::it.runs.on.datasets( with data set .*)?$"
        );
    }

    #[test]
    fn a_class_filter_pins_the_class() {
        assert_eq!(
            class_filter("Tests\\Unit\\MathTest"),
            "^Tests\\\\Unit\\\\MathTest::"
        );
        assert_eq!(class_filter("LegacyTest"), "^LegacyTest::");
    }

    /// A quoted dataset name is free text — Pest truncates long ones with `…`,
    /// and nothing stops one from containing ` - ` or `::`. The suffix starts
    /// at the first quote, and what follows never splits the row.
    #[test]
    fn a_dataset_name_is_free_text() {
        let tests = pest_parse(
            " - P\\Tests\\Unit\\ExceptionsTest::__pest_evaluable_it_builds\"('App\\Exceptions\\FlowinApiNotAc…eption - x::y')\"\n - P\\Tests\\Unit\\ExceptionsTest::__pest_evaluable_it_builds\"('App\\Exceptions\\Other')\"\n",
        );
        assert_eq!(tests.len(), 1);
        assert_eq!(tests[0].name, "it builds");
        assert_eq!(tests[0].datasets, 2);
    }

    /// PHPUnit's own providers suffix ` with data set #0`, unquoted: a space
    /// cuts the method as a quote does.
    #[test]
    fn a_phpunit_provider_is_folded_too() {
        let tests = pest_parse(
            " - LegacyTest::testCases with data set #0\n - LegacyTest::testCases with data set #1.\n",
        );
        assert_eq!(tests.len(), 1);
        assert_eq!(tests[0].name, "testCases");
        assert_eq!(tests[0].datasets, 2);
    }

    #[test]
    fn what_is_not_a_test_line_is_ignored() {
        assert!(pest_parse("   INFO  No tests found.\n").is_empty());
        assert!(pest_parse("").is_empty());
    }

    /// The failure the panel quotes must be the explanation, not the noise:
    /// stderr carries lines that are there on every run — an ini loading
    /// Xdebug twice — while Pest explains itself on stdout. Both are shown,
    /// stdout first, and the exit status only when neither spoke.
    #[test]
    fn a_failure_quotes_both_streams_before_the_status() {
        assert_eq!(
            complaint(
                "  ERROR  ParseError in tests/Unit/BrokenTest.php  ",
                "Cannot load Xdebug - it was already loaded",
                "exit status: 1",
            ),
            "ERROR  ParseError in tests/Unit/BrokenTest.php\nCannot load Xdebug - it was already loaded"
        );
        assert_eq!(
            complaint("", "PHP Fatal error: out of memory", "exit status: 255"),
            "PHP Fatal error: out of memory"
        );
        assert_eq!(complaint("", "  ", "exit status: 139"), "exit status: 139");
    }

    /// `pest --log-junit` on the same suite: real descriptions, one entry per
    /// dataset case, the failure's message carrying its `file:line`.
    const ACCOUNT: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<testsuites>
  <testsuite name="Unit" tests="6">
    <testsuite name="LegacyTest" file="Legacy">
      <testcase name="Old school" file="Legacy::Old school" class="LegacyTest" classname="LegacyTest" assertions="1" time="0.000947"/>
    </testsuite>
    <testsuite name="Tests\Unit\MathTest" file="tests/Unit/MathTest.php">
      <testcase name="it sums 2 + 2" file="tests/Unit/MathTest.php::it sums 2 + 2" class="Tests\Unit\MathTest" classname="Tests.Unit.MathTest" time="0.004124"/>
      <testcase name="la casse UTF-8 : éléphant" file="tests/Unit/MathTest.php::u" class="Tests\Unit\MathTest" time="0.000100"/>
      <testcase name="`inside a describe` &#8594; it nests fine" file="tests/Unit/MathTest.php::x" class="Tests\Unit\MathTest" time="0.000213"/>
      <testsuite name="it runs on datasets">
        <testcase name="it runs on datasets with data set &quot;(1)&quot;" file="tests/Unit/MathTest.php::y" class="Tests\Unit\MathTest" time="0.000252"/>
        <testcase name="it runs on datasets with data set &quot;(2)&quot;" file="tests/Unit/MathTest.php::y" class="Tests\Unit\MathTest" time="0.000201"/>
      </testsuite>
      <testcase name="it is skipped" file="tests/Unit/MathTest.php::z" class="Tests\Unit\MathTest" time="0.001594">
        <skipped/>
      </testcase>
    </testsuite>
  </testsuite>
  <testsuite name="Feature" tests="2">
    <testsuite name="Tests\Feature\HttpTest" file="tests/Feature/HttpTest.php">
      <testcase name="it answers" file="tests/Feature/HttpTest.php::it answers" class="Tests\Feature\HttpTest" time="0.001734"/>
      <testcase name="it will fail on purpose" file="tests/Feature/HttpTest.php::it will fail on purpose" class="Tests\Feature\HttpTest" time="0.000448">
        <failure type="PHPUnit\Framework\ExpectationFailedException">it will fail on purposeFailed asserting that 1 is identical to 2.
at tests/Feature/HttpTest.php:3</failure>
      </testcase>
    </testsuite>
  </testsuite>
</testsuites>"#;

    #[test]
    fn the_account_gives_one_outcome_per_test() {
        let outcomes = parse_junit(ACCOUNT);
        let names: Vec<&str> = outcomes.iter().map(|o| o.name.as_str()).collect();
        assert_eq!(
            names,
            [
                "Old school",
                "it sums 2 + 2",
                "la casse UTF-8 : éléphant",
                "`inside a describe` → it nests fine",
                "it runs on datasets",
                "it is skipped",
                "it answers",
                "it will fail on purpose",
            ]
        );
        // The two dataset cases fold into one green row.
        let datasets = &outcomes[4];
        assert_eq!(datasets.cases, 2);
        assert_eq!(datasets.status, Status::Passed);
        assert_eq!(outcomes[5].status, Status::Skipped);
        // The failure keeps its message and the line its text points at.
        let failed = &outcomes[7];
        assert_eq!(failed.status, Status::Failed);
        assert_eq!(failed.file, "tests/Feature/HttpTest.php");
        assert_eq!(failed.line, Some(3));
        assert!(failed.message.contains("1 is identical to 2"));
        // `Legacy::Old school` is no path.
        assert_eq!(outcomes[0].file, "");
    }

    /// The listing's mangled name and the account's real description name the
    /// same test — that pairing is what puts a dot on a row.
    #[test]
    fn the_listing_and_the_account_name_the_same_tests() {
        let tests = pest_parse(LISTING);
        let outcomes = parse_junit(ACCOUNT);
        for test in &tests {
            let paired = outcomes
                .iter()
                .filter(|o| o.class == test.class && same_test(&test.method, &o.name))
                .count();
            // `it answers` and `it will fail on purpose` live in the account
            // only; every listed test finds exactly one outcome.
            assert_eq!(paired, 1, "{}::{} pairs once", test.class, test.method);
        }
        // And not by accident of looseness: a different description does not.
        assert!(!same_test(
            "__pest_evaluable_it_sums_2___2",
            "it sums 2 + 3"
        ));
        assert!(same_test("testOldSchool", "Old school"));
        assert!(!same_test("testOldSchool", "New school"));
    }

    /// A folder of the tree runs as a class-prefix regex; the folder that
    /// *is* the class runs as the class.
    #[test]
    fn a_folder_runs_everything_under_it() {
        let class = "Tests\\TenantFeature\\Forms\\BillTest";
        assert_eq!(scope_filter(class, 0), "^Tests\\\\TenantFeature\\\\");
        assert_eq!(
            scope_filter(class, 1),
            "^Tests\\\\TenantFeature\\\\Forms\\\\"
        );
        assert_eq!(
            scope_filter(class, 2),
            "^Tests\\\\TenantFeature\\\\Forms\\\\BillTest::"
        );
        // A class with no `Tests\` prefix is its own folder from depth zero.
        assert_eq!(scope_filter("LegacyTest", 0), "^LegacyTest::");
    }

    /// While a run goes, the tree shows as loading what it covers: the one
    /// test, the class, a folder, or everything — never another runner's rows.
    #[test]
    fn a_target_covers_what_it_runs() {
        let tests = pest_parse(LISTING);
        let (math, sibling, feature) = (&tests[1], &tests[2], &tests[5]);
        assert!(covers(&Target::everything(Runner::Pest), math));
        assert!(!covers(&Target::everything(Runner::Vitest), math));
        let one = test_target(math);
        assert!(covers(&one, math));
        assert!(!covers(&one, sibling));
        let class = Target {
            filter: Some(class_filter(&math.class)),
            ..Target::everything(Runner::Pest)
        };
        assert!(covers(&class, math));
        assert!(covers(&class, sibling));
        assert!(!covers(&class, feature));
        let scope = scope_target(math, 0);
        assert!(covers(&scope, math));
        assert!(!covers(&scope, feature));

        let js = vitest_rows(
            r#"[
              {"name": "answers", "file": "/w/src/http.test.js"},
              {"name": "sums", "file": "/w/tests/unit/math.test.js"}
            ]"#,
            Path::new("/w"),
        );
        assert!(covers(&test_target(&js[1]), &js[1]));
        assert!(!covers(&test_target(&js[1]), &js[0]));
        assert!(covers(&scope_target(&js[1], 1), &js[1]));
        assert!(!covers(&scope_target(&js[1], 1), &js[0]));
    }

    #[test]
    fn the_group_header_drops_the_common_prefix() {
        assert_eq!(short_class("Tests\\Unit\\MathTest"), "Unit\\MathTest");
        assert_eq!(short_class("LegacyTest"), "LegacyTest");
    }

    /// `vitest list --json`, as Vitest 4 prints it: full titles with the
    /// describe chain, dataset cases unrolled, absolute files.
    #[test]
    fn a_vitest_listing_gives_one_row_per_test() {
        let json = r#"[
          {"name": "answers", "file": "/w/src/http.test.js"},
          {"name": "inside a describe > deeper > nests twice", "file": "/w/tests/unit/math.test.js"},
          {"name": "sums 2 + 2", "file": "/w/tests/unit/math.test.js"}
        ]"#;
        let tests = vitest_rows(json, Path::new("/w"));
        assert_eq!(tests.len(), 3);
        assert_eq!(tests[0].class, "src/http.test.js");
        assert_eq!(tests[0].name, "answers");
        // The `-t` full name joins describes with a space, and the title's
        // own regex characters are escaped — both verified against Vitest.
        assert_eq!(tests[1].pattern, "inside a describe deeper nests twice");
        assert_eq!(tests[2].pattern, "sums 2 \\+ 2");
        assert_eq!(
            test_target(&tests[2]),
            Target {
                runner: Runner::Vitest,
                filter: Some("^sums 2 \\+ 2$".into()),
                path: Some("tests/unit/math.test.js".into()),
                exact: true,
                headed: false,
                parallel: false,
            }
        );
    }

    /// `jest --listTests`: files only, absolute, in scheduling order — the
    /// rows are files, sorted back into the file system's order.
    #[test]
    fn a_jest_listing_gives_one_row_per_file() {
        let out = "/w/tests/unit/math.test.js\n/w/src/http.test.js\n";
        let tests = jest_rows(out, Path::new("/w"));
        assert_eq!(tests.len(), 2);
        assert_eq!(tests[0].class, "src");
        assert_eq!(tests[0].name, "http.test.js");
        assert_eq!(tests[0].method, "src/http.test.js");
        assert_eq!(
            test_target(&tests[0]),
            Target {
                runner: Runner::Jest,
                filter: None,
                path: Some("src/http.test.js".into()),
                exact: true,
                headed: false,
                parallel: false,
            }
        );
    }

    /// Jest's `--json` account: real titles, `pending` reading as skipped,
    /// the failure's line read from its own stack.
    #[test]
    fn a_jest_account_gives_one_outcome_per_test() {
        let json = r#"{"testResults": [{
            "name": "/w/src/http.test.js",
            "assertionResults": [
                {"title": "answers", "fullName": "answers", "status": "passed", "failureMessages": []},
                {"title": "will fail", "fullName": "inside will fail", "status": "failed",
                 "failureMessages": ["expect(received).toBe(expected)\n    at Object.<anonymous> (/w/src/http.test.js:2:53)"]},
                {"title": "later", "fullName": "later", "status": "pending", "failureMessages": []}
            ]
        }]}"#;
        let outcomes = parse_jest(json, Path::new("/w"));
        assert_eq!(outcomes.len(), 3);
        assert_eq!(outcomes[0].class, "src/http.test.js");
        assert_eq!(outcomes[1].status, Status::Failed);
        assert_eq!(outcomes[1].file, "src/http.test.js");
        assert_eq!(outcomes[1].line, Some(2));
        assert_eq!(outcomes[2].status, Status::Skipped);
    }

    /// A Vitest JUnit case: the class is `classname`, the file is the class,
    /// and the failure's line is on the `❯` arrow.
    #[test]
    fn a_vitest_account_reads_like_a_junit() {
        let xml = r#"<testsuites><testsuite name="src/http.test.js">
            <testcase classname="src/http.test.js" name="will fail on purpose" time="0.002">
                <failure message="expected 1 to be 2" type="AssertionError">AssertionError: expected 1 to be 2
 ❯ src/http.test.js:3:46</failure>
            </testcase>
        </testsuite></testsuites>"#;
        let outcomes = parse_junit(xml);
        assert_eq!(outcomes.len(), 1);
        assert_eq!(outcomes[0].class, "src/http.test.js");
        assert_eq!(outcomes[0].file, "src/http.test.js");
        assert_eq!(outcomes[0].line, Some(3));
    }

    /// The tree reads both worlds: namespaces split on `\`, paths on `/`,
    /// and a prefix is written as the group writes it.
    #[test]
    fn the_tree_reads_both_separators() {
        assert_eq!(segments("Tests\\Unit\\MathTest"), ["Unit", "MathTest"]);
        assert_eq!(
            segments("tests/unit/math.test.js"),
            ["tests", "unit", "math.test.js"]
        );
        assert_eq!(segments(""), Vec::<&str>::new());
        assert_eq!(group_prefix("tests/unit/math.test.js", 0), "tests");
        assert_eq!(group_prefix("tests/unit/math.test.js", 1), "tests/unit");
        assert_eq!(group_prefix("Tests\\Unit\\MathTest", 0), "Unit");
        let test = &vitest_rows(
            r#"[{"name": "x", "file": "/w/tests/unit/math.test.js"}]"#,
            Path::new("/w"),
        )[0];
        // A folder scopes by path; the deepest folder is the file, exact.
        assert_eq!(
            scope_target(test, 1),
            Target {
                runner: Runner::Vitest,
                filter: None,
                path: Some("tests/unit".into()),
                exact: false,
                headed: false,
                parallel: false,
            }
        );
        assert!(scope_target(test, 2).exact);
    }

    /// The terminal line carries the same narrowing, quoted for a shell.
    #[test]
    fn the_terminal_line_says_the_same_run() {
        let pest = Target {
            runner: Runner::Pest,
            filter: Some("^LegacyTest::testOldSchool$".into()),
            path: None,
            exact: false,
            headed: false,
            parallel: false,
        };
        // No quoting needed: `^`, `:` and a trailing `$` are all literal to a
        // POSIX shell, and `join_command` only quotes what would not be.
        assert_eq!(
            command_line(false, &pest),
            "vendor/bin/pest --filter ^LegacyTest::testOldSchool$"
        );
        // Headed comes first, before the narrowing — the browser plugin pops
        // it from the arguments before PHPUnit reads them.
        let headed = Target {
            headed: true,
            ..pest.clone()
        };
        assert_eq!(
            command_line(false, &headed),
            "vendor/bin/pest --headed --filter ^LegacyTest::testOldSchool$"
        );
        let parallel = Target {
            parallel: true,
            ..pest.clone()
        };
        assert_eq!(
            command_line(false, &parallel),
            "vendor/bin/pest --parallel --filter ^LegacyTest::testOldSchool$"
        );
        // A Sail checkout runs Pest through the containers — and only Pest:
        // the JS runners live on the host either way.
        assert_eq!(
            command_line(true, &pest),
            "vendor/bin/sail pest --filter ^LegacyTest::testOldSchool$"
        );
        let vitest = Target {
            runner: Runner::Vitest,
            filter: Some("^sums 2 \\+ 2$".into()),
            path: Some("tests/unit/math.test.js".into()),
            exact: true,
            headed: false,
            parallel: false,
        };
        assert_eq!(
            command_line(false, &vitest),
            "node_modules/.bin/vitest run tests/unit/math.test.js -t \"^sums 2 \\\\+ 2$\""
        );
        let jest = Target {
            runner: Runner::Jest,
            filter: None,
            path: Some("src/http.test.js".into()),
            exact: true,
            headed: false,
            parallel: false,
        };
        // Sail leaves a JS runner untouched: node lives on the host.
        assert_eq!(
            command_line(true, &jest),
            "node_modules/.bin/jest --runTestsByPath src/http.test.js"
        );
    }

    #[test]
    fn the_colours_come_out_of_a_narrated_line() {
        assert_eq!(
            strip_ansi(
                "\u{1b}[7m\u{1b}[1m\u{1b}[31m FAIL \u{1b}[39m\u{1b}[22m\u{1b}[27m src/http.test.js"
            ),
            " FAIL  src/http.test.js"
        );
        assert_eq!(strip_ansi("plain"), "plain");
    }
}
