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

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use super::tree;

/// What the right of a board shows.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub enum View {
    /// The digest of all the others, in one screen.
    Home,
    /// The branch, how far it is from its remote and its base, and what
    /// waits for a commit.
    Git,
    /// What the branch has written since its base — a face of the git tab
    /// (`GitFace::Review`), kept as a view for the store, which may hold it,
    /// and for the home's cards, which open it.
    Review,
    /// The branch's pull request, or the form that opens one — a face of
    /// the git tab too (`GitFace::Pr`), for the same reasons.
    Pr,
    /// Its tests — the ones the branch touched, by default — and the run
    /// being followed.
    Tests,
    /// The notes, the principal one first — and, a face of the same tab,
    /// the worktree's `TODO.md`.
    Notes,
    /// The worktree's `TODO.md` — a face of the notes tab (`NotesFace::Todo`),
    /// kept as a view for the store and the home's cards.
    Todo,
    /// Its terminals, side by side: where its agents work.
    Terminals,
}

impl View {
    /// In the tabs' order.
    pub const ALL: [View; 5] = [
        View::Home,
        View::Git,
        View::Tests,
        View::Notes,
        View::Terminals,
    ];
}

/// A face of the git tab: what waits for a commit, the history, what the
/// branch has written since its base, its pull request. Everything the
/// branch is, under one tab.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum GitFace {
    #[default]
    Changes,
    History,
    Review,
    Pr,
}

impl GitFace {
    /// In the git tab's order.
    pub const ALL: [GitFace; 4] = [
        GitFace::Changes,
        GitFace::History,
        GitFace::Review,
        GitFace::Pr,
    ];
}

/// A face of the notes tab: the notes, and the to-do list.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum NotesFace {
    #[default]
    Notes,
    Todo,
}

/// A face of a tab that has several.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Face {
    Git(GitFace),
    Notes(NotesFace),
}

/// Where asking for a view lands: the review and the pull request are faces
/// of the git tab, the to-do list one of the notes tab — the tab shown, on
/// that face.
pub fn landing(view: View) -> (View, Option<Face>) {
    match view {
        View::Review => (View::Git, Some(Face::Git(GitFace::Review))),
        View::Pr => (View::Git, Some(Face::Git(GitFace::Pr))),
        View::Todo => (View::Notes, Some(Face::Notes(NotesFace::Todo))),
        other => (other, None),
    }
}

/// The view a board shows: the one chosen — the git tab for one of its
/// faces —, else its home.
pub fn view_of(chosen: Option<View>) -> View {
    landing(chosen.unwrap_or(View::Home)).0
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

/// A branch carrying up to this many files opens its review card wide open;
/// past it, shut on its top folders. What a card says of a large branch is
/// **where** the work went — a thousand names in a row say nothing of it.
pub const REVIEW_OPEN_UP_TO: usize = 40;

/// A row of the review card's tree.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReviewRow {
    Dir {
        /// The fold key: see [`tree::Entry::Dir`].
        path: PathBuf,
        label: String,
        depth: usize,
        collapsed: bool,
        /// Its whole subtree, folded part included.
        files: usize,
        added: usize,
        removed: usize,
    },
    File {
        path: PathBuf,
        depth: usize,
        added: usize,
        removed: usize,
    },
}

/// The review card's rows: the branch's files — `(path, added, removed)` —
/// as a tree, by their place in the project. `toggled` holds the folders the
/// hand folded or unfolded, read against how the card opens
/// ([`REVIEW_OPEN_UP_TO`]).
pub fn review_rows(
    files: &[(PathBuf, usize, usize)],
    toggled: &HashSet<PathBuf>,
) -> Vec<ReviewRow> {
    let folds = if files.len() <= REVIEW_OPEN_UP_TO {
        tree::Folds::OpenBut(toggled)
    } else {
        tree::Folds::ShutBut(toggled)
    };
    let paths: Vec<PathBuf> = files.iter().map(|file| file.0.clone()).collect();
    tree::build(&paths, folds)
        .into_iter()
        .map(|entry| match entry {
            tree::Entry::Dir {
                path,
                label,
                depth,
                collapsed,
                leaves,
            } => ReviewRow::Dir {
                path,
                label,
                depth,
                collapsed,
                files: leaves.len(),
                added: leaves.iter().map(|index| files[*index].1).sum(),
                removed: leaves.iter().map(|index| files[*index].2).sum(),
            },
            tree::Entry::Leaf { index, depth } => ReviewRow::File {
                path: files[index].0.clone(),
                depth,
                added: files[index].1,
                removed: files[index].2,
            },
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

#[cfg(test)]
mod tests {
    use super::*;

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

    /// A board opens on its home; on what was chosen, once chosen — the
    /// review and the pull request being the git tab's.
    #[test]
    fn a_board_opens_on_its_home() {
        assert_eq!(view_of(None), View::Home);
        assert_eq!(view_of(Some(View::Tests)), View::Tests);
        assert_eq!(view_of(Some(View::Pr)), View::Git);
        assert_eq!(
            landing(View::Review),
            (View::Git, Some(Face::Git(GitFace::Review)))
        );
        assert_eq!(
            landing(View::Todo),
            (View::Notes, Some(Face::Notes(NotesFace::Todo)))
        );
        assert!(!View::ALL.contains(&View::Pr) && !View::ALL.contains(&View::Todo));
    }

    /// A small branch reads wide open, a large one on its top folders; a
    /// folder counts its whole subtree, and the hand's fold is the exception.
    #[test]
    fn the_review_card_is_a_tree_open_while_small() {
        let file = |path: &str, added| (PathBuf::from(path), added, 1);
        let small = vec![
            file("app/Http/A.php", 3),
            file("app/Http/B.php", 4),
            file("lang/fr.json", 5),
        ];
        let none = HashSet::new();
        let rows = review_rows(&small, &none);
        assert_eq!(
            rows[0],
            ReviewRow::Dir {
                path: PathBuf::from("app/Http"),
                label: "app/Http".into(),
                depth: 0,
                collapsed: false,
                files: 2,
                added: 7,
                removed: 2,
            }
        );
        assert_eq!(rows.len(), 5);

        let folded = HashSet::from([PathBuf::from("app/Http")]);
        assert_eq!(review_rows(&small, &folded).len(), 3);

        let large: Vec<_> = (0..=REVIEW_OPEN_UP_TO)
            .map(|n| file(&format!("src/f{n}.rs"), 1))
            .chain([file("README.md", 1)])
            .collect();
        let rows = review_rows(&large, &none);
        assert!(matches!(
            &rows[0],
            ReviewRow::Dir { collapsed: true, files, .. } if *files == REVIEW_OPEN_UP_TO + 1
        ));
        assert_eq!(rows.len(), 2);
        let opened = HashSet::from([PathBuf::from("src")]);
        assert_eq!(review_rows(&large, &opened).len(), large.len() + 1);
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
