//! What a worktree's Pest suite adds to Claudhub: its tests, and a panel that
//! runs one.
//!
//! Same bargain as the `justfile`, one domain over: a PHP project already
//! declares its tests, and Claudhub lists them **without parsing PHP** — the
//! list comes from `vendor/bin/pest --list-tests`, Pest's own answer, the only
//! reading that stays right when a suite uses `describe`, datasets, `uses()`
//! or plain PHPUnit classes mixed in. The price is that listing boots PHP —
//! about a second on a real Laravel suite of two thousand tests — which is why
//! it runs on the background queue and never on a frame.
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

/// One test, as the panel lists and runs it. Dataset entries are collapsed:
/// the row is the test, and running it runs every case.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Test {
    /// The printable test case name, `Tests\Unit\MathTest` — the `P\` prefix
    /// Pest puts on generated classes is stripped, because the filter is
    /// matched against the printable name, which does not carry it.
    pub class: String,
    /// The method name as listed, mangling included: the key that pairs this
    /// test with a run's outcome (`same_test`).
    pub method: String,
    /// What the row shows. For a Pest test, the description read back from the
    /// mangled name — lost bytes become single spaces, so `it sums 2 + 2`
    /// shows as "it sums 2 2"; honest, and a run's account then supplies the
    /// real one. For a PHPUnit method, the method name itself.
    pub name: String,
    /// The regex core matching this test's description in `--filter`, without
    /// anchors: what [`test_filter`] wraps.
    pub pattern: String,
    /// How many dataset entries were collapsed into this row. Zero for a test
    /// without datasets.
    pub datasets: u32,
}

/// What asking a worktree for its tests answered.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum Report {
    /// No `vendor/bin/pest`: not a Pest project, or `composer install` has not
    /// run. The panel disappears, which is the truth — there is nothing to
    /// run either.
    Missing,
    /// Pest is there and refused to list — a parse error in a test file, PHP
    /// missing, a bootstrap that failed. The message is Pest's own, shown in
    /// the panel with a way to retry.
    Failed(String),
    Tests(Vec<Test>),
}

/// What a worktree's suite declares.
///
/// Never an error: a missing binary and a listing that failed are both answers
/// the panel shows, not conditions to unwrap.
pub fn report(worktree: &Path) -> Report {
    let bin = worktree.join("vendor/bin/pest");
    if !bin.is_file() {
        return Report::Missing;
    }
    match list(worktree, &bin) {
        Ok(output) => Report::Tests(parse(&output)),
        Err(e) => {
            log::warn!("pest --list-tests in {}: {e:#}", worktree.display());
            Report::Failed(format!("{e:#}"))
        }
    }
}

