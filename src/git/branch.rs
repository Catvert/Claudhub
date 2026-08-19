//! Branches : la liste du sélecteur, et les questions fermées que posent les
//! opérations de worktree.

use std::path::{Path, PathBuf};

use anyhow::Result;

use super::{git, git_ok, git_opt};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BranchKind {
    Local,
    /// Branche de suivi à distance (`origin/…`) sans équivalent local.
    Remote,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Upstream {
    pub name: String,
    pub ahead: usize,
    pub behind: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Branch {
    pub name: String,
    pub kind: BranchKind,
    /// Vrai pour la branche de HEAD dans le checkout interrogé.
    pub is_head: bool,
    /// Date du dernier commit, en relatif (« il y a 3 jours »), telle que git
    /// la formule — nous n'avons pas à la recalculer.
    pub date: String,
    pub subject: String,
    /// Auteur du dernier commit. Dans un dépôt d'équipe, c'est ce qui
    /// distingue deux branches au nom voisin plus sûrement que leur date.
    pub author: String,
    pub upstream: Option<Upstream>,
    /// Worktree qui a déjà cette branche déployée. Git refuse deux checkouts
    /// de la même branche : le dire avant d'essayer vaut mieux qu'une erreur.
    pub checked_out_at: Option<PathBuf>,
}

/// Liste les branches, locales d'abord puis les distantes sans jumelle
/// locale, du commit le plus récent au plus ancien.
pub fn list(main: &Path) -> Result<Vec<Branch>> {
    // Le séparateur doit être un caractère qu'un sujet de commit ne contient
    // pas ; `%00` est écrit littéralement par for-each-ref comme un octet nul.
    // L'auteur est en dernier : ajouter un champ à la fin garde les sorties
    // écrites par une version antérieure lisibles, un champ absent valant la
    // chaîne vide.
    const FORMAT: &str = "%(refname:short)%00%(HEAD)%00%(committerdate:relative)%00\
                          %(contents:subject)%00%(upstream:short)%00%(upstream:track)%00\
                          %(authorname)";

    let raw = git(
        main,
        &[
            "for-each-ref",
            "--sort=-committerdate",
            &format!("--format={FORMAT}"),
            "refs/heads",
            "refs/remotes",
        ],
    )?;

    let locals: Vec<String> = git_opt(
        main,
        &["for-each-ref", "--format=%(refname:short)", "refs/heads"],
    )
    .unwrap_or_default()
    .lines()
    .map(str::to_string)
    .collect();

    let mut branches: Vec<Branch> = raw
        .lines()
        .filter_map(|line| parse_ref(line, &locals))
        .collect();
    // for-each-ref trie par date sur l'ensemble ; on veut les locales d'abord,
    // chaque groupe restant trié par date (le tri stable de Rust le garantit).
    branches.sort_by_key(|b| match b.kind {
        BranchKind::Local => 0,
        BranchKind::Remote => 1,
    });
    for b in &mut branches {
        if b.kind == BranchKind::Local {
            b.checked_out_at = checked_out_at(main, &b.name);
        }
    }
    Ok(branches)
}

fn parse_ref(line: &str, locals: &[String]) -> Option<Branch> {
    let mut f = line.split('\0');
    let name = f.next()?.to_string();
    let head = f.next().unwrap_or("").trim() == "*";
    let date = f.next().unwrap_or("").to_string();
    let subject = f.next().unwrap_or("").to_string();
    let upstream_name = f.next().unwrap_or("");
    let track = f.next().unwrap_or("");
    let author = f.next().unwrap_or("").to_string();

    let kind = if name.contains('/') && !locals.iter().any(|l| l == &name) {
        BranchKind::Remote
    } else {
        BranchKind::Local
    };

    if kind == BranchKind::Remote {
        // `refs/remotes/origin/HEAD` est un alias, pas une branche.
        if name.ends_with("/HEAD") {
            return None;
        }
        // Une distante déjà présente localement n'apporte rien au sélecteur.
        if let Some((_, short)) = name.split_once('/') {
            if locals.iter().any(|l| l == short) {
                return None;
            }
        }
    }

    Some(Branch {
        name,
        kind,
        is_head: head,
        date,
        subject,
        author,
        upstream: (!upstream_name.is_empty()).then(|| {
            let (ahead, behind) = parse_track(track);
            Upstream {
                name: upstream_name.to_string(),
                ahead,
                behind,
            }
        }),
        checked_out_at: None,
    })
}

/// `%(upstream:track)` vaut « [ahead 2, behind 3] », « [gone] » ou rien.
fn parse_track(track: &str) -> (usize, usize) {
    let inner = track.trim().trim_start_matches('[').trim_end_matches(']');
    let mut ahead = 0;
    let mut behind = 0;
    for part in inner.split(',') {
        let part = part.trim();
        if let Some(n) = part.strip_prefix("ahead ") {
            ahead = n.trim().parse().unwrap_or(0);
        } else if let Some(n) = part.strip_prefix("behind ") {
            behind = n.trim().parse().unwrap_or(0);
        }
    }
    (ahead, behind)
}

pub fn current(dir: &Path) -> Option<String> {
    let name = git_opt(dir, &["symbolic-ref", "--short", "-q", "HEAD"])?;
    (!name.is_empty()).then_some(name)
}

pub fn local_exists(main: &Path, branch: &str) -> bool {
    git_ok(
        main,
        &[
            "show-ref",
            "--verify",
            "--quiet",
            &format!("refs/heads/{branch}"),
        ],
    )
}

/// Worktree qui détient déjà cette branche, s'il y en a un.
pub fn checked_out_at(main: &Path, branch: &str) -> Option<PathBuf> {
    let out = git_opt(main, &["worktree", "list", "--porcelain"])?;
    let mut current: Option<&str> = None;
    let target = format!("refs/heads/{branch}");
    for line in out.lines() {
        if let Some(p) = line.strip_prefix("worktree ") {
            current = Some(p);
        } else if line.strip_prefix("branch ") == Some(target.as_str()) {
            return current.map(PathBuf::from);
        }
    }
    None
}

/// Point de divergence entre `branch` et sa base : c'est ce commit-là que la
/// vue « diff de branche » compare à HEAD, et non la pointe de la base — sans
/// quoi le diff inclut tout ce qui a atterri sur la base entre-temps, que
/// l'auteur de la branche n'a ni écrit ni à relire.
pub fn merge_base(dir: &Path, a: &str, b: &str) -> Option<String> {
    git_opt(dir, &["merge-base", a, b]).filter(|s| !s.is_empty())
}

/// Devine la branche d'intégration du dépôt.
///
/// L'ordre suit ce qui fait autorité : ce que le dépôt distant déclare comme
/// sa branche par défaut, puis la configuration locale, puis les deux noms
/// usuels — et seulement s'ils existent.
pub fn default_base(main: &Path) -> Option<String> {
    if let Some(head) = git_opt(
        main,
        &["symbolic-ref", "--short", "-q", "refs/remotes/origin/HEAD"],
    ) {
        if let Some((_, short)) = head.split_once('/') {
            return Some(short.to_string());
        }
    }
    if let Some(name) = git_opt(main, &["config", "--get", "init.defaultBranch"]) {
        if local_exists(main, &name) {
            return Some(name);
        }
    }
    ["main", "master", "develop"]
        .into_iter()
        .find(|b| local_exists(main, b))
        .map(str::to_string)
}

/// Rattache une branche sans amont à `origin/<branche>` pour que le premier
/// `git push` n'ait pas besoin de `-u`. La référence distante n'existe pas
/// encore : c'est justement ce que le push créera.
pub(crate) fn ensure_upstream(main: &Path, branch: &str) {
    let merge_key = format!("branch.{branch}.merge");
    if git_opt(main, &["config", "--get", &merge_key]).is_some() {
        return;
    }
    if git_opt(main, &["remote", "get-url", "origin"]).is_none() {
        return;
    }
    let _ = git(
        main,
        &["config", &format!("branch.{branch}.remote"), "origin"],
    );
    let _ = git(
        main,
        &["config", &merge_key, &format!("refs/heads/{branch}")],
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_a_local_branch_with_its_upstream() {
        let locals = vec!["main".to_string()];
        let line =
            "main\0*\0il y a 2 heures\0Corrige le rendu\0origin/main\0[ahead 1, behind 4]\0Zoé";
        let b = parse_ref(line, &locals).unwrap();
        assert_eq!(b.name, "main");
        assert_eq!(b.kind, BranchKind::Local);
        assert!(b.is_head);
        assert_eq!(b.subject, "Corrige le rendu");
        assert_eq!(b.author, "Zoé");
        let up = b.upstream.unwrap();
        assert_eq!(up.name, "origin/main");
        assert_eq!((up.ahead, up.behind), (1, 4));
    }

    #[test]
    fn a_branch_without_upstream_has_none() {
        let line = "wt/essai\0 \0hier\0Brouillon\0\0";
        let b = parse_ref(line, &["wt/essai".to_string()]).unwrap();
        assert_eq!(b.kind, BranchKind::Local, "une locale peut contenir un /");
        assert!(!b.is_head);
        assert_eq!(b.upstream, None);
        // Un champ absent — une sortie d'avant l'ajout de l'auteur — ne fait
        // pas échouer la lecture.
        assert_eq!(b.author, "");
    }

    #[test]
    fn hides_remote_duplicates_and_head_alias() {
        let locals = vec!["main".to_string()];
        assert!(parse_ref("origin/main\0 \0hier\0x\0\0", &locals).is_none());
        assert!(parse_ref("origin/HEAD\0 \0hier\0x\0\0", &locals).is_none());
        let b = parse_ref("origin/feature\0 \0hier\0x\0\0", &locals).unwrap();
        assert_eq!(b.kind, BranchKind::Remote);
    }

    #[test]
    fn parses_track_variants() {
        assert_eq!(parse_track("[ahead 3]"), (3, 0));
        assert_eq!(parse_track("[behind 2]"), (0, 2));
        assert_eq!(parse_track("[ahead 1, behind 2]"), (1, 2));
        // « [gone] » : l'amont a disparu, ni avance ni retard mesurables.
        assert_eq!(parse_track("[gone]"), (0, 0));
        assert_eq!(parse_track(""), (0, 0));
    }
}
