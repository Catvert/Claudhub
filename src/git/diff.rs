//! Diffs : la liste des fichiers touchés, et le contenu d'un fichier découpé
//! en hunks pour la vue de revue.
//!
//! Claudhub ne calcule pas de diff lui-même — git le fait mieux, avec la
//! détection de renommage, les règles de `.gitattributes` et les filtres de
//! l'utilisateur. Ce module ne fait que lire sa sortie unifiée.

use std::path::{Path, PathBuf};

use anyhow::Result;

use super::{git, split_nul};

/// L'empreinte de l'arbre vide, telle que git la calcule partout.
const EMPTY_TREE: &str = "4b825dc642cb6eb9a060e54bf8d69288fbee4904";

/// Ce que la revue compare.
///
/// `Hash` parce que les fichiers de chaque domaine sont rangés par domaine :
/// deux panneaux montrent deux listes en même temps, et elles ne se
/// chevauchent pas.
/// Sérialisable : une note de relecture retient le domaine où elle a été
/// prise, et le magasin d'état la relit au lancement suivant.
#[derive(Debug, Clone, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub enum Range {
    /// Tout ce qui sépare le répertoire de travail de HEAD, indexé ou non.
    ///
    /// Claudhub ne propose pas de comparer séparément l'index et le répertoire de
    /// travail : la distinction est un détail de plomberie git, et la vue de
    /// revue la restitue par une case à cocher par fichier plutôt que par deux
    /// listes qu'il faut mentalement recoudre.
    Working,
    /// Un commit précis, comparé à son premier parent.
    ///
    /// `parent` est explicite plutôt que déduit d'un `^` : un commit racine
    /// n'a pas de parent, et `<sha>^` y échoue au lieu de rendre le diff
    /// complet du premier commit.
    Commit { id: String, parent: Option<String> },
    /// Revue de branche : de la divergence d'avec `base` jusqu'à HEAD.
    ///
    /// Écrit `base...HEAD` (trois points) et non `base..HEAD` : le premier
    /// part du point de divergence, donc ne montre que ce que la branche a
    /// écrit, là où le second y mêlerait tout ce qui a atterri sur la base
    /// depuis — du bruit que le relecteur n'a pas à lire.
    Branch { base: String },
}

impl Range {
    fn args(&self) -> Vec<String> {
        match self {
            Self::Working => vec!["HEAD".into()],
            Self::Branch { base } => vec![format!("{base}...HEAD")],
            Self::Commit { id, parent } => match parent {
                Some(parent) => vec![parent.clone(), id.clone()],
                // L'arbre vide : le seul point de comparaison d'un commit sans
                // parent. Son empreinte est une constante de git, la même dans
                // tous les dépôts.
                None => vec![EMPTY_TREE.to_string(), id.clone()],
            },
        }
    }
}

