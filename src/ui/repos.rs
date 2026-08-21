//! The open repositories, and everything one asks of that list.
//!
//! Free of gpui, and deliberately so: "which repository does this worktree
//! belong to", "what does `Ctrl+3` name", "is this base worth offering" are
//! decisions, not rendering, and they were only reachable through an entity
//! nothing can build outside a window. It is the same split as
//! `notes.rs` / `notes_view.rs`.

use std::path::{Path, PathBuf};

use crate::git::{Branch, Worktree};

/// A repository open in the sidebar.
pub struct RepoState {
    pub main: PathBuf,
    pub name: String,
    pub worktrees: Vec<Worktree>,
    pub branches: Vec<Branch>,
    /// The integration branch, as git declares it. It is only known once the
    /// worker has answered: until then the branch review has no base and its tab
    /// stays inactive — offering an assumed `main` would produce an "unknown
    /// revision" on any repository not called that.
    pub default_base: Option<String>,
    pub collapsed: bool,
}

/// A remembered repository we could not open.
///
/// It lives apart from the `RepoState`s and not among them with a flag:
/// everything that walks the open list — the agent sweep, the summaries, the
/// `wt` reading, the automatic fetch — assumes a repository that exists, and the
/// opposite would be paid for in guards scattered everywhere, one forgotten
/// among them running git commands in a missing folder every two seconds.
pub struct UnavailableRepo {
    pub path: PathBuf,
    /// What git answered, for the tooltip. The row itself only says "not found":
    /// that is what one needs to know to decide to remove it.
    pub message: String,
}

/// What the window knows of the repositories: those that opened, and those that
/// did not.
#[derive(Default)]
pub struct Repos {
    open: Vec<RepoState>,
    missing: Vec<UnavailableRepo>,
}

impl Repos {
    pub fn iter(&self) -> impl Iterator<Item = &RepoState> {
        self.open.iter()
    }

    pub fn iter_mut(&mut self) -> impl Iterator<Item = &mut RepoState> {
        self.open.iter_mut()
    }

    /// The repositories that could not be opened. They stay on screen, in
    /// error: a repository that appears nowhere cannot be removed either.
    pub fn missing(&self) -> &[UnavailableRepo] {
        &self.missing
    }

    pub fn is_empty(&self) -> bool {
        self.open.is_empty()
    }

    /// The main repositories, which is what the background sweep works from.
    pub fn mains(&self) -> Vec<PathBuf> {
        self.open.iter().map(|repo| repo.main.clone()).collect()
    }

    pub fn get_mut(&mut self, main: &Path) -> Option<&mut RepoState> {
        self.open.iter_mut().find(|repo| repo.main == main)
    }

    /// Opens a repository, and says whether there was anything to open.
    ///
    /// `false` when it is already there: reopening a repository — from the
    /// picker, or because the server was relaunched and the window reposted
    /// everything — must not duplicate its row. A folder that comes back
    /// (remounted, recloned) stops being missing.
    pub fn open(&mut self, main: PathBuf, name: String, worktrees: Vec<Worktree>) -> bool {
        if self.open.iter().any(|repo| repo.main == main) {
            return false;
        }
        self.missing.retain(|repo| repo.path != main);
        self.open.push(RepoState {
            main,
            name,
            worktrees,
            branches: Vec::new(),
            default_base: None,
            collapsed: false,
        });
        true
    }

    /// Closes a repository and returns the worktrees that went with it — what
    /// the window has to let go of afterwards.
    ///
    /// Both lists, because a row of either kind is removed by the same gesture:
    /// a repository that does not open is closed from the same button.
    pub fn close(&mut self, main: &Path) -> Vec<PathBuf> {
        let closed = self.worktree_paths(main);
        self.open.retain(|repo| repo.main != main);
        self.missing.retain(|repo| repo.path != main);
        closed
    }

