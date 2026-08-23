//! Turning a list of paths into a collapsible tree.
//!
//! Two lists use it — a review's files and the project's — and they do not
//! display the same things: one carries checkboxes and change volumes, the
//! other a git status and nothing else. This module therefore knows **only
//! paths**, and returns **indices** into the list it was given: it is up to
//! the caller to decide what a row shows.
//!
//! Indices and not values, because the same leaf appears in the subtree of
//! each of its parent directories: a Laravel project of forty thousand files
//! would otherwise make hundreds of thousands of `PathBuf` clones on every
//! rebuild.

use std::collections::{BTreeMap, HashSet};
use std::path::{Path, PathBuf};

/// A row of the tree.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Entry {
    Dir {
        /// Full path, and collapse key. It is the deepest directory of the
        /// merged chain: collapsing `app/Http` and collapsing
        /// `app/Http/Livewire` are two different gestures, but a merged chain
        /// only offers one.
        path: PathBuf,
        /// What is displayed: one segment, or the merged chain.
        label: String,
        depth: usize,
        collapsed: bool,
        /// Every index in the subtree, including what a collapse hides:
        /// ticking a closed directory must act on what it contains, not on
        /// what is visible of it.
        leaves: Vec<usize>,
    },
    Leaf {
        index: usize,
        depth: usize,
    },
}

/// Which directories are open, and which way the set is read.
///
/// The two lists that use this module do not start from the same place, and a
/// single polarity would be wrong for one of them. A review shows a few dozen
/// files and is meant to be read wide open, so it records what was **closed**.
/// The project explorer opens on the whole worktree — forty thousand files on
/// a Laravel project — where everything unfolded is a list nobody scrolls, so
/// it starts shut and records what was **opened**.
///
/// Recording the exception and not the state is what keeps either set small:
/// seeding one with every directory of a project would be a set the size of
/// the tree, rebuilt on every fold.
#[derive(Debug, Clone, Copy)]
pub enum Folds<'a> {
    /// Open, except the directories named.
    OpenBut(&'a HashSet<PathBuf>),
    /// Shut, except the directories named.
    ShutBut(&'a HashSet<PathBuf>),
}

impl Folds<'_> {
    fn hides(&self, path: &Path) -> bool {
        match self {
            Self::OpenBut(named) => named.contains(path),
            Self::ShutBut(named) => !named.contains(path),
        }
    }
}

/// The tree of a list of paths: built once, folded as often as one likes.
///
/// The split matters on a real checkout. Turning a hundred thousand paths into
/// this tree costs some twenty milliseconds — a `BTreeMap` node per folder —
/// and **a collapse changes none of it**: what a fold changes is which rows
/// come out, which is `rows` and costs a walk of what is visible. Rebuilding
/// the whole thing on every chevron is what made `target/` take a fifth of a
/// second to open.
#[derive(Default)]
pub struct Tree {
    root: Node,
}

impl Tree {
    pub fn new(paths: &[PathBuf]) -> Self {
        Self::of(paths, None, &[])
    }

    /// The same thing, told which of the paths are **directories**.
    ///
    /// A tree built from paths has no other way to know: a path with nothing
    /// under it is a file, and `vendor/` — which git names and refuses to walk
    /// — would draw itself as one. Such a path makes a directory node instead
    /// of a row, and its index still counts in the subtree: greying a folder
    /// asks whether every index under it is excluded, and for a folder nobody
    /// has opened that index is the only one there.
    ///
    /// A directory stays in this list once its contents have arrived, or it
    /// would be drawn twice — once as the folder its files make, once as a
    /// file of its own.
    pub fn with_dirs(paths: &[PathBuf], dirs: &[PathBuf]) -> Self {
        Self::of(paths, None, dirs)
    }

    /// The same thing, restricted to some of the paths.
    ///
    /// This is what a search needs: the indices carried always refer to the
    /// **whole** list, the one the caller holds, and never to the subset —
    /// otherwise a lookup table would be needed on every keystroke, and the row
    /// clicked would not open the right file.
    pub fn subset(paths: &[PathBuf], keep: &[usize], dirs: &[PathBuf]) -> Self {
        Self::of(paths, Some(keep), dirs)
    }

