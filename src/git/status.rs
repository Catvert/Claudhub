//! État du répertoire de travail : ce que le panneau « fichiers modifiés »
//! affiche, et ce sur quoi il déclenche l'indexation.
//!
//! La source est `git status --porcelain=v2 -z --branch`. La v2 est la seule
//! qui distingue nettement l'état de l'index de celui du répertoire de travail
//! (un fichier peut être ajouté *et* remodifié), donne le score des renommages
//! et sépare les chemins par des octets nuls — nécessaire dès qu'un fichier
//! contient un espace, un guillemet ou un saut de ligne.

use std::path::{Path, PathBuf};

use anyhow::Result;

use super::{git, split_nul};

/// Le code d'un côté (index ou répertoire de travail) pour un fichier.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StatusCode {
    Unmodified,
    Modified,
    Added,
    Deleted,
    Renamed,
    Copied,
    TypeChanged,
    Untracked,
    Ignored,
    /// Fusion en conflit : les deux côtés portent ce code.
    Unmerged,
}

impl StatusCode {
    fn from_char(c: char) -> Self {
        match c {
            'M' => Self::Modified,
            'A' => Self::Added,
            'D' => Self::Deleted,
            'R' => Self::Renamed,
            'C' => Self::Copied,
            'T' => Self::TypeChanged,
            '?' => Self::Untracked,
            '!' => Self::Ignored,
            'U' => Self::Unmerged,
            _ => Self::Unmodified,
        }
    }

    /// Lettre affichée dans la liste, à gauche du chemin.
    pub fn letter(self) -> &'static str {
        match self {
            Self::Unmodified => " ",
            Self::Modified => "M",
            Self::Added => "A",
            Self::Deleted => "D",
            Self::Renamed => "R",
            Self::Copied => "C",
            Self::TypeChanged => "T",
            Self::Untracked => "?",
            Self::Ignored => "!",
            Self::Unmerged => "U",
        }
    }
}

/// Un fichier tel qu'il apparaît dans le panneau de revue.
///
/// `index` et `worktree` sont indépendants : un fichier indexé puis remodifié
/// est `index: Modified, worktree: Modified` et apparaît des deux côtés de la
/// liste. C'est précisément ce que la v1 rendait pénible à lire.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileStatus {
    pub path: PathBuf,
    /// Ancien chemin d'un fichier renommé ou copié.
    pub original: Option<PathBuf>,
    pub index: StatusCode,
    pub worktree: StatusCode,
}

impl FileStatus {
    /// A quelque chose à valider (une partie au moins est dans l'index).
    pub fn is_staged(&self) -> bool {
        !matches!(self.index, StatusCode::Unmodified | StatusCode::Untracked)
    }

    /// A des modifications hors index.
    pub fn is_unstaged(&self) -> bool {
        !matches!(self.worktree, StatusCode::Unmodified)
    }

    pub fn is_untracked(&self) -> bool {
        self.index == StatusCode::Untracked || self.worktree == StatusCode::Untracked
    }

    pub fn is_conflicted(&self) -> bool {
        self.index == StatusCode::Unmerged || self.worktree == StatusCode::Unmerged
    }

    /// Nom de fichier seul, pour la colonne de gauche ; le dossier est affiché
    /// à côté, en atténué.
    pub fn file_name(&self) -> String {
        self.path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| self.path.display().to_string())
    }

    pub fn directory(&self) -> String {
        self.path
            .parent()
            .filter(|p| !p.as_os_str().is_empty())
            .map(|p| p.display().to_string())
            .unwrap_or_default()
    }
}

/// L'état complet d'un checkout à un instant donné.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Status {
    /// Branche courante, `None` en HEAD détachée.
    pub branch: Option<String>,
    pub upstream: Option<String>,
    /// Commits d'avance et de retard sur l'amont, quand il y en a un.
    pub ahead: usize,
    pub behind: usize,
    pub files: Vec<FileStatus>,
}

impl Status {
    pub fn staged(&self) -> impl Iterator<Item = &FileStatus> {
        self.files.iter().filter(|f| f.is_staged())
    }

    pub fn unstaged(&self) -> impl Iterator<Item = &FileStatus> {
        self.files.iter().filter(|f| f.is_unstaged())
    }

    pub fn conflicted(&self) -> impl Iterator<Item = &FileStatus> {
        self.files.iter().filter(|f| f.is_conflicted())
    }

    pub fn is_clean(&self) -> bool {
        self.files.is_empty()
    }
}

