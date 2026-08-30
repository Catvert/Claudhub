//! Which of a project's paths a handful of letters named.
//!
//! PhpStorm's `Shift Shift`: a few letters, and the file they name. It is not
//! `ui::find`, which **filters** a list already on screen row by row: here the
//! list is forty thousand paths, nothing of it is shown until something is
//! typed, and what comes back is an **order** — the twenty paths those letters
//! most plausibly meant, best first.
//!
//! **The match is loose, and that is the whole gesture.** `uisrch` finds
//! `src/ui/search_view.rs`: one types the shape of a path, not a fragment of
//! it, because the fragment one remembers is rarely contiguous. Every other
//! search in this window is a substring — `ui::find`, `git grep` — and they are
//! right to be: they answer *where is this text*, which is a question a typo
//! must not widen. This one answers *which file did I mean*, where the typo is
//! the normal case.
//!
//! **Case is derived from the query**, as everywhere else here: an
//! all-lowercase query ignores case, one carrying a capital respects it.
//!
//! Pure, and tested: what a score rewards is the whole of the ranking, and a
//! ranking one cannot re-read is a ranking nobody can fix.

use std::ops::Range;
use std::path::{Path, PathBuf};

/// How many files back the history goes.
///
/// Two screens' worth, past which nobody scrolls looking for "the one from
/// before". It caps what is **written**, where `MAX_ROWS` caps what is shown.
pub const MAX_RECENT: usize = 50;

/// How many rows the list shows at most.
///
/// The list is read from the top and abandoned by the fifth row; past a couple
/// of hundred it is neither read nor scrolled, and every one of them is shaped
/// by gpui. It is a cap on the **display**, not on the search: the score has
/// already seen every path.
pub const MAX_ROWS: usize = 200;

/// A candidate that matched, and how well.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Hit {
    /// Where the candidate sits in the slice handed to `rank`.
    pub index: usize,
    pub score: i32,
    /// The matched characters, as **byte** ranges into the candidate — that is
    /// what gpui expects to style a fragment of text, and indexing by
    /// characters breaks at the first accent. Adjacent characters are merged
    /// into one range: a run of five is one styled fragment, not five.
    pub ranges: Vec<Range<usize>>,
}

/// The paths those letters most plausibly named, best first.
///
/// An empty query matches nothing rather than everything: the first two
/// hundred paths of a checkout are whatever sorts first — `.github/`, most
/// often — and a list nobody asked for reads as an answer.
pub fn rank(query: &str, candidates: &[&str]) -> Vec<Hit> {
    let query: Vec<char> = query.trim().chars().collect();
    if query.is_empty() {
        return Vec::new();
    }
    let sensitive = query.iter().copied().any(char::is_uppercase);
    let mut hits: Vec<Hit> = candidates
        .iter()
        .enumerate()
        .filter_map(|(index, hay)| {
            score(&query, hay, sensitive).map(|(score, ranges)| Hit {
                index,
                score,
                ranges,
            })
        })
        .collect();
    // The whole list is scored and only the shown part is sorted: a checkout
    // of a hundred thousand paths sorts in ten milliseconds, which is a
    // keystroke one feels, and the rows past the two hundredth are thrown
    // away immediately afterwards.
    if hits.len() > MAX_ROWS {
        hits.select_nth_unstable_by(MAX_ROWS, |a, b| better(a, b, candidates));
        hits.truncate(MAX_ROWS);
    }
    hits.sort_by(|a, b| better(a, b, candidates));
    hits
}

/// The order the list is read in: the higher score first, and on a tie the
/// shorter path.
///
/// The tie-break is not decoration. `Handler.php` and
/// `vendor/…/Handler.php` score alike on the letters they share — the second's
/// extra segments earn nothing — and the one meant is almost always the one
/// nearer the root. The final comparison on the text itself is what keeps the
/// order **stable**: two paths of the same length and the same score would
/// otherwise swap places from one keystroke to the next.
fn better(a: &Hit, b: &Hit, candidates: &[&str]) -> std::cmp::Ordering {
    let (left, right) = (candidates[a.index], candidates[b.index]);
    b.score
        .cmp(&a.score)
        .then_with(|| left.len().cmp(&right.len()))
        .then_with(|| left.cmp(right))
}