    fn of(paths: &[PathBuf], keep: Option<&[usize]>, dirs: &[PathBuf]) -> Self {
        // Sorted and searched rather than walked per path: `dirs` holds the few
        // hundred folders git stopped at, and `insert` runs once per file.
        let is_dir = |path: &Path| dirs.binary_search_by(|d| d.as_path().cmp(path)).is_ok();
        let mut root = Node::default();
        let insert = |root: &mut Node, path: &Path, index: usize| {
            if !dirs.is_empty() && is_dir(path) {
                root.insert_dir(path, index);
            } else {
                root.insert(path, index);
            }
        };
        match keep {
            Some(keep) => {
                for &index in keep {
                    if let Some(path) = paths.get(index) {
                        insert(&mut root, path, index);
                    }
                }
            }
            None => {
                for (index, path) in paths.iter().enumerate() {
                    insert(&mut root, path, index);
                }
            }
        }
        // Sorted here and not at every emission: the order of a folder's files
        // does not depend on what is folded, and comparing file names is the
        // one place this module touches the paths themselves.
        root.sort(paths);
        Self { root }
    }

    /// The rows to display, given what is folded.
    ///
    /// Directories come before files, in alphabetical order; that is a
    /// `BTreeMap`'s, and it has to be stable from one refresh to the next — a
    /// list that reorders itself on every `git status` is unreadable.
    pub fn rows(&self, folds: Folds) -> Vec<Entry> {
        let mut out = Vec::new();
        emit(&self.root, Path::new(""), 0, folds, &mut out);
        out
    }
}

/// Turns a list of paths into a tree, in one go.
///
/// For the lists that are rebuilt anyway — a review counts hundreds of files,
/// not tens of thousands. The explorer keeps its `Tree`.
pub fn build(paths: &[PathBuf], folds: Folds) -> Vec<Entry> {
    Tree::new(paths).rows(folds)
}

#[derive(Default)]
struct Node {
    dirs: BTreeMap<String, Node>,
    leaves: Vec<usize>,
    /// The index of the path that **is** this directory, when it came from the
    /// list as a directory of its own rather than being deduced from a file
    /// under it. It counts in `all` — it is the only index a folder nobody has
    /// opened has — and `emit` never draws it as a row.
    own: Option<usize>,
}

impl Node {
    fn insert(&mut self, path: &Path, index: usize) {
        let mut node = self;
        if let Some(parent) = path.parent() {
            for component in parent.components() {
                let name = component.as_os_str().to_string_lossy();
                // Looked up before it is owned. `entry` takes a `String`, which
                // means allocating the segment's name **again** for every file
                // that goes through the same folder: on a checkout whose
                // `target/` holds a hundred thousand files, that is half a
                // million allocations for nothing.
                node = if node.dirs.contains_key(name.as_ref()) {
                    node.dirs.get_mut(name.as_ref()).expect("just looked up")
                } else {
                    node.dirs.entry(name.into_owned()).or_default()
                };
            }
        }
        node.leaves.push(index);
    }

    /// The same, for a path that names a directory: it goes all the way down
    /// and marks the node instead of leaving a file behind.
    fn insert_dir(&mut self, path: &Path, index: usize) {
        let mut node = self;
        for component in path.components() {
            let name = component.as_os_str().to_string_lossy();
            node = if node.dirs.contains_key(name.as_ref()) {
                node.dirs.get_mut(name.as_ref()).expect("just looked up")
            } else {
                node.dirs.entry(name.into_owned()).or_default()
            };
        }
        node.own = Some(index);
    }

    fn sort(&mut self, paths: &[PathBuf]) {
        // `sort_by` and not `sort_by_key`: a key is computed at every
        // comparison, and an owned `String` there is `n log n` allocations for
        // a folder that holds nothing but files.
        self.leaves
            .sort_by(|a, b| paths[*a].file_name().cmp(&paths[*b].file_name()));
        for child in self.dirs.values_mut() {
            child.sort(paths);
        }
    }

    /// Every index in the subtree, in the order they would be displayed.
    fn all(&self, out: &mut Vec<usize>) {
        out.extend(self.own);
        for child in self.dirs.values() {
            child.all(out);
        }
        out.extend(self.leaves.iter().copied());
    }
}