/// Lit l'état de `dir`.
///
/// Les fichiers ignorés ne sont pas demandés : une liste de revue noyée sous
/// `target/` ou `node_modules/` n'a aucune valeur, et les énumérer coûte le
/// parcours complet des dossiers exclus.
pub fn status(dir: &Path) -> Result<Status> {
    let out = git(
        dir,
        &[
            "status",
            "--porcelain=v2",
            "--branch",
            "-z",
            // `all` et non `normal` : sans cela, un dossier entièrement
            // nouveau apparaît comme une seule entrée `dossier/` qu'on ne peut
            // ni lire ni indexer fichier par fichier — et un worktree d'agent
            // en crée. Le coût est un parcours complet des dossiers non
            // versionnés *et non ignorés*, ce que `.gitignore` borne déjà.
            "--untracked-files=all",
        ],
    )?;
    Ok(parse(&out))
}

fn parse(out: &str) -> Status {
    let mut status = Status::default();
    let mut records = split_nul(out);

    while let Some(rec) = records.next() {
        let mut chars = rec.chars();
        match chars.next() {
            Some('#') => parse_header(rec, &mut status),
            Some('1') => {
                if let Some(f) = parse_ordinary(rec) {
                    status.files.push(f);
                }
            }
            Some('2') => {
                // Un renommage occupe deux enregistrements : l'entrée, puis
                // l'ancien chemin. Consommer le second ici est ce qui garde
                // l'itérateur aligné pour la suite.
                let original = records.next().map(PathBuf::from);
                if let Some(mut f) = parse_ordinary(rec) {
                    f.original = original;
                    status.files.push(f);
                }
            }
            Some('u') => {
                if let Some(f) = parse_unmerged(rec) {
                    status.files.push(f);
                }
            }
            Some('?') => status.files.push(FileStatus {
                path: PathBuf::from(&rec[2..]),
                original: None,
                index: StatusCode::Untracked,
                worktree: StatusCode::Untracked,
            }),
            Some('!') => status.files.push(FileStatus {
                path: PathBuf::from(&rec[2..]),
                original: None,
                index: StatusCode::Ignored,
                worktree: StatusCode::Ignored,
            }),
            _ => {}
        }
    }
    status
}

fn parse_header(rec: &str, status: &mut Status) {
    let rest = rec.trim_start_matches("# ");
    if let Some(head) = rest.strip_prefix("branch.head ") {
        // git écrit littéralement "(detached)" quand il n'y a pas de branche.
        status.branch = (head != "(detached)").then(|| head.to_string());
    } else if let Some(up) = rest.strip_prefix("branch.upstream ") {
        status.upstream = Some(up.to_string());
    } else if let Some(ab) = rest.strip_prefix("branch.ab ") {
        // Format « +2 -3 ».
        for part in ab.split_whitespace() {
            let n: usize = part[1..].parse().unwrap_or(0);
            match part.as_bytes().first() {
                Some(b'+') => status.ahead = n,
                Some(b'-') => status.behind = n,
                _ => {}
            }
        }
    }
}

/// `1 <XY> <sub> <mH> <mI> <mW> <hH> <hI> <path>` et la variante `2` du
/// renommage, dont seuls les deux premiers champs nous intéressent.
fn parse_ordinary(rec: &str) -> Option<FileStatus> {
    let mut fields = rec.splitn(9, ' ');
    fields.next()?; // '1' ou '2'
    let xy = fields.next()?;
    let mut xy = xy.chars();
    let index = StatusCode::from_char(xy.next()?);
    let worktree = StatusCode::from_char(xy.next()?);
    // sub, mH, mI, mW, hH, hI — sans intérêt pour l'affichage.
    for _ in 0..6 {
        fields.next()?;
    }
    let rest = fields.next()?;
    // Pour un renommage, le champ de score (`R100`) précède le chemin.
    let path = if index == StatusCode::Renamed
        || index == StatusCode::Copied
        || worktree == StatusCode::Renamed
        || worktree == StatusCode::Copied
    {
        rest.split_once(' ').map(|(_, p)| p)?
    } else {
        rest
    };
    Some(FileStatus {
        path: PathBuf::from(path),
        original: None,
        index,
        worktree,
    })
}

