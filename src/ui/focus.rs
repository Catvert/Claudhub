//! The focus view's board: tabs, and **one** view under them — the home, a
//! digest of the worktree in one screen (its git, its review, its principal
//! note, its to-do list, its agents), or the whole of one of those.
//!
//! One at a time: views of different kinds side by side — a diff beside two
//! terminals beside a note — was a board to arrange before it was a board
//! to read, and a digest in a column of its own left each view a strip. A
//! card of the home opens its tab; the tab chosen is kept per worktree.
//!
//! Pure: the view hands in what it knows, this says what shows.

use std::path::{Path, PathBuf};

/// What the right of a board shows.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub enum View {
    /// The digest of all the others, in one screen.
    Home,
    /// The branch, how far it is from its remote and its base, and what
    /// waits for a commit.
    Git,
    /// What the branch has written since its base.
    Review,
    /// The notes, the principal one first.
    Notes,
    /// The worktree's `TODO.md`.
    Todo,
    /// Its terminals, side by side: where its agents work.
    Terminals,
}

impl View {
    /// In the tabs' order.
    pub const ALL: [View; 6] = [
        View::Home,
        View::Git,
        View::Review,
        View::Notes,
        View::Todo,
        View::Terminals,
    ];
}

/// The view a board shows: the one chosen, else its home.
pub fn view_of(chosen: Option<View>) -> View {
    chosen.unwrap_or(View::Home)
}

/// The principal note among a worktree's — `(path, created)` —: the one
/// pinned while it is still there, else the newest. A note without a date
/// is older than any with one.
pub fn principal_note(
    pinned: Option<&Path>,
    notes: &[(PathBuf, Option<String>)],
) -> Option<PathBuf> {
    if let Some(pinned) = pinned.filter(|pinned| notes.iter().any(|(path, _)| path == pinned)) {
        return Some(pinned.to_path_buf());
    }
    notes
        .iter()
        .max_by(|a, b| a.1.cmp(&b.1))
        .map(|(path, _)| path.clone())
}

/// The files of a review that weigh most — `(path, added, removed)` —, the
/// heaviest first, at most `count`, each with its share of the heaviest's
/// weight: what a bar beside its name is drawn at.
pub fn heaviest(
    files: &[(PathBuf, usize, usize)],
    count: usize,
) -> Vec<(PathBuf, usize, usize, f32)> {
    let mut files: Vec<&(PathBuf, usize, usize)> = files.iter().collect();
    files.sort_by(|a, b| (b.1 + b.2).cmp(&(a.1 + a.2)).then_with(|| a.0.cmp(&b.0)));
    let top = files.first().map_or(0, |file| file.1 + file.2).max(1) as f32;
    files
        .into_iter()
        .take(count)
        .map(|(path, added, removed)| {
            (
                path.clone(),
                *added,
                *removed,
                (added + removed) as f32 / top,
            )
        })
        .collect()
}

/// The terminal a board's home shows under its sub-tabs — `terminals` in
/// the order they were opened, with what their agent is doing —: the one
/// chosen while it lives, else the first whose agent asks something, else
/// the last opened.
pub fn shown_terminal(chosen: Option<u64>, terminals: &[(u64, bool)]) -> Option<u64> {
    if let Some(chosen) = chosen.filter(|id| terminals.iter().any(|(t, _)| t == id)) {
        return Some(chosen);
    }
    terminals
        .iter()
        .find(|(_, waiting)| *waiting)
        .or(terminals.last())
        .map(|(id, _)| *id)
}

/// The worktrees on show, side by side: the ones chosen in the sidebar, as
/// long as the window's own — `primary` — is among them; else that one
/// alone. Choosing a worktree anywhere else in the window is choosing it
/// here, and the others go.
///
/// In the sidebar's order, the boards reading as the list they were picked
/// from; what the pickers no longer show drops out, the primary never.
pub fn shown_worktrees(
    chosen: &[PathBuf],
    primary: Option<&Path>,
    on_show: &[PathBuf],
) -> Vec<PathBuf> {
    let Some(primary) = primary else {
        return Vec::new();
    };
    if !chosen.iter().any(|path| path == primary) {
        return vec![primary.to_path_buf()];
    }
    let mut shown: Vec<PathBuf> = chosen
        .iter()
        .filter(|path| path.as_path() == primary || on_show.contains(path))
        .cloned()
        .collect();
    shown.dedup();
    // Not in the list — another project's — reads first.
    shown.sort_by_key(|path| on_show.iter().position(|shown| shown == path));
    shown
}