/// Does the candidate carry the query's characters in order, and how well.
///
/// **Two passes, and the second is what makes the answer readable.** A forward
/// walk finds the first place the query runs out — the earliest possible end —
/// and a backward walk from there gives the **tightest** match ending at it. A
/// forward walk alone would match the `s` of `src` and the `e` of `search` for
/// a query of `se`, scattering the highlight across a path whose file name
/// carries both side by side; the backward pass pulls every matched character
/// as far right as it can go, which is where a run of them is.
fn score(query: &[char], hay: &str, sensitive: bool) -> Option<(i32, Vec<Range<usize>>)> {
    // The earliest end, forward.
    let mut wanted = query.iter();
    let mut next = wanted.next()?;
    let mut end = None;
    for (offset, candidate) in hay.char_indices() {
        if !same(candidate, *next, sensitive) {
            continue;
        }
        match wanted.next() {
            Some(following) => next = following,
            None => {
                end = Some(offset + candidate.len_utf8());
                break;
            }
        }
    }
    let end = end?;
    // The tightest start, backward from it. The positions come out reversed.
    let mut wanted = query.iter().rev();
    let mut next = wanted.next()?;
    let mut hit: Vec<(usize, char)> = Vec::with_capacity(query.len());
    for (offset, candidate) in hay[..end].char_indices().rev() {
        if !same(candidate, *next, sensitive) {
            continue;
        }
        hit.push((offset, candidate));
        match wanted.next() {
            Some(preceding) => next = preceding,
            None => break,
        }
    }
    hit.reverse();
    Some((points(hay, &hit), merge(&hit)))
}

/// What a set of matched positions is worth.
///
/// Four rewards and two penalties, and each answers a way of naming a file. A
/// run of characters is worth more than the same characters scattered — one
/// types `search`, not `s…e…a`. A character starting a word is worth more than
/// one inside it: `uisv` is `ui/search_view` and nothing else. The file
/// **name** outweighs the folders leading to it, which is what one is looking
/// for. And the two penalties are the tie-breaks that make a short path near
/// the root beat the same name buried under `vendor/`.
fn points(hay: &str, hit: &[(usize, char)]) -> i32 {
    let name = name_at(hay);
    let mut score = 0;
    let mut previous: Option<(usize, char)> = None;
    for &(offset, character) in hit {
        score += 16;
        if let Some((before, letter)) = previous {
            if before + letter.len_utf8() == offset {
                score += 12;
            }
        }
        if boundary(hay, offset) {
            score += 14;
        }
        if offset >= name {
            score += 8;
        }
        previous = Some((offset, character));
    }
    // Where the match begins, and how much path there is around it: an early
    // hit in a short path is the one meant.
    score -= hit.first().map_or(0, |&(offset, _)| offset as i32) / 4;
    score -= hay.len() as i32 / 16;
    score
}

/// Does a word begin at this offset?
///
/// The separators of a path and of every naming convention it carries —
/// `src/ui/search_view.rs`, `SearchView.php`, `search-view.ts` — plus the
/// lowercase-to-uppercase step, which is the only boundary camel case has.
fn boundary(hay: &str, offset: usize) -> bool {
    if offset == 0 {
        return true;
    }
    let before = hay[..offset].chars().next_back();
    let here = hay[offset..].chars().next();
    match (before, here) {
        (Some('/' | '\\' | '_' | '-' | '.' | ' '), _) => true,
        (Some(before), Some(here)) => before.is_lowercase() && here.is_uppercase(),
        _ => false,
    }
}

/// The matched characters as ranges, consecutive ones merged.
fn merge(hit: &[(usize, char)]) -> Vec<Range<usize>> {
    let mut out: Vec<Range<usize>> = Vec::new();
    for &(offset, character) in hit {
        let end = offset + character.len_utf8();
        match out.last_mut() {
            Some(last) if last.end == offset => last.end = end,
            _ => out.push(offset..end),
        }
    }
    out
}

/// How long the second press of `Shift Shift` has to arrive.
///
/// PhpStorm's own window, near enough. Longer, and a Shift pressed twice while
/// typing two capitals a second apart opens a palette nobody asked for;
/// shorter, and the gesture has to be drummed rather than tapped.
pub const DOUBLE_TAP: std::time::Duration = std::time::Duration::from_millis(400);

/// Where the `Shift Shift` gesture stands.
///
/// **A bare modifier is not a key gpui can bind**, and that is why this exists
/// at all: `KeyBinding::new` wants a key, and Shift alone never becomes one —
/// on X11 as on Windows the platform turns it into a modifier change and
/// nothing else. So the double tap is read from the modifier changes
/// themselves, on the root, where they arrive whatever holds the keyboard.
///
/// **Anything else the hand does breaks the run** (`interrupt`), and that is
/// the whole of what keeps this from firing while one types. `AB` is Shift
/// down, `a`, Shift up, Shift down, `b`, Shift up — two presses and two
/// releases, which is the gesture exactly, told apart from it only by the
/// letters in between.
#[derive(Debug, Default)]
pub struct DoubleTap(Tap);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
enum Tap {
    #[default]
    Idle,
    Down,
    Released(std::time::Instant),
}