    /// Folds or unfolds the repository at that row.
    ///
    /// By index and not by path: the sidebar's rows carry their index, and it
    /// is the same list in the same order.
    pub fn toggle_collapse(&mut self, ix: usize) {
        if let Some(repo) = self.open.get_mut(ix) {
            repo.collapsed = !repo.collapsed;
        }
    }

    /// Records a repository that would not open, and says whether it is new.
    pub fn mark_missing(&mut self, path: PathBuf, message: String) -> bool {
        if self.missing.iter().any(|repo| repo.path == path) {
            return false;
        }
        self.missing.push(UnavailableRepo { path, message });
        true
    }

    pub fn set_worktrees(&mut self, main: &Path, worktrees: Vec<Worktree>) {
        if let Some(repo) = self.get_mut(main) {
            repo.worktrees = worktrees;
        }
    }

    /// The paths of a repository's worktrees, as git has just enumerated them.
    pub fn worktree_paths(&self, main: &Path) -> Vec<PathBuf> {
        self.open
            .iter()
            .find(|repo| repo.main == main)
            .map(|repo| repo.worktrees.iter().map(|w| w.path.clone()).collect())
            .unwrap_or_default()
    }

    pub fn repo_of(&self, worktree: &Path) -> Option<&RepoState> {
        self.open
            .iter()
            .find(|repo| repo.worktrees.iter().any(|w| w.path == worktree))
    }

    pub fn main_of(&self, worktree: &Path) -> Option<PathBuf> {
        self.repo_of(worktree).map(|repo| repo.main.clone())
    }

    pub fn worktree(&self, path: &Path) -> Option<&Worktree> {
        self.open
            .iter()
            .flat_map(|repo| repo.worktrees.iter())
            .find(|w| w.path == path)
    }

    pub fn contains_worktree(&self, path: &Path) -> bool {
        self.worktree(path).is_some()
    }

    pub fn first_worktree(&self) -> Option<PathBuf> {
        self.open
            .iter()
            .flat_map(|repo| repo.worktrees.iter())
            .next()
            .map(|w| w.path.clone())
    }

    /// The worktrees in the order the sidebar shows them, which is what
    /// `Ctrl+1` to `Ctrl+9` name.
    ///
    /// Collapses change nothing here: `Ctrl+3` has to name the same worktree
    /// whether its repository is collapsed or not, otherwise the shortcut would
    /// only be memorable in one state of the list.
    pub fn worktrees_in_order(&self) -> Vec<PathBuf> {
        self.open
            .iter()
            .flat_map(|repo| repo.worktrees.iter().map(|w| w.path.clone()))
            .collect()
    }

