//! Mettre une liste de chemins en arborescence repliable.
//!
//! Deux listes s'en servent — les fichiers d'une revue et ceux du projet — et
//! elles n'affichent pas les mêmes choses : l'une porte des cases à cocher et
//! des volumes de modification, l'autre un statut git et rien d'autre. Ce
//! module ne connaît donc **que des chemins**, et rend des **indices** dans la
//! liste qu'on lui a donnée : c'est à l'appelant de décider ce qu'une ligne
//! affiche.
//!
//! Des indices et non des valeurs, parce que la même feuille apparaît dans le
//! sous-arbre de chacun de ses dossiers parents : un projet Laravel de
//! quarante mille fichiers ferait sinon des centaines de milliers de clones de
//! `PathBuf` à chaque reconstruction.

use std::collections::{BTreeMap, HashSet};
use std::path::{Path, PathBuf};

/// Une ligne de l'arborescence.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Entry {
    Dir {
        /// Chemin complet, et clé du repli. C'est celui du dossier le plus
        /// profond de la chaîne fusionnée : replier `app/Http` et replier
        /// `app/Http/Livewire` sont deux gestes différents, mais une chaîne
        /// fusionnée n'en offre qu'un.
        path: PathBuf,
        /// Ce qui s'affiche : un segment, ou la chaîne fusionnée.
        label: String,
        depth: usize,
        collapsed: bool,
        /// Tous les indices du sous-arbre, y compris ce qu'un repli cache :
        /// cocher un dossier fermé doit agir sur ce qu'il contient, et non sur
        /// ce qu'on en voit.
        leaves: Vec<usize>,
    },
    Leaf {
        index: usize,
        depth: usize,
    },
}

/// Met une liste de chemins en arborescence.
///
/// Les dossiers viennent avant les fichiers, dans l'ordre alphabétique ; c'est
/// celui d'un `BTreeMap`, et il doit être stable d'un rafraîchissement à
/// l'autre — une liste qui se réordonne à chaque `git status` est illisible.
pub fn build(paths: &[PathBuf], collapsed: &HashSet<PathBuf>) -> Vec<Entry> {
    let mut root = Node::default();
    for (index, path) in paths.iter().enumerate() {
        root.insert(path, index);
    }
    let mut out = Vec::new();
    emit(&root, paths, Path::new(""), 0, collapsed, &mut out);
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

    /// Tous les indices du sous-arbre, dans l'ordre où ils s'afficheraient.
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
    collapsed: &HashSet<PathBuf>,
    out: &mut Vec<Entry>,
) {
    for (name, child) in &node.dirs {
        // Fusion des dossiers intermédiaires : tant qu'un dossier n'a qu'un
        // sous-dossier et aucun fichier, il n'apporte qu'un niveau
        // d'indentation. Sans cela, un projet Laravel coûte six niveaux avant
        // le premier fichier.
        let mut label = name.clone();
        let mut path = prefix.join(name);
        let mut deepest = child;
        while deepest.leaves.is_empty() && deepest.dirs.len() == 1 {
            let (name, child) = deepest.dirs.iter().next().expect("un seul enfant");
            label.push('/');
            label.push_str(name);
            path = path.join(name);
            deepest = child;
        }

        let mut leaves = Vec::new();
        deepest.all(&mut leaves);
        let is_collapsed = collapsed.contains(&path);
        out.push(Entry::Dir {
            label,
            depth,
            collapsed: is_collapsed,
            leaves,
            path: path.clone(),
        });
        if !is_collapsed {
            emit(deepest, paths, &path, depth + 1, collapsed, out);
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
        let paths = paths(&["app/Http/Livewire/Forms/Devis.php", "README.md"]);
        let entries = build(&paths, &HashSet::new());
        assert_eq!(
            labels(&entries, &paths),
            vec!["app/Http/Livewire/Forms/", "  Devis.php", "README.md"]
        );
    }

    #[test]
    fn directories_come_before_files_at_each_level() {
        let paths = paths(&["z.rs", "src/a.rs", "a.rs"]);
        let entries = build(&paths, &HashSet::new());
        assert_eq!(
            labels(&entries, &paths),
            vec!["src/", "  a.rs", "a.rs", "z.rs"]
        );
    }

    #[test]
    fn a_collapsed_directory_hides_its_files_but_still_carries_them() {
        let paths = paths(&["src/ui/a.rs", "src/ui/b.rs"]);
        let collapsed: HashSet<PathBuf> = [PathBuf::from("src/ui")].into_iter().collect();
        let entries = build(&paths, &collapsed);
        assert_eq!(entries.len(), 1, "les fichiers sont cachés");
        // Cocher un dossier fermé doit agir sur ce qu'il contient, pas sur ce
        // qu'on en voit.
        match &entries[0] {
            Entry::Dir {
                leaves, collapsed, ..
            } => {
                assert!(collapsed);
                assert_eq!(leaves, &vec![0, 1]);
            }
            other => panic!("attendu un dossier : {other:?}"),
        }
    }

    #[test]
    fn a_flat_list_has_no_directory_at_all() {
        let paths = paths(&["b.rs", "a.rs"]);
        let entries = build(&paths, &HashSet::new());
        assert_eq!(labels(&entries, &paths), vec!["a.rs", "b.rs"]);
    }
}