impl DoubleTap {
    /// A modifier change: `shift` is whether Shift is held, `alone` whether it
    /// is the only modifier held. True on the second press of the pair.
    pub fn modifiers(&mut self, shift: bool, alone: bool, now: std::time::Instant) -> bool {
        match (shift, alone, self.0) {
            // Shift with a companion — `Ctrl+Shift+F` is being typed, and its
            // Shift is not a tap.
            (true, false, _) => self.0 = Tap::Idle,
            (true, true, Tap::Released(at)) if now.duration_since(at) < DOUBLE_TAP => {
                self.0 = Tap::Idle;
                return true;
            }
            (true, true, _) => self.0 = Tap::Down,
            // A release only counts after a press this state saw: the Shift
            // going up at the end of a capital letter is not one.
            (false, _, Tap::Down) => self.0 = Tap::Released(now),
            (false, _, _) => self.0 = Tap::Idle,
        }
        false
    }

    /// A key, or a click: whatever was being tapped, it was not this.
    pub fn interrupt(&mut self) {
        self.0 = Tap::Idle;
    }
}

/// Where a path's file name begins — the byte after its last separator.
pub fn name_at(path: &str) -> usize {
    path.rfind('/').map(|slash| slash + 1).unwrap_or(0)
}

/// The matched ranges, cut where the folders end and the file name begins.
///
/// The row shows the two halves apart — the name in the foreground, the
/// folders leading to it in muted text — because that is the order one reads
/// them in, and a path is mostly folders. Which means the highlight has to be
/// cut in two as well, and rebased on the second half: a range is an offset
/// into the string it styles, and the name is its own string once painted.
///
/// A range **straddling** the cut is split rather than dropped: a query
/// carrying a slash — `ui/se` — matches across it, and a highlight that
/// vanished exactly where the query was most explicit would be the one place
/// it must not.
pub fn split(ranges: &[Range<usize>], at: usize) -> (Vec<Range<usize>>, Vec<Range<usize>>) {
    let mut folders = Vec::new();
    let mut name = Vec::new();
    for range in ranges {
        if range.start < at {
            folders.push(range.start..range.end.min(at));
        }
        if range.end > at {
            name.push(range.start.max(at) - at..range.end - at);
        }
    }
    (folders, name)
}

/// Two characters, read as the query asks them to be.
///
/// `ui::find`'s rule and its reason: lowercasing the whole text would change
/// its byte length, and the offsets returned would no longer point at anything
/// in the original.
fn same(a: char, b: char, sensitive: bool) -> bool {
    if sensitive {
        a == b
    } else {
        a == b || a.to_lowercase().eq(b.to_lowercase())
    }
}

/// Puts a file at the head of a history, and says whether anything moved.
///
/// The answer is what keeps the state file still: coming back to the tab one
/// is already on is the ordinary case — every switch goes through here — and
/// rewriting the same list would be a file changing with nothing having
/// changed.
pub fn promote(history: &mut Vec<PathBuf>, path: &Path) -> bool {
    if history.first().is_some_and(|first| first == path) {
        return false;
    }
    history.retain(|seen| seen != path);
    history.insert(0, path.to_path_buf());
    history.truncate(MAX_RECENT);
    true
}