    /// A worktree's comparison base: the repository's integration branch, except
    /// when that is precisely the one checked out there — comparing a branch
    /// against itself shows nothing.
    pub fn default_base_for(&self, worktree: &Path) -> Option<String> {
        let repo = self.repo_of(worktree)?;
        let base = repo.default_base.as_deref()?;
        let current = repo
            .worktrees
            .iter()
            .find(|w| w.path == worktree)
            .and_then(|w| w.branch.as_deref());
        (Some(base) != current).then(|| base.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn worktree(path: &str, branch: Option<&str>) -> Worktree {
        Worktree {
            path: PathBuf::from(path),
            branch: branch.map(str::to_string),
            head: String::new(),
            is_main: false,
            locked: false,
            prunable: false,
        }
    }

    fn repos() -> Repos {
        let mut repos = Repos::default();
        repos.open(
            PathBuf::from("/p/site"),
            "site".into(),
            vec![
                worktree("/p/site", Some("main")),
                worktree("/p/site-fix", Some("fix")),
            ],
        );
        repos.open(
            PathBuf::from("/p/api"),
            "api".into(),
            vec![worktree("/p/api", Some("dev"))],
        );
        repos
    }

    #[test]
    fn opening_the_same_repository_twice_adds_one_row() {
        let mut repos = repos();
        assert!(!repos.open(PathBuf::from("/p/api"), "api".into(), Vec::new()));
        assert_eq!(repos.iter().count(), 2);
        // And it has kept its worktrees rather than being replaced by an
        // empty one: the window reposts everything when a server comes back.
        assert_eq!(repos.worktree_paths(Path::new("/p/api")).len(), 1);
    }

    #[test]
    fn a_repository_that_comes_back_stops_being_missing() {
        let mut repos = Repos::default();
        assert!(repos.mark_missing(PathBuf::from("/p/site"), "not a git folder".into()));
        // Twice is once: the startup asks for every remembered repository, and
        // a relaunched server asks again.
        assert!(!repos.mark_missing(PathBuf::from("/p/site"), "not a git folder".into()));
        assert_eq!(repos.missing().len(), 1);
        repos.open(PathBuf::from("/p/site"), "site".into(), Vec::new());
        assert!(repos.missing().is_empty());
    }

    #[test]
    fn closing_removes_a_row_of_either_kind() {
        let mut repos = repos();
        repos.mark_missing(PathBuf::from("/p/gone"), "not found".into());
        assert!(repos.close(Path::new("/p/gone")).is_empty());
        assert_eq!(
            repos.close(Path::new("/p/api")),
            vec![PathBuf::from("/p/api")]
        );
        assert!(repos.missing().is_empty());
        assert_eq!(repos.mains(), vec![PathBuf::from("/p/site")]);
    }

    #[test]
    fn a_worktree_names_its_repository() {
        let repos = repos();
        assert_eq!(
            repos.main_of(Path::new("/p/site-fix")),
            Some(PathBuf::from("/p/site"))
        );
        assert!(repos.contains_worktree(Path::new("/p/api")));
        assert_eq!(repos.main_of(Path::new("/p/elsewhere")), None);
        assert!(!repos.contains_worktree(Path::new("/p/elsewhere")));
    }

    #[test]
    fn the_numbered_shortcuts_follow_the_sidebar() {
        let mut repos = repos();
        let order = vec![
            PathBuf::from("/p/site"),
            PathBuf::from("/p/site-fix"),
            PathBuf::from("/p/api"),
        ];
        assert_eq!(repos.worktrees_in_order(), order);
        // Collapsing a repository must not move what `Ctrl+3` names.
        repos
            .get_mut(Path::new("/p/site"))
            .expect("the repo")
            .collapsed = true;
        assert_eq!(repos.worktrees_in_order(), order);
        assert_eq!(repos.first_worktree(), Some(PathBuf::from("/p/site")));
    }

    #[test]
    fn a_branch_is_never_offered_as_its_own_base() {
        let mut repos = repos();
        repos
            .get_mut(Path::new("/p/site"))
            .expect("the repo")
            .default_base = Some("main".into());
        // The agent's worktree compares against `main`…
        assert_eq!(
            repos.default_base_for(Path::new("/p/site-fix")),
            Some("main".into())
        );
        // …but the checkout that *is* `main` has nothing to compare to.
        assert_eq!(repos.default_base_for(Path::new("/p/site")), None);
        // And without git's answer there is no base to propose at all.
        assert_eq!(repos.default_base_for(Path::new("/p/api")), None);
    }

    #[test]
    fn worktrees_replaced_are_the_list_git_has_just_given() {
        let mut repos = repos();
        repos.set_worktrees(
            Path::new("/p/site"),
            vec![worktree("/p/site", Some("main"))],
        );
        assert_eq!(
            repos.worktree_paths(Path::new("/p/site")),
            vec![PathBuf::from("/p/site")]
        );
        assert!(!repos.contains_worktree(Path::new("/p/site-fix")));
        // An unknown repository is not a reason to panic, only nothing to do.
        repos.set_worktrees(Path::new("/p/gone"), Vec::new());
        assert!(repos.worktree_paths(Path::new("/p/gone")).is_empty());
    }
}