fn emit(node: &Node, prefix: &Path, depth: usize, folds: Folds, out: &mut Vec<Entry>) {
    for (name, child) in &node.dirs {
        // Merging intermediate directories: as long as a directory has one
        // subdirectory and no files, all it adds is a level of indentation.
        // Without this, a Laravel project costs six levels before the first
        // file.
        let mut label = name.clone();
        let mut path = prefix.join(name);
        let mut deepest = child;
        while deepest.own.is_none() && deepest.leaves.is_empty() && deepest.dirs.len() == 1 {
            let (name, child) = deepest.dirs.iter().next().expect("exactly one child");
            label.push('/');
            label.push_str(name);
            path = path.join(name);
            deepest = child;
        }

        let mut leaves = Vec::new();
        deepest.all(&mut leaves);
        let is_collapsed = folds.hides(&path);
        out.push(Entry::Dir {
            label,
            depth,
            collapsed: is_collapsed,
            leaves,
            path: path.clone(),
        });
        if !is_collapsed {
            emit(deepest, &path, depth + 1, folds, out);
        }
    }

    out.extend(node.leaves.iter().map(|index| Entry::Leaf {
        index: *index,
        depth,
    }));
}

#[cfg(test)]
mod tests {
    use super::*;

    fn paths(list: &[&str]) -> Vec<PathBuf> {
        list.iter().map(PathBuf::from).collect()
    }

    /// The same, for the directory list `with_dirs` takes.
    fn paths_of(list: &[&str]) -> Vec<PathBuf> {
        paths(list)
    }

    fn labels(entries: &[Entry], paths: &[PathBuf]) -> Vec<String> {
        entries
            .iter()
            .map(|entry| match entry {
                Entry::Dir { label, depth, .. } => format!("{}{label}/", "  ".repeat(*depth)),
                Entry::Leaf { index, depth } => format!(
                    "{}{}",
                    "  ".repeat(*depth),
                    paths[*index].file_name().unwrap().to_string_lossy()
                ),
            })
            .collect()
    }

    #[test]
    fn lonely_directories_are_merged_into_one_line() {
        let paths = paths(&["app/Http/Livewire/Forms/Quote.php", "README.md"]);
        let entries = build(&paths, Folds::OpenBut(&HashSet::new()));
        assert_eq!(
            labels(&entries, &paths),
            vec!["app/Http/Livewire/Forms/", "  Quote.php", "README.md"]
        );
    }

    #[test]
    fn directories_come_before_files_at_each_level() {
        let paths = paths(&["z.rs", "src/a.rs", "a.rs"]);
        let entries = build(&paths, Folds::OpenBut(&HashSet::new()));
        assert_eq!(
            labels(&entries, &paths),
            vec!["src/", "  a.rs", "a.rs", "z.rs"]
        );
    }

    #[test]
    fn a_collapsed_directory_hides_its_files_but_still_carries_them() {
        let paths = paths(&["src/ui/a.rs", "src/ui/b.rs"]);
        let collapsed: HashSet<PathBuf> = [PathBuf::from("src/ui")].into_iter().collect();
        let entries = build(&paths, Folds::OpenBut(&collapsed));
        assert_eq!(entries.len(), 1, "the files are hidden");
        // Ticking a closed directory must act on what it contains, not on what
        // is visible of it.
        match &entries[0] {
            Entry::Dir {
                leaves, collapsed, ..
            } => {
                assert!(collapsed);
                assert_eq!(leaves, &vec![0, 1]);
            }
            other => panic!("expected a directory: {other:?}"),
        }
    }

    /// The explorer's polarity: shut unless named. A directory nobody opened
    /// hides its files, and opening one level does not cascade to the next —
    /// which is the whole difference with seeding the other set with the top
    /// level only.
    #[test]
    fn a_shut_tree_opens_one_level_at_a_time() {
        let paths = paths(&["src/ui/a.rs", "README.md"]);
        let shut: HashSet<PathBuf> = HashSet::new();
        assert_eq!(
            labels(&build(&paths, Folds::ShutBut(&shut)), &paths),
            vec!["src/ui/", "README.md"],
            "a merged chain is one row, and it is closed"
        );
        let opened: HashSet<PathBuf> = [PathBuf::from("src/ui")].into_iter().collect();
        assert_eq!(
            labels(&build(&paths, Folds::ShutBut(&opened)), &paths),
            vec!["src/ui/", "  a.rs", "README.md"]
        );
    }

    /// The indices returned refer to the whole list, not to the subset: that
    /// is what makes filtering possible without a lookup table.
    #[test]
    fn a_subset_still_indexes_the_whole_list() {
        let paths = paths(&["a.rs", "src/b.rs", "src/c.rs"]);
        let entries = Tree::subset(&paths, &[2], &[]).rows(Folds::OpenBut(&HashSet::new()));
        assert_eq!(labels(&entries, &paths), vec!["src/", "  c.rs"]);
        match entries[1] {
            Entry::Leaf { index, .. } => assert_eq!(index, 2),
            _ => panic!("expected a leaf"),
        }
    }