/// The history, cut down to what the checkout still holds.
///
/// **Not tidying.** The history outlives the files in it: one deleted, renamed
/// or belonging to a branch since left is still written there, and a row that
/// opens nothing is worse than a row missing. `known` is the project's own
/// list, which is git's, so this also keeps a worktree's history to its own
/// worktree.
pub fn recent(history: &[PathBuf], known: &[PathBuf]) -> Vec<PathBuf> {
    let known: std::collections::HashSet<&PathBuf> = known.iter().collect();
    history
        .iter()
        .filter(|path| known.contains(*path))
        .take(MAX_ROWS)
        .cloned()
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    /// Ranges written as pairs.
    ///
    /// Clippy reads a one-element array of `Range` as an array somebody meant
    /// to be a range, and refuses it — which is fair everywhere but here,
    /// where a single highlighted run is the ordinary answer.
    fn ranges(pairs: &[(usize, usize)]) -> Vec<Range<usize>> {
        pairs.iter().map(|&(start, end)| start..end).collect()
    }

    /// The paths in `rank`'s order, for a query.
    fn ranked<'a>(query: &str, candidates: &[&'a str]) -> Vec<&'a str> {
        rank(query, candidates)
            .into_iter()
            .map(|hit| candidates[hit.index])
            .collect()
    }

    #[test]
    fn an_empty_query_names_nothing() {
        assert!(ranked("", &["src/ui/app.rs"]).is_empty());
        assert!(ranked("   ", &["src/ui/app.rs"]).is_empty());
    }

    #[test]
    fn the_letters_need_not_touch() {
        assert_eq!(
            ranked("uisrch", &["src/ui/search_view.rs"]),
            ["src/ui/search_view.rs"]
        );
    }

    #[test]
    fn a_letter_missing_from_the_path_matches_nothing() {
        assert!(ranked("zzz", &["src/ui/app.rs"]).is_empty());
    }

    #[test]
    fn the_order_of_the_letters_is_the_order_of_the_path() {
        assert!(ranked("rsapp", &["src/ui/app.rs"]).is_empty());
    }

    #[test]
    fn a_run_beats_the_same_letters_scattered() {
        assert_eq!(
            ranked("app", &["a/p/p/other.rs", "src/ui/app.rs"]),
            ["src/ui/app.rs", "a/p/p/other.rs"]
        );
    }

    #[test]
    fn the_file_name_beats_the_folders_leading_to_it() {
        assert_eq!(
            ranked("app", &["app/models/user.rs", "src/app.rs"]),
            ["src/app.rs", "app/models/user.rs"]
        );
    }

    #[test]
    fn the_shorter_path_wins_a_tie() {
        assert_eq!(
            ranked(
                "handler",
                &["vendor/pkg/src/Handler.php", "src/Handler.php"]
            ),
            ["src/Handler.php", "vendor/pkg/src/Handler.php"]
        );
    }

    /// Both paths carry the letters in their file name; the one where each
    /// starts a word is the one meant.
    #[test]
    fn word_starts_beat_letters_inside_words() {
        assert_eq!(
            ranked("sv", &["src/ui/serves.rs", "src/ui/search_view.rs"]),
            ["src/ui/search_view.rs", "src/ui/serves.rs"]
        );
    }

    /// Smart case: a lowercase query ignores case, a query with a capital does
    /// not.
    #[test]
    fn case_follows_the_query() {
        assert_eq!(ranked("handler", &["src/Handler.php"]), ["src/Handler.php"]);
        assert!(ranked("Handler", &["src/handler.php"]).is_empty());
        assert_eq!(ranked("Handler", &["src/Handler.php"]), ["src/Handler.php"]);
    }

    /// The backward pass: the highlight lands on the run, not on the first
    /// letters that happened to match.
    #[test]
    fn the_highlight_is_the_tightest_match() {
        let hits = rank("se", &["src/ui/search_view.rs"]);
        assert_eq!(hits[0].ranges, ranges(&[(7, 9)]));
    }

    #[test]
    fn adjacent_characters_make_one_range() {
        let hits = rank("app", &["src/app.rs"]);
        assert_eq!(hits[0].ranges, ranges(&[(4, 7)]));
    }

    /// The offsets are bytes, and an accent is two of them.
    #[test]
    fn the_ranges_are_byte_offsets() {
        let hits = rank("erf", &["src/été/perf.rs"]);
        let path = "src/été/perf.rs";
        let shown: Vec<&str> = hits[0]
            .ranges
            .iter()
            .map(|range| &path[range.clone()])
            .collect();
        assert_eq!(shown.concat(), "erf");
    }

    /// The gesture: press, release, press again, nothing in between.
    #[test]
    fn two_taps_of_shift_fire() {
        let start = std::time::Instant::now();
        let mut tap = DoubleTap::default();
        assert!(!tap.modifiers(true, true, start));
        assert!(!tap.modifiers(false, true, start));
        assert!(tap.modifiers(true, true, start + Duration::from_millis(120)));
    }

    #[test]
    fn a_second_tap_too_late_is_a_first_one() {
        let start = std::time::Instant::now();
        let mut tap = DoubleTap::default();
        tap.modifiers(true, true, start);
        tap.modifiers(false, true, start);
        assert!(!tap.modifiers(true, true, start + DOUBLE_TAP + Duration::from_millis(1)));
    }

    /// Two capitals typed a moment apart: the same four modifier changes, with
    /// a letter in each pair.
    #[test]
    fn a_letter_between_the_taps_breaks_them() {
        let start = std::time::Instant::now();
        let mut tap = DoubleTap::default();
        tap.modifiers(true, true, start);
        tap.interrupt();
        tap.modifiers(false, true, start + Duration::from_millis(40));
        assert!(!tap.modifiers(true, true, start + Duration::from_millis(80)));
    }

    #[test]
    fn shift_with_another_modifier_is_not_a_tap() {
        let start = std::time::Instant::now();
        let mut tap = DoubleTap::default();
        tap.modifiers(true, true, start);
        tap.modifiers(false, true, start);
        // `Ctrl+Shift+F`, right after a tap.
        assert!(!tap.modifiers(true, false, start + Duration::from_millis(50)));
    }

    #[test]
    fn the_name_begins_after_the_last_slash() {
        assert_eq!(name_at("src/ui/app.rs"), 7);
        assert_eq!(name_at("app.rs"), 0);
    }

    #[test]
    fn the_highlight_is_cut_where_the_name_begins() {
        // `src/ui/app.rs`, with `ui` and `app` matched.
        let (folders, name) = split(&ranges(&[(4, 6), (7, 10)]), 7);
        assert_eq!(folders, ranges(&[(4, 6)]));
        assert_eq!(name, ranges(&[(0, 3)]));
    }

    #[test]
    fn a_range_straddling_the_cut_is_split_in_two() {
        // `src/ui/app.rs` matched on `i/a`: one range across the slash.
        let (folders, name) = split(&ranges(&[(5, 8)]), 7);
        assert_eq!(folders, ranges(&[(5, 7)]));
        assert_eq!(name, ranges(&[(0, 1)]));
    }

    #[test]
    fn the_list_stops_at_the_cap() {
        let paths: Vec<String> = (0..MAX_ROWS + 50).map(|n| format!("src/a{n}.rs")).collect();
        let candidates: Vec<&str> = paths.iter().map(String::as_str).collect();
        assert_eq!(rank("a", &candidates).len(), MAX_ROWS);
    }

    /// Same score, same length: the order must not depend on where the paths
    /// sat in the input.
    #[test]
    fn ties_are_broken_the_same_way_every_time() {
        let forward = ranked("a", &["src/ab.rs", "src/aa.rs"]);
        let backward = ranked("a", &["src/aa.rs", "src/ab.rs"]);
        assert_eq!(forward, backward);
    }

    fn paths(names: &[&str]) -> Vec<PathBuf> {
        names.iter().map(PathBuf::from).collect()
    }

    #[test]
    fn a_file_read_again_moves_to_the_head_and_is_not_listed_twice() {
        let mut history = paths(&["a.rs", "b.rs", "c.rs"]);
        assert!(promote(&mut history, Path::new("c.rs")));
        assert_eq!(history, paths(&["c.rs", "a.rs", "b.rs"]));
    }

    /// The tab one is already on: every switch comes through here, and a
    /// rewrite of the same list would be a state file changing for nothing.
    #[test]
    fn the_file_already_at_the_head_changes_nothing() {
        let mut history = paths(&["a.rs", "b.rs"]);
        assert!(!promote(&mut history, Path::new("a.rs")));
        assert_eq!(history, paths(&["a.rs", "b.rs"]));
    }

    #[test]
    fn the_history_stops_at_the_cap() {
        let mut history = Vec::new();
        for n in 0..MAX_RECENT + 10 {
            promote(&mut history, Path::new(&format!("src/a{n}.rs")));
        }
        assert_eq!(history.len(), MAX_RECENT);
        assert_eq!(
            history[0],
            PathBuf::from(&format!("src/a{}.rs", MAX_RECENT + 9))
        );
    }

    /// A file the checkout no longer holds — deleted, renamed, or on a branch
    /// since left — is a row that would open nothing.
    #[test]
    fn a_remembered_file_the_project_lost_is_left_out() {
        let history = paths(&["gone.rs", "a.rs"]);
        let known = paths(&["a.rs", "b.rs"]);
        assert_eq!(recent(&history, &known), paths(&["a.rs"]));
    }

    #[test]
    fn the_history_keeps_the_order_it_was_read_in() {
        let history = paths(&["c.rs", "a.rs", "b.rs"]);
        let known = paths(&["a.rs", "b.rs", "c.rs"]);
        assert_eq!(recent(&history, &known), paths(&["c.rs", "a.rs", "b.rs"]));
    }
}
