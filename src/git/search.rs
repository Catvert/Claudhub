//! Searching the whole project, through `git grep`.
//!
//! This is PhpStorm's `Ctrl+Shift+F` and Doom's `SPC s p`: a word, and every
//! place in the checkout that carries it. Claudhub had a search **per panel**
//! (`ui::find`) — which filters a list it already holds — and nothing at all
//! for the question one actually asks while reviewing: *where else is this
//! called?*
//!
//! **`git grep` and not a walk of our own**, for the reason the whole `git/`
//! layer exists: it already knows what belongs to the project. The index gives
//! it the tracked files without a single `readdir`, `--untracked` adds what is
//! new and not ignored, and `.gitignore` keeps `vendor/` and `node_modules/`
//! out — which on a Laravel project is the difference between seven hundred
//! files and forty thousand. It is also threaded, and it skips binaries by
//! itself (`-I`).
//!
//! Three guards, and each is there because its absence is unusable rather than
//! merely slow:
//!
//! - **Every line is capped** (`MAX_LINE`). One minified asset makes a two
//!   megabyte "line", which travels over the wire, is shaped by gpui and pushes
//!   the list off the screen. The cut is on a **character** boundary — cutting
//!   bytes leaves a slice that is not UTF-8.
//! - **The hits are capped** (`MAX_HITS`), and the cap is **said**: a search
//!   silently showing its first two thousand hits reads as a search that found
//!   two thousand.
//! - **Case is derived from the query**, exactly as in `ui::find`: an
//!   all-lowercase query ignores case, one carrying a capital respects it. It
//!   is every editor's convention, and it saves a button for a setting that
//!   changes with every search.

use std::path::{Path, PathBuf};

use anyhow::Result;

use super::git_tolerant;

/// Past this, a line is an asset and not code. Counted in **bytes**, cut on a
/// character boundary.
pub const MAX_LINE: usize = 400;

/// How many hits come back at most, all files together.
pub const MAX_HITS: usize = 2_000;

/// How many hits a single file contributes.
///
/// A generated file matching on every one of its lines would otherwise fill the
/// global budget by itself, and the point of a project-wide search is to see
/// *which files* are concerned.
pub const MAX_PER_FILE: usize = 100;

/// What was asked for.
///
/// No case flag: it is derived from the text — see the module's note.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Query {
    pub text: String,
    /// The text is a POSIX extended regular expression rather than a literal.
    ///
    /// `-E` and not `-P`: PCRE is a compile-time option of git, absent from
    /// several distributions' packages, and a search that fails on the user's
    /// machine and not on ours is the worst kind of feature.
    pub regex: bool,
    /// Whole words only (`-w`).
    pub whole_word: bool,
    /// Which files to look in, as git pathspecs separated by commas —
    /// `*.rs, src/ui/*`. Empty: all of them.
    pub include: String,
}

impl Query {
    /// Is the query worth sending? An empty one matches every line of the
    /// project, which is a way of saying nothing at all.
    pub fn is_empty(&self) -> bool {
        self.text.trim().is_empty()
    }

    /// Whether case matters, by the smart-case convention.
    pub fn case_sensitive(&self) -> bool {
        self.text.chars().any(char::is_uppercase)
    }

    /// The pathspecs, cleaned up. Public because the view shows what it will
    /// send: a glob that is dropped in silence reads as a search ignoring it.
    pub fn pathspecs(&self) -> Vec<String> {
        self.include
            .split(',')
            .map(str::trim)
            .filter(|spec| !spec.is_empty())
            .map(str::to_string)
            .collect()
    }
}

/// One matching line.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Hit {
    /// One-based, as git prints it and as an editor counts.
    pub line: u32,
    pub text: String,
}

/// One file, and what it carries.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct FileHits {
    pub path: PathBuf,
    pub hits: Vec<Hit>,
    /// The file had more than `MAX_PER_FILE`.
    pub capped: bool,
}

/// What a search answered.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Results {
    pub files: Vec<FileHits>,
    /// Hits kept, all files together — `files` is what is left after the caps.
    pub total: usize,
    /// A cap cut the answer short. Said out loud rather than shown as a full
    /// list that happens to stop.
    pub truncated: bool,
}

