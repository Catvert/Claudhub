//! Real repositories: a gitlink is not a file, and a parser fixture alone
//! cannot prove which repository a stage or commit actually changes.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};

use super::{diff, git, repo, status, DiffRange, Repo};

pub(crate) struct Fixture {
    pub root: PathBuf,
    pub parent: PathBuf,
    pub child: PathBuf,
    pub path: PathBuf,
}

impl Fixture {
    pub fn new() -> Self {
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        let root = std::env::temp_dir().join(format!(
            "claudhub-submodule-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&root).unwrap();
        let root = root.canonicalize().unwrap();
        let parent = root.join("parent");
        init(&parent);
        let path = PathBuf::from("modules/app tech");
        let child = add_submodule(&parent, "app-tech", &path);
        commit(&parent);
        Self {
            root,
            parent,
            child,
            path,
        }
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

fn init(dir: &Path) {
    std::fs::create_dir_all(dir).unwrap();
    git(dir, &["init", "-q", "-b", "main"]).unwrap();
    for (key, value) in [
        ("user.name", "Test"),
        ("user.email", "test@example.com"),
        ("commit.gpgsign", "false"),
        ("core.hooksPath", "/dev/null"),
    ] {
        git(dir, &["config", key, value]).unwrap();
    }
}

pub(crate) fn add_submodule(parent: &Path, name: &str, path: &Path) -> PathBuf {
    let child = parent.join(path);
    init(&child);
    std::fs::create_dir_all(child.join("src/deep")).unwrap();
    std::fs::write(child.join("src/deep/code.txt"), "before\n").unwrap();
    std::fs::write(child.join(".gitignore"), "vendor/\n").unwrap();
    git(&child, &["add", "."]).unwrap();
    commit(&child);
    git(
        parent,
        &[
            "config",
            "-f",
            ".gitmodules",
            &format!("submodule.{name}.path"),
            &path.to_string_lossy(),
        ],
    )
    .unwrap();
    git(
        parent,
        &[
            "config",
            "-f",
            ".gitmodules",
            &format!("submodule.{name}.url"),
            "./local-only",
        ],
    )
    .unwrap();
    git(
        parent,
        &["add", "--", ".gitmodules", &path.to_string_lossy()],
    )
    .unwrap();
    git(
        parent,
        &["submodule", "absorbgitdirs", "--", &path.to_string_lossy()],
    )
    .unwrap();
    child
}

fn commit(dir: &Path) {
    repo::commit(
        dir,
        repo::CommitOptions {
            message: "Test change",
            amend: false,
            all: false,
        },
    )
    .unwrap();
}

#[test]
fn submodule_dirty_and_untracked_changes_override_ignore_preferences() {
    let fixture = Fixture::new();
    let f = &fixture;
    assert!(status::status(&f.parent).unwrap().is_clean());
    git(&f.parent, &["config", "submodule.app-tech.ignore", "all"]).unwrap();
    git(&f.parent, &["config", "diff.ignoreSubmodules", "all"]).unwrap();
    git(&f.parent, &["config", "diff.submodule", "log"]).unwrap();

    // Untracked-only is a dirty submodule too, but cannot be staged here.
    std::fs::write(f.child.join("new file.txt"), "new\n").unwrap();
    let st = status::status(&f.parent).unwrap();
    assert_eq!(st.files.len(), 1);
    let file = &st.files[0];
    assert_eq!(file.path, f.path);
    assert_eq!(
        file.submodule,
        Some(status::SubmoduleStatus {
            commit_changed: false,
            modified: false,
            untracked: true,
        })
    );
    assert!(!file.is_stageable());
    assert!(file.is_unstaged());

    std::fs::write(f.child.join("src/deep/code.txt"), "after\n").unwrap();
    let st = status::status(&f.parent).unwrap();
    assert!(st.files[0].submodule.unwrap().modified);
    assert!(!st.files[0].is_staged());
    assert_eq!(status::summary(&f.parent).unwrap().files, 1);
    assert_eq!(
        diff::files(&f.parent, &DiffRange::Working).unwrap()[0].path,
        f.path
    );
    let patch = diff::file(&f.parent, &DiffRange::Working, &f.path, None, 3).unwrap();
    assert!(!patch.empty);
    assert!(patch
        .hunks
        .iter()
        .flat_map(|h| &h.lines)
        .any(|l| l.text.ends_with("-dirty")));
    assert!(!diff::unstaged_file(&f.parent, &f.path, 3).unwrap().empty);
    // These are per-command overrides, never a rewrite of the user's config.
    assert_eq!(
        git(&f.parent, &["config", "submodule.app-tech.ignore"]).unwrap(),
        "all"
    );
}

#[test]
fn submodule_commits_and_parent_gitlinks_use_separate_indexes() {
    let f = Fixture::new();
    git(&f.parent, &["config", "submodule.app-tech.ignore", "all"]).unwrap();
    git(&f.parent, &["config", "diff.submodule", "diff"]).unwrap();
    let initial = git(&f.child, &["rev-parse", "HEAD"]).unwrap();
    std::fs::write(f.child.join("src/deep/code.txt"), "committed\n").unwrap();
    repo::stage(&f.child, &["src/deep/code.txt".into()]).unwrap();
    assert!(status::status(&f.parent).unwrap().staged().next().is_none());
    commit(&f.child);
    let current = git(&f.child, &["rev-parse", "HEAD"]).unwrap();
    assert_ne!(current, initial);
    std::fs::write(f.child.join("src/deep/code.txt"), "still dirty\n").unwrap();
    let st = status::status(&f.parent).unwrap();
    assert!(st.files[0].is_stageable());
    assert!(st.files[0].submodule.unwrap().commit_changed);
    assert!(st.files[0].submodule.unwrap().modified);

    repo::stage(&f.parent, std::slice::from_ref(&f.path)).unwrap();
    let st = status::status(&f.parent).unwrap();
    assert!(st.files[0].is_staged());
    assert!(!st.files[0].is_stageable());
    assert!(!st.files[0].submodule.unwrap().commit_changed);
    assert!(status::status(&f.child).unwrap().staged().next().is_none());
    let staged = diff::staged_text(&f.parent).unwrap();
    assert!(staged.contains(&initial) && staged.contains(&current));
    assert!(!staged.contains("still dirty"));
    repo::unstage(&f.parent, std::slice::from_ref(&f.path)).unwrap();
    assert!(status::status(&f.parent).unwrap().files[0].is_stageable());
    repo::stage(&f.parent, std::slice::from_ref(&f.path)).unwrap();
    commit(&f.parent);
    assert!(
        status::status(&f.parent).unwrap().files[0]
            .submodule
            .unwrap()
            .modified
    );
    let range = DiffRange::Commit {
        id: git(&f.parent, &["rev-parse", "HEAD"]).unwrap(),
        parent: Some(git(&f.parent, &["rev-parse", "HEAD^"]).unwrap()),
    };
    assert!(
        !diff::file(&f.parent, &range, &f.path, None, 3)
            .unwrap()
            .empty
    );
}

#[test]
fn submodule_discovery_finds_checkouts_instead_of_metadata_directories() {
    let f = Fixture::new();
    for dir in [&f.child, &f.child.join("src/deep")] {
        let repo = Repo::discover(dir).unwrap();
        assert_eq!(repo.main, f.child);
        let trees = repo.worktrees().unwrap();
        assert_eq!(trees[0].path, f.child);
        assert_eq!(trees[0].branch.as_deref(), Some("main"));
    }
    for main in [&f.parent, &f.child] {
        let linked = f.root.join(if main == &f.parent {
            "parent-linked"
        } else {
            "child-linked"
        });
        git(
            main,
            &["worktree", "add", "--detach", &linked.to_string_lossy()],
        )
        .unwrap();
        let repo = Repo::discover(&linked).unwrap();
        assert_eq!(&repo.main, main);
        let trees = repo.worktrees().unwrap();
        assert_eq!(&trees[0].path, main);
        assert!(trees
            .iter()
            .any(|tree| tree.path == linked && tree.branch.is_none()));
    }
}

#[test]
fn submodule_uninitialized_checkouts_do_not_break_parent_status() {
    let f = Fixture::new();
    std::fs::remove_dir_all(&f.child).unwrap();
    std::fs::create_dir(&f.child).unwrap();
    assert!(status::status(&f.parent).unwrap().is_clean());
    assert!(diff::files(&f.parent, &DiffRange::Working)
        .unwrap()
        .is_empty());
    let files = repo::list_files(&f.parent, false).unwrap();
    assert!(files.all.contains(&f.path));
    assert!(files.dirs.contains(&f.path));
    assert!(!files
        .all
        .iter()
        .any(|path| path != &f.path && path.starts_with(&f.path)));
}

#[test]
fn submodule_files_are_browsable_from_the_parent_with_their_own_ignore_rules() {
    let f = Fixture::new();
    let nested_path = Path::new("deps/nested module");
    let nested = add_submodule(&f.child, "nested", nested_path);
    std::fs::write(f.child.join("new file.txt"), "untracked\n").unwrap();
    for checkout in [&f.child, &nested] {
        std::fs::create_dir_all(checkout.join("vendor/package")).unwrap();
        std::fs::write(checkout.join("vendor/package/code.txt"), "ignored\n").unwrap();
    }
    let nested_relative = f.path.join(nested_path);
    let plain = repo::list_files(&f.parent, false).unwrap();
    for path in [f.path.clone(), nested_relative.clone()] {
        assert!(plain.dirs.contains(&path));
        assert!(plain.all.contains(&path.join("src/deep/code.txt")));
        assert!(!plain.all.contains(&path.join(".git")));
        assert!(!plain
            .all
            .iter()
            .any(|file| file.starts_with(path.join("vendor"))));
    }
    assert!(plain.all.contains(&f.path.join("new file.txt")));
    assert!(plain.ignored.is_empty());
    assert_eq!(
        crate::files::read(&f.parent, &nested_relative.join("src/deep/code.txt"))
            .unwrap()
            .text,
        "before\n"
    );

    let all = repo::list_files(&f.parent, true).unwrap();
    for path in [&f.path, &nested_relative] {
        let vendor = path.join("vendor");
        assert!(all.dirs.contains(&vendor));
        assert!(all.ignored.contains(&vendor));
        assert!(!all.ignored.contains(path));
        assert!(
            !all.all.contains(&vendor.join("package/code.txt")),
            "ignored directories remain lazy"
        );
    }
    assert!(plain.all.iter().all(|path| all.all.contains(path)));
    for paths in [&all.all, &all.dirs, &all.ignored] {
        assert!(
            paths.windows(2).all(|pair| pair[0] < pair[1]),
            "sorted and unique: {paths:?}"
        );
    }
}

#[test]
fn submodule_changes_are_grouped_with_separate_indexes_and_file_actions() {
    let f = Fixture::new();
    let local = Path::new("src/deep/code.txt");
    let displayed = f.path.join(local);
    let new_file = f.path.join("new file.txt");
    git(&f.parent, &["config", "submodule.app-tech.ignore", "all"]).unwrap();
    std::fs::write(f.child.join(local), "staged child\n").unwrap();
    std::fs::write(f.child.join("new file.txt"), "new child\n").unwrap();
    std::fs::write(f.parent.join("parent.txt"), "parent\n").unwrap();

    repo::stage(&f.parent, &[displayed.clone(), "parent.txt".into()]).unwrap();
    std::fs::write(f.child.join(local), "unstaged child\n").unwrap();
    let st = status::status(&f.parent).unwrap();
    assert_eq!(st.staged().count(), 1, "the parent owns only parent.txt");
    assert_eq!(st.submodules.len(), 1);
    assert_eq!(st.submodules[0].path, f.path);
    assert_eq!(st.submodules[0].status.staged().count(), 1);
    let child_file = st.file(&displayed).unwrap();
    assert!(child_file.is_staged() && child_file.is_unstaged());
    assert_eq!(child_file.path, local);
    assert!(st.file(&new_file).unwrap().is_untracked());
    assert_eq!(st.file_location(&displayed), (f.path.clone(), local.into()));
    assert_eq!(st.file_location(&f.path), (PathBuf::new(), f.path.clone()));
    assert!(st
        .files_recursive()
        .iter()
        .any(|(path, _)| path == &new_file));
    assert_eq!(
        repo::head_blob(&f.parent, &displayed).as_deref(),
        Some("before\n")
    );

    let files = diff::files(&f.parent, &DiffRange::Working).unwrap();
    let changed = files.iter().find(|file| file.path == displayed).unwrap();
    assert_eq!((changed.added, changed.removed), (1, 1));
    let whole = diff::file(&f.parent, &DiffRange::Working, &displayed, None, 3).unwrap();
    assert!(whole.hunks[0]
        .lines
        .iter()
        .any(|line| line.text == "before"));
    assert!(whole.hunks[0]
        .lines
        .iter()
        .any(|line| line.text == "unstaged child"));
    let remainder = diff::unstaged_file(&f.parent, &displayed, 3).unwrap();
    assert!(remainder.hunks[0]
        .lines
        .iter()
        .any(|line| line.text == "staged child"));
    assert!(!diff::untracked_file(&f.parent, &new_file).unwrap().empty);

    // The same owner/local mapping used by the hunk button applies only to
    // the child's index, even when the parent also has a staged file.
    let (owner, path) = st.file_location(&displayed);
    let patch = diff::hunk_patch(&path, None, &remainder.hunks[0], false);
    repo::apply_patch(&f.parent.join(owner), &patch, false).unwrap();
    assert!(diff::unstaged_file(&f.parent, &displayed, 3).unwrap().empty);
    assert_eq!(status::status(&f.parent).unwrap().staged().count(), 1);
    repo::unstage(&f.parent, std::slice::from_ref(&displayed)).unwrap();
    assert!(status::status(&f.child).unwrap().staged().next().is_none());
    repo::discard(&f.parent, std::slice::from_ref(&displayed)).unwrap();
    repo::clean(&f.parent, std::slice::from_ref(&new_file)).unwrap();
    assert_eq!(
        std::fs::read_to_string(f.child.join(local)).unwrap(),
        "before\n"
    );
    assert!(!f.child.join("new file.txt").exists());
    assert!(status::status(&f.parent).unwrap().submodules.is_empty());
}

#[test]
fn submodule_nested_changes_route_to_the_innermost_repository() {
    let f = Fixture::new();
    let nested_path = Path::new("deps/nested module");
    let nested = add_submodule(&f.child, "nested", nested_path);
    commit(&f.child);
    repo::stage(&f.parent, std::slice::from_ref(&f.path)).unwrap();
    commit(&f.parent);
    let local = Path::new("src/deep/code.txt");
    let displayed = f.path.join(nested_path).join(local);
    std::fs::write(nested.join(local), "nested change\n").unwrap();
    let st = status::status(&f.parent).unwrap();
    assert_eq!(st.submodules[0].status.submodules[0].path, nested_path);
    assert!(st.file(&displayed).unwrap().is_stageable());
    assert_eq!(
        st.file_location(&displayed),
        (f.path.join(nested_path), local.into())
    );
    assert!(diff::files(&f.parent, &DiffRange::Working)
        .unwrap()
        .iter()
        .any(|file| file.path == displayed));
    assert!(
        !diff::file(&f.parent, &DiffRange::Working, &displayed, None, 3)
            .unwrap()
            .empty
    );
    repo::stage(&f.parent, std::slice::from_ref(&displayed)).unwrap();
    assert_eq!(status::status(&nested).unwrap().staged().count(), 1);
    assert_eq!(status::status(&f.child).unwrap().staged().count(), 0);
    assert_eq!(status::status(&f.parent).unwrap().staged().count(), 0);
    commit(&nested);
    // Selecting the nested gitlink itself updates the middle index.
    repo::stage(&f.parent, &[f.path.join(nested_path)]).unwrap();
    assert_eq!(status::status(&f.child).unwrap().staged().count(), 1);
    assert_eq!(status::status(&f.parent).unwrap().staged().count(), 0);
}