/// Un fichier de la liste de revue, avec son volume de modifications.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct DiffFile {
    pub path: PathBuf,
    /// Ancien chemin d'un renommage.
    pub original: Option<PathBuf>,
    pub added: usize,
    pub removed: usize,
    /// git ne compte pas les lignes d'un binaire : rien à afficher côté texte.
    pub binary: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum DiffLineKind {
    Context,
    Added,
    Removed,
    /// « \ No newline at end of file » — à afficher, jamais à compter.
    NoNewline,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct DiffLine {
    pub kind: DiffLineKind,
    /// Numéros dans l'ancien et le nouveau fichier ; une ligne ajoutée n'a pas
    /// d'ancien numéro, une ligne supprimée pas de nouveau.
    pub old_no: Option<usize>,
    pub new_no: Option<usize>,
    pub text: String,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Hunk {
    /// L'en-tête `@@ … @@` tel quel, avec la section que git y ajoute.
    pub header: String,
    pub old_start: usize,
    pub new_start: usize,
    pub lines: Vec<DiffLine>,
}

/// Le diff d'un seul fichier.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct FileDiff {
    pub hunks: Vec<Hunk>,
    pub binary: bool,
    /// Diff tronqué par git (fichier très gros) ou vide.
    pub empty: bool,
}

/// Liste les fichiers du domaine de revue avec leur volume.
pub fn files(dir: &Path, range: &Range) -> Result<Vec<DiffFile>> {
    let mut args: Vec<String> = vec!["diff".into(), "--numstat".into(), "-z".into(), "-M".into()];
    args.extend(range.args());
    let out = git(dir, &args)?;
    Ok(parse_numstat(&out))
}

/// Diff d'un fichier unique.
///
/// `context` est le nombre de lignes autour de chaque modification ; la vue le
/// remonte quand on demande « plus de contexte ».
pub fn file(dir: &Path, range: &Range, path: &Path, context: usize) -> Result<FileDiff> {
    let mut args: Vec<String> = vec![
        "diff".into(),
        format!("-U{context}"),
        "-M".into(),
        // Sans cela, un `diff.external` ou un pilote de `.gitattributes`
        // remplace la sortie unifiée par un format que nous ne savons pas lire.
        "--no-ext-diff".into(),
        "--no-color".into(),
    ];
    args.extend(range.args());
    args.push("--".into());
    args.push(path.to_string_lossy().into_owned());
    let out = git(dir, &args)?;
    Ok(parse_unified(&out))
}

/// Le texte brut de ce qui est indexé, tel que git l'écrit.
///
/// Ni `DiffFile` ni `FileDiff` : ce diff-là n'est pas affiché, il est **lu par
/// un agent** à qui l'on demande un message de commit. Le format unifié est
/// justement ce qu'un modèle sait lire, et le redécouper pour le recomposer
/// ensuite ne ferait que perdre les en-têtes qui disent quel fichier change.
///
/// Le contexte est réduit à trois lignes : ce qu'on paie ici, c'est le nombre
/// de jetons envoyés. Un binaire modifié n'y met qu'une ligne — git ne
/// l'écrit pas sans `--text`, et c'est ce qu'on veut.
pub fn staged_text(dir: &Path) -> Result<String> {
    git(
        dir,
        &[
            "diff",
            "--cached",
            "-U3",
            "-M",
            "--no-ext-diff",
            "--no-color",
        ],
    )
}

/// Diff d'un fichier non suivi : git ne le connaît pas, donc `diff` seul rend
/// une sortie vide. `--no-index` contre `/dev/null` produit le même format que
/// pour les autres fichiers, ce qui évite un second chemin d'affichage.
pub fn untracked_file(dir: &Path, path: &Path) -> Result<FileDiff> {
    let full = dir.join(path);
    // `--no-index` sort avec le code 1 dès qu'il y a une différence, ce qui
    // est le cas normal ici : le fichier entier *est* la différence. Passer
    // par `git` jetterait la sortie avec l'« erreur », et le fichier
    // s'affichait vide — c'est ce qui faisait croire qu'un fichier nouveau
    // n'était pas lisible.
    let out = super::git_tolerant(
        dir,
        &[
            "diff",
            "--no-index",
            "--no-color",
            "--no-ext-diff",
            "/dev/null",
            &full.to_string_lossy(),
        ],
        1,
    )?;
    Ok(parse_unified(&out))
}

/// `--numstat -z` : `ajouts\tsuppressions\tchemin\0`, et pour un renommage
/// `ajouts\tsuppressions\t\0ancien\0nouveau\0`.
fn parse_numstat(out: &str) -> Vec<DiffFile> {
    let mut files = Vec::new();
    let mut records = split_nul(out);
    while let Some(rec) = records.next() {
        let mut f = rec.splitn(3, '\t');
        let added = f.next().unwrap_or("");
        let removed = f.next().unwrap_or("");
        let path = f.next().unwrap_or("");
        // git écrit « - » pour un binaire, dont les lignes n'ont pas de sens.
        let binary = added == "-" || removed == "-";
        let (path, original) = if path.is_empty() {
            // Renommage : le chemin est vide et suivi de deux enregistrements.
            let old = records.next().unwrap_or("");
            let new = records.next().unwrap_or("");
            (new.to_string(), Some(PathBuf::from(old)))
        } else {
            (path.to_string(), None)
        };
        if path.is_empty() {
            continue;
        }
        files.push(DiffFile {
            path: PathBuf::from(path),
            original,
            added: added.parse().unwrap_or(0),
            removed: removed.parse().unwrap_or(0),
            binary,
        });
    }
    files
}

fn parse_unified(out: &str) -> FileDiff {
    let mut diff = FileDiff {
        empty: true,
        ..Default::default()
    };
    let mut old_no = 0usize;
    let mut new_no = 0usize;

    for line in out.lines() {
        if line.starts_with("@@") {
            let (old_start, new_start) = parse_hunk_header(line);
            old_no = old_start;
            new_no = new_start;
            diff.hunks.push(Hunk {
                header: line.to_string(),
                old_start,
                new_start,
                lines: Vec::new(),
            });
            diff.empty = false;
            continue;
        }
        if diff.hunks.is_empty() {
            // Toujours dans l'en-tête (`diff --git`, `index`, `---`, `+++`).
            if line.starts_with("Binary files") || line.starts_with("GIT binary patch") {
                diff.binary = true;
                diff.empty = false;
            }
            continue;
        }
        let hunk = diff.hunks.last_mut().expect("un hunk est ouvert");
        let (kind, text) = match line.as_bytes().first() {
            Some(b'+') => (DiffLineKind::Added, &line[1..]),
            Some(b'-') => (DiffLineKind::Removed, &line[1..]),
            Some(b' ') => (DiffLineKind::Context, &line[1..]),
            Some(b'\\') => (DiffLineKind::NoNewline, line),
            // Une ligne vide dans un diff unifié est une ligne de contexte
            // dont git a élagué l'espace de tête.
            None => (DiffLineKind::Context, line),
            // `diff --git` d'un fichier suivant : on ne lit qu'un fichier.
            _ => break,
        };
        let (l_old, l_new) = match kind {
            DiffLineKind::Added => {
                let n = new_no;
                new_no += 1;
                (None, Some(n))
            }
            DiffLineKind::Removed => {
                let n = old_no;
                old_no += 1;
                (Some(n), None)
            }
            DiffLineKind::Context => {
                let (a, b) = (old_no, new_no);
                old_no += 1;
                new_no += 1;
                (Some(a), Some(b))
            }
            DiffLineKind::NoNewline => (None, None),
        };
        hunk.lines.push(DiffLine {
            kind,
            old_no: l_old,
            new_no: l_new,
            text: text.to_string(),
        });
    }
    diff
}

/// `@@ -12,7 +12,9 @@ fn quelque_chose()` → (12, 12).
fn parse_hunk_header(line: &str) -> (usize, usize) {
    let mut old = 1;
    let mut new = 1;
    for tok in line.split_whitespace() {
        let (target, body) = match tok.as_bytes().first() {
            Some(b'-') => (&mut old, &tok[1..]),
            Some(b'+') => (&mut new, &tok[1..]),
            _ => continue,
        };
        let start = body.split(',').next().unwrap_or("");
        if let Ok(n) = start.parse::<usize>() {
            *target = n;
        }
    }
    (old, new)
}

/// Reconstitue un patch applicable pour un seul hunk.
///
/// C'est ce que la vue envoie à `git apply --cached` pour indexer un morceau
/// isolé : git n'a pas de commande « ajoute ce hunk-là », seulement l'index
/// et un patch.
pub fn hunk_patch(path: &Path, original: Option<&Path>, hunk: &Hunk, reverse: bool) -> String {
    let new_path = path.to_string_lossy();
    let old_path = original
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_else(|| new_path.to_string());
    let mut patch =
        format!("diff --git a/{old_path} b/{new_path}\n--- a/{old_path}\n+++ b/{new_path}\n");
    let (old_count, new_count) = hunk.lines.iter().fold((0, 0), |(o, n), l| match l.kind {
        DiffLineKind::Added => (o, n + 1),
        DiffLineKind::Removed => (o + 1, n),
        DiffLineKind::Context => (o + 1, n + 1),
        DiffLineKind::NoNewline => (o, n),
    });
    patch.push_str(&format!(
        "@@ -{},{} +{},{} @@\n",
        hunk.old_start, old_count, hunk.new_start, new_count
    ));
    for line in &hunk.lines {
        match line.kind {
            DiffLineKind::Added => patch.push('+'),
            DiffLineKind::Removed => patch.push('-'),
            DiffLineKind::Context => patch.push(' '),
            DiffLineKind::NoNewline => {}
        }
        patch.push_str(&line.text);
        patch.push('\n');
    }
    // `reverse` n'inverse pas le texte : `git apply --reverse` s'en charge, et
    // il le fait juste, y compris pour les fins de fichier sans saut de ligne.
    let _ = reverse;
    patch
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Le fichier nouveau se lit entièrement.
    ///
    /// `--no-index` sort avec le code 1 dès qu'il trouve une différence, et
    /// c'est le cas normal ici : le fichier entier *est* la différence. Une
    /// lecture qui traite ce code comme un échec rend un diff vide, et la vue
    /// affiche « aucune modification » sur un fichier qui n'est que des
    /// ajouts.
    #[test]
    fn an_untracked_file_is_read_whole() {
        let dir = tempdir();
        std::process::Command::new("git")
            .args(["init", "-q", "."])
            .current_dir(&dir)
            .status()
            .expect("git init");
        std::fs::write(dir.join("nouveau.txt"), "une\ndeux\n").unwrap();

        let diff = untracked_file(&dir, Path::new("nouveau.txt")).expect("lecture");
        let lines: Vec<&str> = diff
            .hunks
            .iter()
            .flat_map(|hunk| hunk.lines.iter())
            .map(|line| line.text.as_str())
            .collect();
        assert_eq!(lines, vec!["une", "deux"]);
        assert!(diff
            .hunks
            .iter()
            .flat_map(|h| h.lines.iter())
            .all(|l| l.kind == DiffLineKind::Added));

        let _ = std::fs::remove_dir_all(&dir);
    }

    fn tempdir() -> PathBuf {
        let dir = std::env::temp_dir().join(format!("claudhub-diff-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("répertoire de test");
        dir
    }

    #[test]
    fn reads_numstat_with_a_rename_and_a_binary() {
        let out = "3\t1\tsrc/main.rs\0\
                   12\t0\t\0assets/vieux nom.svg\0assets/nouveau nom.svg\0\
                   -\t-\tassets/logo.png\0";
        let files = parse_numstat(out);
        assert_eq!(files.len(), 3);
        assert_eq!(files[0].path, PathBuf::from("src/main.rs"));
        assert_eq!((files[0].added, files[0].removed), (3, 1));

        assert_eq!(files[1].path, PathBuf::from("assets/nouveau nom.svg"));
        assert_eq!(
            files[1].original,
            Some(PathBuf::from("assets/vieux nom.svg"))
        );

        assert!(files[2].binary, "git écrit « - » pour un binaire");
        assert_eq!((files[2].added, files[2].removed), (0, 0));
    }

    #[test]
    fn numbers_lines_on_both_sides() {
        let out = "\
diff --git a/src/lib.rs b/src/lib.rs
index 1234567..89abcde 100644
--- a/src/lib.rs
+++ b/src/lib.rs
@@ -10,4 +10,5 @@ impl Repo {
 fn inchangee() {
-    ancienne();
+    nouvelle();
+    ajoutee();
 }
";
        let d = parse_unified(out);
        assert!(!d.empty && !d.binary);
        assert_eq!(d.hunks.len(), 1);
        let h = &d.hunks[0];
        assert_eq!((h.old_start, h.new_start), (10, 10));

        let l = &h.lines;
        assert_eq!(l[0].kind, DiffLineKind::Context);
        assert_eq!((l[0].old_no, l[0].new_no), (Some(10), Some(10)));
        // Une suppression avance le compteur de l'ancien fichier seulement.
        assert_eq!(l[1].kind, DiffLineKind::Removed);
        assert_eq!((l[1].old_no, l[1].new_no), (Some(11), None));
        assert_eq!(l[2].kind, DiffLineKind::Added);
        assert_eq!((l[2].old_no, l[2].new_no), (None, Some(11)));
        assert_eq!(l[3].new_no, Some(12));
        // La ligne de contexte finale reprend après les deux côtés.
        assert_eq!((l[4].old_no, l[4].new_no), (Some(12), Some(13)));
        assert_eq!(l[1].text, "    ancienne();");
    }

    #[test]
    fn detects_a_binary_file() {
        let out =
            "diff --git a/logo.png b/logo.png\nBinary files a/logo.png and b/logo.png differ\n";
        let d = parse_unified(out);
        assert!(d.binary);
        assert!(d.hunks.is_empty());
    }

    #[test]
    fn an_empty_diff_stays_empty() {
        assert!(parse_unified("").empty);
    }

    #[test]
    fn rebuilds_an_applicable_patch() {
        let out = "\
@@ -5,3 +5,4 @@
 contexte
-vieux
+neuf
+encore
";
        let d = parse_unified(out);
        let patch = hunk_patch(Path::new("src/x.rs"), None, &d.hunks[0], false);
        assert!(patch.starts_with("diff --git a/src/x.rs b/src/x.rs\n"));
        // Les comptes sont recalculés depuis les lignes retenues, pas copiés
        // de l'en-tête d'origine : un hunk isolé peut être plus court.
        assert!(patch.contains("@@ -5,2 +5,3 @@\n"), "patch = {patch}");
        assert!(patch.ends_with(" contexte\n-vieux\n+neuf\n+encore\n"));
    }

    #[test]
    fn a_commit_compares_against_its_parent_or_the_empty_tree() {
        let with_parent = Range::Commit {
            id: "abc".into(),
            parent: Some("def".into()),
        };
        assert_eq!(with_parent.args(), vec!["def", "abc"]);

        // Un commit racine se compare à l'arbre vide : `abc^` n'existe pas.
        let root = Range::Commit {
            id: "abc".into(),
            parent: None,
        };
        assert_eq!(root.args(), vec![EMPTY_TREE, "abc"]);
    }

    #[test]
    fn header_defaults_to_one_when_unparsable() {
        assert_eq!(parse_hunk_header("@@ -1 +1 @@"), (1, 1));
        assert_eq!(parse_hunk_header("@@ -0,0 +1,5 @@"), (0, 1));
    }
}