/// The arguments handed to `git grep`, in order.
///
/// Split out from `run` because it is the whole decision, and the only part
/// worth a test: an option in the wrong place makes git read the pattern as a
/// pathspec, and a search that quietly looks for the wrong thing is the failure
/// nobody diagnoses.
///
/// `-e` before the pattern and `--` before the pathspecs are not decoration:
/// they are what makes a pattern starting with a dash a pattern, and a pathspec
/// named like a file a pathspec.
pub fn args(query: &Query) -> Vec<String> {
    let mut args: Vec<String> = vec![
        "grep".into(),
        // Skip binaries. Without it a `.png` matching by accident comes back as
        // a line of control characters.
        "-I".into(),
        "--line-number".into(),
        // `path NUL line NUL text`: a path may contain a space, a colon or a
        // newline, and every `-z` format in this layer exists for that reason.
        "-z".into(),
        "--no-color".into(),
        // What is new and not ignored counts: an agent's worktree is full of
        // files git has not been told about yet.
        "--untracked".into(),
        format!("--max-count={MAX_PER_FILE}"),
    ];
    if !query.case_sensitive() {
        args.push("-i".into());
    }
    if query.whole_word {
        args.push("-w".into());
    }
    args.push(if query.regex {
        "-E".into()
    } else {
        "-F".into()
    });
    args.push("-e".into());
    args.push(query.text.clone());
    let specs = query.pathspecs();
    if !specs.is_empty() {
        args.push("--".into());
        args.extend(specs);
    }
    args
}

/// Runs the search in a checkout.
///
/// `git_tolerant` with a ceiling of 1: `git grep` exits with **1 when it finds
/// nothing**, which is an answer and not a failure — the same reason
/// `diff --no-index` needed it. Past that it is a real error, a bad regular
/// expression being the common one, and its message is what the panel shows.
pub fn run(dir: &Path, query: &Query) -> Result<Results> {
    if query.is_empty() {
        return Ok(Results::default());
    }
    let out = git_tolerant(dir, &args(query), 1)?;
    Ok(parse(&out))
}

/// Turns `path NUL line NUL text` records into files and their hits.
///
/// **The record separator is the newline, the field separator is the NUL.**
/// That is what `-z` buys and it is only half a guarantee: a path holding a
/// space or a colon survives — which is the case that matters, since without
/// `-z` git prints `path:line:text` and a colon in a path splits the record in
/// the wrong place — but a path holding a **newline** does not, git grep having
/// no format that escapes it. It is the one file name this layer cannot read,
/// and it is worth knowing rather than guarding against.
///
/// git groups its output by file and walks the index in order, so a file's
/// records are consecutive: grouping is a comparison with the previous path,
/// never a map.
///
/// A record that does not parse is **skipped**, not fatal: the alternative is
/// losing a whole search because one line came back malformed.
pub fn parse(out: &str) -> Results {
    let mut files: Vec<FileHits> = Vec::new();
    let mut total = 0usize;
    let mut truncated = false;
    for record in out.lines() {
        if record.is_empty() {
            continue;
        }
        let mut fields = record.splitn(3, '\0');
        let (Some(path), Some(number), Some(text)) = (fields.next(), fields.next(), fields.next())
        else {
            continue;
        };
        let Ok(line) = number.parse::<u32>() else {
            continue;
        };
        if total >= MAX_HITS {
            truncated = true;
            break;
        }
        total += 1;
        let hit = Hit {
            line,
            text: clip(text),
        };
        match files.last_mut() {
            Some(last) if last.path.as_os_str() == path => last.hits.push(hit),
            _ => files.push(FileHits {
                path: PathBuf::from(path),
                hits: vec![hit],
                capped: false,
            }),
        }
    }
    for file in &mut files {
        if file.hits.len() >= MAX_PER_FILE {
            file.capped = true;
            truncated = true;
        }
    }
    Results {
        files,
        total,
        truncated,
    }
}