/// A Ctrl+click in the sidebar: the worktree joins what is shown, or
/// leaves it — never the last one, a screen with nothing on it being no
/// answer to anything.
pub fn toggled(shown: &[PathBuf], path: &Path) -> Vec<PathBuf> {
    if !shown.iter().any(|shown| shown == path) {
        let mut shown = shown.to_vec();
        shown.push(path.to_path_buf());
        return shown;
    }
    if shown.len() == 1 {
        return shown.to_vec();
    }
    shown
        .iter()
        .filter(|shown| shown.as_path() != path)
        .cloned()
        .collect()
}

/// What stands for a worktree on the sidebar's rail: two letters of its
/// name — the first of two words, or the start of one.
pub fn initials(label: &str) -> String {
    let name = label.rsplit('/').next().unwrap_or(label);
    let words: Vec<&str> = name
        .split(|c: char| !c.is_alphanumeric())
        .filter(|word| !word.is_empty())
        .collect();
    let letters: String = match words.as_slice() {
        [] => String::new(),
        [one] => one.chars().take(2).collect(),
        [first, second, ..] => first
            .chars()
            .take(1)
            .chain(second.chars().take(1))
            .collect(),
    };
    letters.to_uppercase()
}

/// Where the arrows over the boards take the row: the left edge of the
/// next column past the one at the view's edge, or of the one before it —
/// `lefts` in the row's own coordinates, `at` how far it is scrolled, `max`
/// how far it can be. Past the last column's start, the end; before the
/// first, the start.
pub fn next_stop(lefts: &[f32], at: f32, max: f32, forward: bool) -> f32 {
    // A pixel either way is the same place: a column the view already
    // starts at is not the next one.
    let target = if forward {
        lefts
            .iter()
            .copied()
            .filter(|left| *left > at + 1.)
            .fold(None, |nearest: Option<f32>, left| {
                Some(nearest.map_or(left, |nearest| nearest.min(left)))
            })
            .unwrap_or(max)
    } else {
        lefts
            .iter()
            .copied()
            .filter(|left| *left < at - 1.)
            .fold(None, |nearest: Option<f32>, left| {
                Some(nearest.map_or(left, |nearest| nearest.max(left)))
            })
            .unwrap_or(0.)
    };
    target.clamp(0., max.max(0.))
}

/// Where a board's title stands in the header over the row: the part of
/// the board in view, `board` its edges in the row, `at` how far the row is
/// scrolled, `room` how much of the header titles may take. `None` when too
/// little of it shows to say anything — a title stays in view as long as
/// its board does, and gives way to the next one's.
pub fn header_span(board: (f32, f32), at: f32, room: f32) -> Option<(f32, f32)> {
    const LEAST: f32 = 48.;
    let left = (board.0 - at).max(0.);
    let right = (board.1 - at).min(room);
    (right - left >= LEAST).then_some((left, right))
}

/// What a worktree's row in the sidebar says: a title, and a second line
/// only when it adds something. The main checkout is named by its branch —
/// its folder is the project's, already the heading above it. A linked
/// worktree is named by its folder, and its branch follows only when it is
/// not the same word again: `wt/rentability-v2` under `rentability-v2`
/// said it twice.
pub fn row_words(label: &str, branch: Option<&str>, is_main: bool) -> (String, Option<String>) {
    match (is_main, branch) {
        (true, Some(branch)) => (branch.to_string(), None),
        (_, None) => (label.to_string(), None),
        (false, Some(branch)) => {
            let last = branch.rsplit('/').next().unwrap_or(branch);
            let same = branch == label || last == label;
            (label.to_string(), (!same).then(|| branch.to_string()))
        }
    }
}

/// A count in few characters, for a line that has little room: the lines a
/// worktree has in progress read as an order of size, not a figure.
pub fn short_count(count: usize) -> String {
    match count {
        0..=999 => count.to_string(),
        1_000..=9_999 => {
            let tenths = (count + 50) / 100;
            if tenths.is_multiple_of(10) {
                format!("{}k", tenths / 10)
            } else {
                format!("{}.{}k", tenths / 10, tenths % 10)
            }
        }
        10_000..=999_999 => format!("{}k", (count + 500) / 1_000),
        _ => format!("{:.1}M", count as f64 / 1_000_000.),
    }
}

