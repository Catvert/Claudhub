//! The open repositories, and everything one asks of that list.
//!
//! Free of gpui, and deliberately so: "which repository does this worktree
//! belong to", "what does `Ctrl+3` name", "is this base worth offering" are
//! decisions, not rendering, and they were only reachable through an entity
//! nothing can build outside a window. It is the same split as
//! `notes.rs` / `notes_view.rs`.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use crate::git::{Branch, Worktree};

/// A repository open in the sidebar.
pub struct RepoState {
    pub main: PathBuf,
    pub name: String,
    pub worktrees: Vec<Worktree>,
    pub branches: Vec<Branch>,
    /// Where new work starts in it — `branch::start_point` —, read with the
    /// branches.
    pub integration: Option<String>,
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
    /// The order the hand gave the home screen's sidebar — projects, and
    /// each one's worktrees by main checkout. **Kept here and applied as the
    /// lists arrive**, not at the sidebar's render: everything that walks the
    /// list — the rail, the plane, `Ctrl+1` to `Ctrl+9` — reads the same
    /// order, and the repositories answer in whatever order their workers
    /// finish.
    order: Order,
}

/// See `Repos::order`. What it does not name comes after what it does, in the
/// order it came: a new worktree at the foot of its project, a project opened
/// for the first time at the foot of the list.
#[derive(Default)]
struct Order {
    projects: Vec<PathBuf>,
    worktrees: BTreeMap<PathBuf, Vec<PathBuf>>,
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
            integration: None,
        });
        self.arrange();
        true
    }

    /// The order a past session left, given before any repository opens.
    pub fn set_order(
        &mut self,
        projects: Vec<PathBuf>,
        worktrees: BTreeMap<PathBuf, Vec<PathBuf>>,
    ) {
        self.order = Order {
            projects,
            worktrees,
        };
        self.arrange();
    }

    /// The projects, then each one's worktrees, in the order kept.
    fn arrange(&mut self) {
        arrange(&mut self.open, &self.order.projects, |repo| &repo.main);
        for repo in &mut self.open {
            if let Some(order) = self.order.worktrees.get(&repo.main) {
                arrange(&mut repo.worktrees, order, |worktree| &worktree.path);
            }
        }
    }

    /// A project dropped on another: it takes that one's place. The order
    /// to keep, `None` when nothing moved.
    pub fn move_project(&mut self, from: &Path, to: &Path) -> Option<Vec<PathBuf>> {
        let shown: Vec<PathBuf> = self.open.iter().map(|repo| repo.main.clone()).collect();
        let order = moved(&self.order.projects, &shown, from, to)?;
        self.order.projects = order.clone();
        self.arrange();
        Some(order)
    }

    /// A worktree dropped on another of the same project: it takes that
    /// one's place. The project's order to keep, `None` when nothing moved —
    /// a drop on another project's worktree among them.
    pub fn move_worktree(&mut self, from: &Path, to: &Path) -> Option<(PathBuf, Vec<PathBuf>)> {
        let main = self.main_of(from)?;
        if self.main_of(to).as_deref() != Some(main.as_path()) {
            return None;
        }
        let shown = self.worktree_paths(&main);
        let kept = self.order.worktrees.get(&main).cloned().unwrap_or_default();
        let order = moved(&kept, &shown, from, to)?;
        self.order.worktrees.insert(main.clone(), order.clone());
        self.arrange();
        Some((main, order))
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
        // git has just enumerated: a worktree it no longer lists loses its
        // place, and one made again at the same path starts at the foot.
        if let Some(order) = self.order.worktrees.get_mut(main) {
            order.retain(|path| worktrees.iter().any(|worktree| &worktree.path == path));
        }
        if let Some(repo) = self.get_mut(main) {
            repo.worktrees = worktrees;
        }
        self.arrange();
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

/// `items` in `order`: what it names in its order, the rest after, as they
/// came — the sort is stable.
fn arrange<T>(items: &mut [T], order: &[PathBuf], key: impl Fn(&T) -> &PathBuf) {
    items.sort_by_key(|item| {
        order
            .iter()
            .position(|path| path == key(item))
            .unwrap_or(usize::MAX)
    });
}

/// The order once `from` has taken `to`'s place — after it when it came from
/// above, before it from below: where the drop was is where it lands.
///
/// `shown` is the list as the sidebar shows it: what has no place yet gets
/// one where it is shown, so that the first move of all does not reshuffle
/// the rest. What `order` names and is not shown — a project not open today —
/// keeps its place.
fn moved(order: &[PathBuf], shown: &[PathBuf], from: &Path, to: &Path) -> Option<Vec<PathBuf>> {
    if from == to {
        return None;
    }
    let mut order = order.to_vec();
    for path in shown {
        if !order.contains(path) {
            order.push(path.clone());
        }
    }
    let at = order.iter().position(|path| path == from)?;
    let target = order.iter().position(|path| path == to)?;
    let item = order.remove(at);
    order.insert(target, item);
    Some(order)
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

    fn paths(list: &[&str]) -> Vec<PathBuf> {
        list.iter().map(PathBuf::from).collect()
    }

    #[test]
    fn a_dropped_entry_takes_the_place_of_the_one_under_it() {
        let shown = paths(&["/a", "/b", "/c"]);
        // Down: after the target. Up: before it.
        assert_eq!(
            moved(&[], &shown, Path::new("/a"), Path::new("/c")),
            Some(paths(&["/b", "/c", "/a"]))
        );
        assert_eq!(
            moved(&[], &shown, Path::new("/c"), Path::new("/a")),
            Some(paths(&["/c", "/a", "/b"]))
        );
        assert_eq!(moved(&[], &shown, Path::new("/b"), Path::new("/b")), None);
        assert_eq!(moved(&[], &shown, Path::new("/z"), Path::new("/b")), None);
    }

    #[test]
    fn a_place_kept_for_what_is_not_open_survives_a_move() {
        // `/gone` is not open today: the move goes round it, and it is still
        // between `/a` and `/b` when it comes back.
        let order = paths(&["/a", "/gone", "/b"]);
        let shown = paths(&["/a", "/b", "/new"]);
        assert_eq!(
            moved(&order, &shown, Path::new("/new"), Path::new("/b")),
            Some(paths(&["/a", "/gone", "/new", "/b"]))
        );
    }

    #[test]
    fn projects_open_in_the_order_kept_whatever_order_they_answer_in() {
        let mut repos = Repos::default();
        repos.set_order(paths(&["/p/api", "/p/site"]), BTreeMap::new());
        repos.open(PathBuf::from("/p/new"), "new".into(), Vec::new());
        repos.open(PathBuf::from("/p/site"), "site".into(), Vec::new());
        repos.open(PathBuf::from("/p/api"), "api".into(), Vec::new());
        let mains: Vec<&Path> = repos.iter().map(|repo| repo.main.as_path()).collect();
        // What the order does not name comes after, as it came.
        assert_eq!(mains, ["/p/api", "/p/site", "/p/new"].map(Path::new));
    }

    #[test]
    fn a_moved_worktree_stays_moved_when_git_lists_again() {
        let mut repos = repos();
        let (main, order) = repos
            .move_worktree(Path::new("/p/site-fix"), Path::new("/p/site"))
            .unwrap();
        assert_eq!(main, PathBuf::from("/p/site"));
        assert_eq!(order, paths(&["/p/site-fix", "/p/site"]));
        // The shortcuts follow the sidebar.
        assert_eq!(
            repos.worktrees_in_order(),
            paths(&["/p/site-fix", "/p/site", "/p/api"])
        );
        // git lists in its own order, and a new worktree appears: the hand's
        // order holds, the newcomer at the foot.
        repos.set_worktrees(
            Path::new("/p/site"),
            vec![
                worktree("/p/site", Some("main")),
                worktree("/p/site-new", Some("new")),
                worktree("/p/site-fix", Some("fix")),
            ],
        );
        assert_eq!(
            repos.worktree_paths(Path::new("/p/site")),
            paths(&["/p/site-fix", "/p/site", "/p/site-new"])
        );
    }

    #[test]
    fn a_worktree_does_not_move_into_another_project() {
        let mut repos = repos();
        assert_eq!(
            repos.move_worktree(Path::new("/p/site-fix"), Path::new("/p/api")),
            None
        );
        assert_eq!(
            repos.worktrees_in_order(),
            paths(&["/p/site", "/p/site-fix", "/p/api"])
        );
    }

    #[test]
    fn a_moved_project_carries_its_worktrees() {
        let mut repos = repos();
        assert_eq!(
            repos.move_project(Path::new("/p/api"), Path::new("/p/site")),
            Some(paths(&["/p/api", "/p/site"]))
        );
        assert_eq!(
            repos.worktrees_in_order(),
            paths(&["/p/api", "/p/site", "/p/site-fix"])
        );
    }
}