    /// A directory git named and refused to walk draws a row of its own, with
    /// a chevron and nothing under it. Without this it has no child, so a tree
    /// built from paths reads it as a file — `vendor/` with a file icon.
    #[test]
    fn a_directory_nobody_opened_is_still_a_directory() {
        let paths = paths(&["README.md", "vendor"]);
        let tree = Tree::with_dirs(&paths, &paths_of(&["vendor"]));
        let entries = tree.rows(Folds::OpenBut(&HashSet::new()));
        assert_eq!(labels(&entries, &paths), vec!["vendor/", "README.md"]);
        match &entries[0] {
            // Its own index, and it is the only one: greying a folder asks
            // whether everything under it is excluded, and this is what there
            // is to ask about.
            Entry::Dir { leaves, .. } => assert_eq!(leaves, &vec![1]),
            other => panic!("expected a directory, got {other:?}"),
        }
    }

    /// A chain of lonely directories ending in an unopened one is still merged
    /// — and the row that comes out is the deepest, which is the one that has
    /// something to read.
    #[test]
    fn a_merged_chain_ends_on_the_unopened_directory() {
        let paths = paths(&["docs/reviews/dev"]);
        let entries = Tree::with_dirs(&paths, &paths_of(&["docs/reviews/dev"]))
            .rows(Folds::OpenBut(&HashSet::new()));
        assert_eq!(labels(&entries, &paths), vec!["docs/reviews/dev/"]);
        match &entries[0] {
            Entry::Dir { path, .. } => assert_eq!(path, &PathBuf::from("docs/reviews/dev")),
            other => panic!("expected a directory, got {other:?}"),
        }
    }

    /// Once its contents arrive it stays in the list of directories, and it
    /// must not then be drawn twice — once as the folder its files make, once
    /// as a file of its own.
    #[test]
    fn a_directory_that_has_been_read_is_drawn_once() {
        let paths = paths(&["vendor", "vendor/autoload.php"]);
        let entries =
            Tree::with_dirs(&paths, &paths_of(&["vendor"])).rows(Folds::OpenBut(&HashSet::new()));
        assert_eq!(labels(&entries, &paths), vec!["vendor/", "  autoload.php"]);
    }

    /// And that holds of a search too: a directory whose contents have arrived
    /// is still a directory, so a subset told otherwise would draw `vendor/`
    /// both as the folder its files make and as a leaf of its own.
    #[test]
    fn a_search_draws_a_read_directory_once() {
        let paths = paths(&["vendor", "vendor/autoload.php"]);
        let entries = Tree::subset(&paths, &[0, 1], &paths_of(&["vendor"]))
            .rows(Folds::OpenBut(&HashSet::new()));
        assert_eq!(labels(&entries, &paths), vec!["vendor/", "  autoload.php"]);
    }

    /// The tree is held from one fold to the next, and folding it must give
    /// exactly what building it afresh would. This is what a `target/` of a
    /// hundred thousand files buys: twenty milliseconds once instead of on
    /// every chevron.
    #[test]
    fn one_tree_answers_every_fold() {
        let paths = paths(&["src/ui/a.rs", "src/ui/b.rs", "README.md"]);
        let tree = Tree::new(&paths);
        let shut = HashSet::new();
        assert_eq!(
            tree.rows(Folds::ShutBut(&shut)),
            build(&paths, Folds::ShutBut(&shut))
        );
        let opened: HashSet<PathBuf> = [PathBuf::from("src/ui")].into_iter().collect();
        assert_eq!(
            tree.rows(Folds::ShutBut(&opened)),
            build(&paths, Folds::ShutBut(&opened))
        );
        // And back: nothing of the fold is kept in the tree.
        assert_eq!(
            tree.rows(Folds::ShutBut(&shut)),
            build(&paths, Folds::ShutBut(&shut))
        );
    }

    #[test]
    fn a_flat_list_has_no_directory_at_all() {
        let paths = paths(&["b.rs", "a.rs"]);
        let entries = build(&paths, Folds::OpenBut(&HashSet::new()));
        assert_eq!(labels(&entries, &paths), vec!["a.rs", "b.rs"]);
    }
}