/// The bright run going down a sidebar entry's edge while its agent works:
/// where it stands `seconds` in, on an edge `height` tall — from above the
/// top, where it enters, to past the bottom, where it leaves — cut to the
/// edge. `None` between two passes.
pub fn edge_run(height: f32, seconds: f32) -> Option<(f32, f32)> {
    /// Fast enough to read as work, slow enough not to flicker on a row.
    const SPEED: f32 = 48.;
    if height <= 0. {
        return None;
    }
    let run = (height * 0.45).clamp(8., height);
    let at = (seconds * SPEED).rem_euclid(height + run) - run;
    let (top, bottom) = (at.max(0.), (at + run).min(height));
    (bottom > top).then_some((top, bottom))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_run_goes_down_the_edge_and_leaves_it() {
        // An edge of 40: a run of 18, a cycle of 58 points.
        let at = |points: f32| edge_run(40., points / 48.);
        // Entering from above: only its end shows.
        assert_eq!(at(0.), None);
        assert_eq!(at(9.), Some((0., 9.)));
        // Whole, halfway down.
        assert_eq!(at(30.), Some((12., 30.)));
        // Leaving at the bottom, cut to the edge.
        assert_eq!(at(50.), Some((32., 40.)));
        assert_eq!(edge_run(0., 1.), None);
    }

    #[test]
    fn a_row_says_each_thing_once() {
        // The main checkout: its branch, the project being the heading.
        assert_eq!(
            row_words("app-tech", Some("main"), true),
            ("main".into(), None)
        );
        // A linked one whose branch is its folder again, prefixed or not.
        assert_eq!(
            row_words("rentability-v2", Some("wt/rentability-v2"), false),
            ("rentability-v2".into(), None)
        );
        assert_eq!(row_words("dev", Some("dev"), false), ("dev".into(), None));
        // One whose branch says something else.
        assert_eq!(
            row_words("review", Some("fix/login"), false),
            ("review".into(), Some("fix/login".into()))
        );
        // Detached: the folder.
        assert_eq!(row_words("app", None, true), ("app".into(), None));
    }

    #[test]
    fn a_count_shortens_to_its_order_of_size() {
        assert_eq!(short_count(106), "106");
        assert_eq!(short_count(1_000), "1k");
        assert_eq!(short_count(1_240), "1.2k");
        assert_eq!(short_count(9_960), "10k");
        assert_eq!(short_count(160_293), "160k");
        assert_eq!(short_count(2_450_000), "2.5M");
    }

    #[test]
    fn the_arrows_go_from_column_to_column() {
        let lefts = [0., 300., 600., 900.];
        // Forward from the start: the second column; from within one, the
        // next one's start.
        assert_eq!(next_stop(&lefts, 0., 1000., true), 300.);
        assert_eq!(next_stop(&lefts, 450., 1000., true), 600.);
        // Back from within one: its own start; from a start, the one before.
        assert_eq!(next_stop(&lefts, 450., 1000., false), 300.);
        assert_eq!(next_stop(&lefts, 600., 1000., false), 300.);
        // Past the last one, the end; never beyond what scrolls.
        assert_eq!(next_stop(&lefts, 900., 1000., true), 1000.);
        assert_eq!(next_stop(&lefts, 600., 700., true), 700.);
        assert_eq!(next_stop(&lefts, 0., 1000., false), 0.);
    }

    #[test]
    fn a_title_stays_in_view_as_long_as_its_board() {
        // In view, where the board is.
        assert_eq!(header_span((0., 800.), 0., 1200.), Some((0., 800.)));
        // Scrolled past its start, held at the left edge.
        assert_eq!(header_span((0., 800.), 300., 1200.), Some((0., 500.)));
        // Cut at the room the arrows leave.
        assert_eq!(header_span((900., 2000.), 0., 1200.), Some((900., 1200.)));
        // Almost gone: it gives way.
        assert_eq!(header_span((0., 800.), 780., 1200.), None);
    }

    fn notes(list: &[(&str, Option<&str>)]) -> Vec<(PathBuf, Option<String>)> {
        list.iter()
            .map(|(path, created)| (PathBuf::from(path), created.map(str::to_string)))
            .collect()
    }

    /// The one pinned, while it is there; else the newest.
    #[test]
    fn the_principal_note_is_the_pinned_one_else_the_newest() {
        let list = notes(&[
            ("/a.md", Some("2026-09-01T10:00:00Z")),
            ("/b.md", Some("2026-09-20T10:00:00Z")),
            ("/c.md", None),
        ]);
        assert_eq!(
            principal_note(Some(Path::new("/a.md")), &list),
            Some(PathBuf::from("/a.md"))
        );
        assert_eq!(
            principal_note(Some(Path::new("/gone.md")), &list),
            Some(PathBuf::from("/b.md"))
        );
        assert_eq!(principal_note(None, &list), Some(PathBuf::from("/b.md")));
        assert_eq!(
            principal_note(None, &notes(&[("/c.md", None)])),
            Some(PathBuf::from("/c.md"))
        );
        assert_eq!(principal_note(None, &[]), None);
    }

    /// A board opens on its home; on what was chosen, once chosen.
    #[test]
    fn a_board_opens_on_its_home() {
        assert_eq!(view_of(None), View::Home);
        assert_eq!(view_of(Some(View::Todo)), View::Todo);
    }

    /// The heaviest first, each against the heaviest.
    #[test]
    fn the_heaviest_files_come_first() {
        let files = vec![
            (PathBuf::from("a"), 1, 1),
            (PathBuf::from("b"), 30, 10),
            (PathBuf::from("c"), 10, 10),
        ];
        let top = heaviest(&files, 2);
        assert_eq!(top.len(), 2);
        assert_eq!((top[0].0.as_path(), top[0].3), (Path::new("b"), 1.));
        assert_eq!((top[1].0.as_path(), top[1].3), (Path::new("c"), 0.5));
        assert!(heaviest(&[], 3).is_empty());
    }

    /// The one chosen while it lives; else the one asking; else the last.
    #[test]
    fn the_home_shows_the_chosen_terminal_else_the_one_asking() {
        let terminals = [(1, false), (2, true), (3, false)];
        assert_eq!(shown_terminal(Some(3), &terminals), Some(3));
        assert_eq!(shown_terminal(Some(9), &terminals), Some(2));
        assert_eq!(shown_terminal(None, &[(1, false), (3, false)]), Some(3));
        assert_eq!(shown_terminal(None, &[]), None);
    }

    fn paths(names: &[&str]) -> Vec<PathBuf> {
        names.iter().map(PathBuf::from).collect()
    }

    #[test]
    fn the_primary_alone_unless_chosen() {
        let on_show = paths(&["/a", "/b", "/c"]);
        // Chosen elsewhere: that one alone.
        assert_eq!(
            shown_worktrees(&paths(&["/a", "/b"]), Some(Path::new("/c")), &on_show),
            paths(&["/c"])
        );
        // Among the chosen: all of them, in the sidebar's order.
        assert_eq!(
            shown_worktrees(&paths(&["/c", "/a"]), Some(Path::new("/a")), &on_show),
            paths(&["/a", "/c"])
        );
        // What the pickers no longer show drops out, the primary never.
        assert_eq!(
            shown_worktrees(&paths(&["/z", "/b", "/y"]), Some(Path::new("/z")), &on_show),
            paths(&["/z", "/b"])
        );
        assert!(shown_worktrees(&[], None, &on_show).is_empty());
    }

    #[test]
    fn a_ctrl_click_adds_or_removes_never_the_last() {
        let shown = paths(&["/a"]);
        assert_eq!(toggled(&shown, Path::new("/b")), paths(&["/a", "/b"]));
        assert_eq!(toggled(&shown, Path::new("/a")), paths(&["/a"]));
        assert_eq!(
            toggled(&paths(&["/a", "/b"]), Path::new("/a")),
            paths(&["/b"])
        );
    }

    #[test]
    fn two_letters_stand_for_a_worktree() {
        assert_eq!(initials("feature-login"), "FL");
        assert_eq!(initials("wt/fix_merge"), "FM");
        assert_eq!(initials("claudhub"), "CL");
        assert_eq!(initials("é"), "É");
        assert_eq!(initials("--"), "");
    }
}