/// Cuts a line down to something a list can show, on a character boundary.
fn clip(line: &str) -> String {
    if line.len() <= MAX_LINE {
        return line.to_string();
    }
    let end = (0..=MAX_LINE)
        .rev()
        .find(|index| line.is_char_boundary(*index))
        .unwrap_or(0);
    let mut out = line[..end].to_string();
    out.push('…');
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn query(text: &str) -> Query {
        Query {
            text: text.into(),
            ..Default::default()
        }
    }

    #[test]
    fn a_lowercase_query_ignores_case_and_an_uppercase_one_does_not() {
        assert!(args(&query("todo")).iter().any(|a| a == "-i"));
        assert!(!args(&query("Todo")).iter().any(|a| a == "-i"));
    }

    #[test]
    fn a_literal_search_is_fixed_and_a_regex_one_extended() {
        assert!(args(&query("a.b")).iter().any(|a| a == "-F"));
        let regex = Query {
            regex: true,
            ..query("a.b")
        };
        assert!(args(&regex).iter().any(|a| a == "-E"));
        assert!(!args(&regex).iter().any(|a| a == "-F"));
    }

    /// The pattern must follow `-e`, and the pathspecs must follow `--`:
    /// otherwise a pattern starting with a dash is read as an option, and a
    /// pathspec as a pattern.
    #[test]
    fn the_pattern_and_the_pathspecs_are_each_behind_their_marker() {
        let mut q = query("-v");
        q.include = "*.rs, src/ui/*".into();
        let args = args(&q);
        let e = args.iter().position(|a| a == "-e").expect("-e");
        assert_eq!(args[e + 1], "-v");
        let dashes = args.iter().position(|a| a == "--").expect("--");
        assert_eq!(&args[dashes + 1..], ["*.rs", "src/ui/*"]);
    }

    #[test]
    fn an_empty_glob_contributes_no_pathspec() {
        let mut q = query("x");
        q.include = " , ,".into();
        assert!(!args(&q).iter().any(|a| a == "--"));
    }

    #[test]
    fn records_group_by_file_in_the_order_git_gives_them() {
        let out = "a.rs\u{0}1\u{0}fn one\na.rs\u{0}9\u{0}fn two\nb.rs\u{0}3\u{0}fn three\n";
        let results = parse(out);
        assert_eq!(results.total, 3);
        assert_eq!(results.files.len(), 2);
        assert_eq!(results.files[0].path, PathBuf::from("a.rs"));
        assert_eq!(results.files[0].hits.len(), 2);
        assert_eq!(results.files[0].hits[1].line, 9);
        assert_eq!(results.files[1].hits[0].text, "fn three");
    }

    /// A path may hold a space or a colon; that is why the format is `-z`.
    #[test]
    fn a_path_with_a_space_survives() {
        let out = "some dir/a b.rs\u{0}2\u{0}x\n";
        let results = parse(out);
        assert_eq!(results.files[0].path, PathBuf::from("some dir/a b.rs"));
    }

    #[test]
    fn a_line_is_cut_on_a_character_boundary() {
        let long = "é".repeat(MAX_LINE);
        let cut = clip(&long);
        assert!(cut.len() <= MAX_LINE + '…'.len_utf8());
        assert!(cut.ends_with('…'));
        // The proof that the cut is not in the middle of a character: it is
        // valid UTF-8 made only of the characters we put in.
        assert!(cut.trim_end_matches('…').chars().all(|c| c == 'é'));
    }

    #[test]
    fn a_short_line_is_left_alone() {
        assert_eq!(clip("fn main() {}"), "fn main() {}");
    }

    #[test]
    fn the_global_cap_stops_the_walk_and_says_so() {
        let mut out = String::new();
        for line in 1..=(MAX_HITS + 10) {
            out.push_str(&format!("a{line}.rs\u{0}{line}\u{0}x\n"));
        }
        let results = parse(&out);
        assert_eq!(results.total, MAX_HITS);
        assert!(results.truncated);
    }

    /// The only test here that runs git, and it is the one that proves the
    /// chain: the arguments, the format, the parsing. The unit tests above
    /// check what we build; this checks that git answers what we think it
    /// does — the precedent is `watch::tests::a_real_write_reaches_the_receiver`.
    ///
    /// It **skips** when the source tree is not a checkout — a tarball build —
    /// rather than failing: a red build that says nothing is worse than no test.
    #[test]
    fn a_real_grep_finds_what_is_in_this_repository() {
        let dir = Path::new(env!("CARGO_MANIFEST_DIR"));
        if !crate::git::repo::is_repo(dir) {
            return;
        }
        // A string this very file carries, so the search cannot come back empty
        // for a reason having nothing to do with the code under test.
        let results = run(
            dir,
            &Query {
                text: "a_real_grep_finds_what_is_in_this_repository".into(),
                ..Default::default()
            },
        )
        .expect("git grep");
        assert!(results.total > 0, "the needle is in this file");
        assert!(results
            .files
            .iter()
            .any(|file| file.path.ends_with("git/search.rs")));
        // And a needle nothing carries is an answer, not a failure: git grep
        // exits with 1 there, which is why `run` tolerates that code. The
        // needle is **assembled** rather than written out: written out, this
        // file would carry it and the search would find it.
        let absent = format!("zzz{}needle{}zzz", "-no-such-", "-");
        let empty = run(
            dir,
            &Query {
                text: absent,
                ..Default::default()
            },
        )
        .expect("git grep with no match is not an error");
        assert_eq!(empty.total, 0);
    }

    #[test]
    fn an_empty_output_is_a_search_that_found_nothing() {
        let results = parse("");
        assert!(results.files.is_empty());
        assert_eq!(results.total, 0);
        assert!(!results.truncated);
    }
}
