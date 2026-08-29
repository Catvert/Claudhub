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

    /// Files the branch a fresh status reports for one worktree.
    ///
    /// **The list is enumerated once and the branch moves under it.** A
    /// checkout rereads the status — every write does — but nothing rereads
    /// `git worktree list`, so the name in the title bar stayed on the branch
    /// one had just left. The status is the reading that follows a checkout, so
    /// it is the one that files it.
    ///
    /// `true` when something moved, which is what says a frame is owed.
    pub fn set_branch(&mut self, path: &Path, branch: Option<&str>) -> bool {
        let Some(worktree) = self
            .open
            .iter_mut()
            .flat_map(|repo| repo.worktrees.iter_mut())
            .find(|w| w.path == path)
        else {
            return false;
        };
        if worktree.branch.as_deref() == branch {
            return false;
        }
        worktree.branch = branch.map(str::to_string);
        true
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

    /// A checkout rereads the status and nothing rereads the worktree list, so
    /// this is the only thing that moves the branch under the title bar.
    #[test]
    fn a_status_moves_the_branch_of_the_worktree_it_is_about() {
        let mut repos = repos();
        assert!(repos.set_branch(Path::new("/p/site"), Some("release")));
        assert_eq!(
            repos
                .worktree(Path::new("/p/site"))
                .unwrap()
                .branch
                .as_deref(),
            Some("release")
        );
        // Its neighbour has not moved: a status is about one checkout.
        assert_eq!(
            repos
                .worktree(Path::new("/p/site-fix"))
                .unwrap()
                .branch
                .as_deref(),
            Some("fix")
        );
        // The same answer twice is no change, so no frame is owed — a status
        // arrives on every file write.
        assert!(!repos.set_branch(Path::new("/p/site"), Some("release")));
        // A detached head has no name, and that is a change like any other.
        assert!(repos.set_branch(Path::new("/p/site"), None));
        assert!(repos
            .worktree(Path::new("/p/site"))
            .unwrap()
            .branch
            .is_none());
        // A path nothing holds is not an error: a status can outlive a
        // repository being closed.
        assert!(!repos.set_branch(Path::new("/p/gone"), Some("main")));
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
    fn the_numbered_shortcuts_follow_the_picker() {
        let repos = repos();
        // The order the picker lists them in, repository by repository: that is
        // what `Ctrl+3` names, and it must not depend on anything else.
        assert_eq!(
            repos.worktrees_in_order(),
            vec![
                PathBuf::from("/p/site"),
                PathBuf::from("/p/site-fix"),
                PathBuf::from("/p/api"),
            ]
        );
        assert_eq!(repos.first_worktree(), Some(PathBuf::from("/p/site")));
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