/// Asks Pest what the suite declares.
fn list(worktree: &Path, bin: &Path) -> Result<String> {
    let mut cmd = Command::new(bin);
    cmd.arg("--list-tests")
        // Without it the `INFO` badge arrives wrapped in escape codes; the
        // test lines themselves happen to be clean, but parsing should not
        // depend on which lines Pest decides to decorate.
        .arg("--colors=never")
        .current_dir(worktree)
        // Closed, like git's and just's: a bootstrap deciding to read from
        // its input would hold the worker for good.
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        // Error messages we quote are read in English, for the same reason
        // the git layer reads them there.
        .env("LC_ALL", "C");
    crate::wsl::no_console(&mut cmd);
    let out = crate::git::wait_with_timeout(cmd, TIMEOUT, || "pest --list-tests".to_string())?;
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
fn parse(output: &str) -> Vec<Test> {
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

/// Runs the suite — all of it, or what `filter` names — saying each line as
/// it comes, and reads the account at the end.
///
/// A failing test is a **successful run**: Pest exits non-zero the moment one
/// test fails, and that is the result, not an error. The error case is the
/// run that produced no account at all — PHP missing, a suite that will not
/// boot — and then the streams speak, as for the listing.
///
/// The lines go out through `progress` while the process runs, the shape
/// `wt`'s hooks already have: a suite is measured in minutes, and a panel
/// saying nothing for minutes reads as hung.
pub fn run(
    worktree: &Path,
    filter: Option<&str>,
    progress: &(dyn Fn(String) + Sync),
) -> std::result::Result<Run, String> {
    let bin = worktree.join("vendor/bin/pest");
    if !bin.is_file() {
        return Err("vendor/bin/pest is missing".to_string());
    }
    let junit = std::env::temp_dir().join(format!(
        "claudhub-pest-{}-{}.xml",
        std::process::id(),
        RUN_SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
    ));
    let result = follow(worktree, &bin, filter, &junit, progress);
    let account = std::fs::read_to_string(&junit).unwrap_or_default();
    let _ = std::fs::remove_file(&junit);
    let (status, kept, stderr, duration_ms) = result?;
    let outcomes = parse_junit(&account);
    if outcomes.is_empty() {
        // No account: the suite never ran. `--filter` finding nothing is the
        // one benign shape — Pest then reports nothing and says "No tests
        // found" on its stdout, which the message passes on.
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

/// The subprocess itself: spawned, followed line by line, killed at the
/// ceiling. Returns the status, the last lines (for the message when there is
/// no account), stderr, and the wall-clock time.
///
/// The lines travel through a channel rather than being read in place — the
/// reason `git`'s streaming does the same: a read blocked on a command that
/// says nothing would have no ceiling, and the ceiling is the point.
#[allow(clippy::type_complexity)]
fn follow(
    worktree: &Path,
    bin: &Path,
    filter: Option<&str>,
    junit: &Path,
    progress: &(dyn Fn(String) + Sync),
) -> std::result::Result<(String, Vec<String>, String, u64), String> {
    use std::io::{BufRead, Read};

    let started = std::time::Instant::now();
    let mut cmd = Command::new(bin);
    cmd.arg("--colors=never")
        .arg("--log-junit")
        .arg(junit)
        .current_dir(worktree)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .env("LC_ALL", "C");
    if let Some(filter) = filter {
        cmd.arg("--filter").arg(filter);
    }
    crate::wsl::no_console(&mut cmd);
    let mut child = cmd.spawn().map_err(|e| format!("vendor/bin/pest: {e}"))?;

    let mut stderr = child.stderr.take().expect("stderr requested as piped");
    let err_reader = std::thread::spawn(move || {
        let mut buffer = Vec::new();
        let _ = stderr.read_to_end(&mut buffer);
        buffer
    });
    let stdout = child.stdout.take().expect("stdout requested as piped");
    let (lines, incoming) = std::sync::mpsc::sync_channel::<String>(256);
    let reader = std::thread::spawn(move || {
        for line in std::io::BufReader::new(stdout).split(b'\n') {
            let Ok(line) = line else { break };
            let mut line = String::from_utf8_lossy(&line).into_owned();
            if line.ends_with('\r') {
                line.pop();
            }
            if lines.send(line).is_err() {
                break;
            }
        }
    });

    let deadline = started + RUN_TIMEOUT;
    let mut kept: std::collections::VecDeque<String> = std::collections::VecDeque::new();
    let overtime = loop {
        let now = std::time::Instant::now();
        if now >= deadline {
            break true;
        }
        match incoming.recv_timeout(deadline - now) {
            Ok(line) => {
                if kept.len() == COMPLAINT_KEPT {
                    kept.pop_front();
                }
                kept.push_back(line.clone());
                progress(line);
            }
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => break false,
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => break true,
        }
    };
    if overtime {
        let _ = child.kill();
        let _ = child.wait();
        let _ = reader.join();
        let _ = err_reader.join();
        return Err(format!(
            "pest did not answer within {RUN_TIMEOUT:?} and was interrupted"
        ));
    }
    let _ = reader.join();
    // stdout is closed: the process is exiting. The bounded wait is for the
    // one that closed its output and wedged anyway.
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) => {
                if std::time::Instant::now() >= deadline {
                    let _ = child.kill();
                    let _ = child.wait();
                    let _ = err_reader.join();
                    return Err(format!(
                        "pest did not answer within {RUN_TIMEOUT:?} and was interrupted"
                    ));
                }
                std::thread::sleep(Duration::from_millis(10));
            }
            Err(e) => return Err(format!("waiting on pest: {e}")),
        }
    };
    let stderr = err_reader.join().unwrap_or_default();
    Ok((
        status.to_string(),
        kept.into(),
        String::from_utf8_lossy(&stderr).into_owned(),
        started.elapsed().as_millis() as u64,
    ))
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
        let Some(class) = case.attribute("class") else {
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
        // path and is dropped.
        let file = case
            .attribute("file")
            .map(|file| file.split("::").next().unwrap_or(file))
            .filter(|file| file.ends_with(".php"))
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
/// attribute for it, but Pest writes `at tests/Feature/HttpTest.php:3` in
/// the text.
fn failure_line(message: &str) -> Option<u32> {
    message.lines().find_map(|line| {
        let (_, number) = line.trim().strip_prefix("at ")?.rsplit_once(':')?;
        number.parse().ok()
    })
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

/// The class as the panel's group header shows it: the `Tests\` every suite
/// starts with says nothing, `Unit\MathTest` is what one scans for.
pub fn short_class(class: &str) -> &str {
    class.strip_prefix("Tests\\").unwrap_or(class)
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
        let tests = parse(LISTING);
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
        let tests = parse(LISTING);
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
        let tests = parse(
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
        let tests = parse(
            " - LegacyTest::testCases with data set #0\n - LegacyTest::testCases with data set #1.\n",
        );
        assert_eq!(tests.len(), 1);
        assert_eq!(tests[0].name, "testCases");
        assert_eq!(tests[0].datasets, 2);
    }

    #[test]
    fn what_is_not_a_test_line_is_ignored() {
        assert!(parse("   INFO  No tests found.\n").is_empty());
        assert!(parse("").is_empty());
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
        let tests = parse(LISTING);
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

    #[test]
    fn the_group_header_drops_the_common_prefix() {
        assert_eq!(short_class("Tests\\Unit\\MathTest"), "Unit\\MathTest");
        assert_eq!(short_class("LegacyTest"), "LegacyTest");
    }
}
