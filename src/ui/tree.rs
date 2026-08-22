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

/// Turns a list of paths into a tree.
///
/// Directories come before files, in alphabetical order; that is a
/// `BTreeMap`'s, and it has to be stable from one refresh to the next — a list
/// that reorders itself on every `git status` is unreadable.
pub fn build(paths: &[PathBuf], folds: Folds) -> Vec<Entry> {
    build_subset(paths, None, folds)
}

/// The same thing, restricted to some of the paths.
///
/// This is what a search needs: the indices returned always refer to the
/// **whole** list, the one the caller holds, and never to the subset —
/// otherwise a lookup table would be needed on every keystroke, and the row
/// clicked would not open the right file.
pub fn build_subset(paths: &[PathBuf], keep: Option<&[usize]>, folds: Folds) -> Vec<Entry> {
    let mut root = Node::default();
    match keep {
        Some(keep) => {
            for &index in keep {
                if let Some(path) = paths.get(index) {
                    root.insert(path, index);
                }
            }
        }
        None => {
            for (index, path) in paths.iter().enumerate() {
                root.insert(path, index);
            }
        }
    }
    let mut out = Vec::new();
    emit(&root, paths, Path::new(""), 0, folds, &mut out);
    out
}

#[derive(Default)]
struct Node {
    dirs: BTreeMap<String, Node>,
    leaves: Vec<usize>,
}

impl Node {
    fn insert(&mut self, path: &Path, index: usize) {
        let mut node = self;
        if let Some(parent) = path.parent() {
            for component in parent.components() {
                let name = component.as_os_str().to_string_lossy().into_owned();
                node = node.dirs.entry(name).or_default();
            }
        }
        node.leaves.push(index);
    }

    /// Every index in the subtree, in the order they would be displayed.
    fn all(&self, out: &mut Vec<usize>) {
        for child in self.dirs.values() {
            child.all(out);
        }
        out.extend(self.leaves.iter().copied());
    }
}

fn emit(
    node: &Node,
    paths: &[PathBuf],
    prefix: &Path,
    depth: usize,
    folds: Folds,
    out: &mut Vec<Entry>,
) {
    for (name, child) in &node.dirs {
        // Merging intermediate directories: as long as a directory has one
        // subdirectory and no files, all it adds is a level of indentation.
        // Without this, a Laravel project costs six levels before the first
        // file.
        let mut label = name.clone();
        let mut path = prefix.join(name);
        let mut deepest = child;
        while deepest.leaves.is_empty() && deepest.dirs.len() == 1 {
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
            emit(deepest, paths, &path, depth + 1, folds, out);
        }
    }

    let mut leaves = node.leaves.clone();
    leaves.sort_by_key(|index| {
        paths[*index]
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default()
    });
    out.extend(leaves.into_iter().map(|index| Entry::Leaf { index, depth }));
}

#[cfg(test)]
mod tests {
    use super::*;

    fn paths(list: &[&str]) -> Vec<PathBuf> {
        list.iter().map(PathBuf::from).collect()
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
        let entries = build_subset(&paths, Some(&[2]), Folds::OpenBut(&HashSet::new()));
        assert_eq!(labels(&entries, &paths), vec!["src/", "  c.rs"]);
        match entries[1] {
            Entry::Leaf { index, .. } => assert_eq!(index, 2),
            _ => panic!("expected a leaf"),
        }
    }

    #[test]
    fn a_flat_list_has_no_directory_at_all() {
        let paths = paths(&["b.rs", "a.rs"]);
        let entries = build(&paths, Folds::OpenBut(&HashSet::new()));
        assert_eq!(labels(&entries, &paths), vec!["a.rs", "b.rs"]);
    }
}
