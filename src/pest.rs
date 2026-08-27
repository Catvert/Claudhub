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
    /// What the row shows. For a Pest test, the description read back from the
    /// mangled name — lost bytes become single spaces, so `it sums 2 + 2`
    /// shows as "it sums 2 2"; honest, and the terminal prints the real one.
    /// For a PHPUnit method, the method name itself.
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

    #[test]
    fn the_group_header_drops_the_common_prefix() {
        assert_eq!(short_class("Tests\\Unit\\MathTest"), "Unit\\MathTest");
        assert_eq!(short_class("LegacyTest"), "LegacyTest");
    }
}