/// `u <XY> <sub> <m1> <m2> <m3> <mW> <h1> <h2> <h3> <path>`
fn parse_unmerged(rec: &str) -> Option<FileStatus> {
    let mut fields = rec.splitn(11, ' ');
    fields.next()?; // 'u'
    let xy = fields.next()?;
    let mut xy = xy.chars();
    let index = StatusCode::from_char(xy.next()?);
    let worktree = StatusCode::from_char(xy.next()?);
    for _ in 0..8 {
        fields.next()?;
    }
    Some(FileStatus {
        path: PathBuf::from(fields.next()?),
        original: None,
        index,
        worktree,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rec(parts: &[&str]) -> String {
        let mut s = String::new();
        for p in parts {
            s.push_str(p);
            s.push('\0');
        }
        s
    }

    #[test]
    fn reads_branch_and_divergence() {
        let out = rec(&[
            "# branch.oid abc123",
            "# branch.head feature/x",
            "# branch.upstream origin/feature/x",
            "# branch.ab +2 -3",
        ]);
        let st = parse(&out);
        assert_eq!(st.branch.as_deref(), Some("feature/x"));
        assert_eq!(st.upstream.as_deref(), Some("origin/feature/x"));
        assert_eq!((st.ahead, st.behind), (2, 3));
        assert!(st.is_clean());
    }

    #[test]
    fn detached_head_has_no_branch() {
        let st = parse(&rec(&["# branch.head (detached)"]));
        assert_eq!(st.branch, None);
    }

    #[test]
    fn separates_index_from_worktree() {
        // Fichier indexé puis remodifié : les deux côtés doivent apparaître.
        let out = rec(&["1 MM N... 100644 100644 100644 aaa bbb src/main.rs"]);
        let st = parse(&out);
        let f = &st.files[0];
        assert_eq!(f.path, PathBuf::from("src/main.rs"));
        assert_eq!(f.index, StatusCode::Modified);
        assert_eq!(f.worktree, StatusCode::Modified);
        assert!(f.is_staged() && f.is_unstaged());
    }

    #[test]
    fn reads_a_rename_and_its_original_path() {
        // L'ancien chemin est un enregistrement à part en mode -z.
        let out = rec(&[
            "2 R. N... 100644 100644 100644 aaa bbb R100 ui/new name.rs",
            "ui/old name.rs",
            "1 .M N... 100644 100644 100644 ccc ddd src/lib.rs",
        ]);
        let st = parse(&out);
        assert_eq!(
            st.files.len(),
            2,
            "l'ancien chemin ne doit pas devenir une entrée"
        );
        assert_eq!(st.files[0].path, PathBuf::from("ui/new name.rs"));
        assert_eq!(st.files[0].original, Some(PathBuf::from("ui/old name.rs")));
        assert_eq!(st.files[0].index, StatusCode::Renamed);
        // L'entrée suivante est bien repartie du bon enregistrement.
        assert_eq!(st.files[1].path, PathBuf::from("src/lib.rs"));
    }

    #[test]
    fn reads_untracked_and_conflicts() {
        let out = rec(&[
            "? nouveau fichier.txt",
            "u UU N... 100644 100644 100644 100644 aaa bbb ccc src/conflit.rs",
        ]);
        let st = parse(&out);
        assert!(st.files[0].is_untracked());
        assert_eq!(st.files[0].path, PathBuf::from("nouveau fichier.txt"));
        assert!(st.files[1].is_conflicted());
        assert_eq!(st.files[1].path, PathBuf::from("src/conflit.rs"));
        assert_eq!(st.conflicted().count(), 1);
    }
}

/// Le résumé d'un checkout : de quoi le décrire d'une ligne dans la barre
/// latérale, sans l'ouvrir.
///
/// Deux commandes plutôt qu'une parce que git n'en a aucune qui donne les
/// deux : `--numstat` compte les lignes mais ignore ce qu'il ne suit pas, et
/// `status` voit les fichiers nouveaux sans savoir ce qu'ils contiennent. Un
/// worktree d'agent est justement plein de fichiers nouveaux.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Summary {
    /// Fichiers touchés, les nouveaux compris.
    pub files: usize,
    pub added: usize,
    pub removed: usize,
}

impl Summary {
    pub fn is_empty(&self) -> bool {
        self.files == 0
    }
}

/// Au-delà de cette taille, un fichier nouveau n'est pas lu.
///
/// Ce résumé tourne en boucle sur tous les worktrees ouverts : un export SQL
/// ou une archive oubliée dans un coin ne doit pas se relire toutes les dix
/// secondes. Le fichier compte quand même comme fichier touché.
const MAX_UNTRACKED_READ: u64 = 1 << 20;

/// Compte les lignes des fichiers nouveaux, que `--numstat` laisse de côté.
///
/// Un fichier binaire n'a pas de lignes ; il compte quand même comme fichier
/// touché, ce que `files` porte déjà.
fn untracked_lines(dir: &std::path::Path, status: &Status) -> usize {
    status
        .files
        .iter()
        .filter(|file| file.is_untracked())
        .map(|file| dir.join(&file.path))
        .filter(|path| std::fs::metadata(path).is_ok_and(|meta| meta.len() <= MAX_UNTRACKED_READ))
        .filter_map(|path| std::fs::read(path).ok())
        .filter(|bytes| !bytes.contains(&0))
        .map(|bytes| bytes.iter().filter(|b| **b == b'\n').count())
        .sum()
}

pub fn summary(dir: &std::path::Path) -> Result<Summary> {
    let status = status(dir)?;
    let files = status
        .files
        .iter()
        .filter(|file| !matches!(file.index, StatusCode::Ignored))
        .count();
    let changed = super::diff::files(dir, &super::DiffRange::Working)?;
    Ok(Summary {
        files,
        added: changed.iter().map(|f| f.added).sum::<usize>() + untracked_lines(dir, &status),
        removed: changed.iter().map(|f| f.removed).sum(),
    })
}
