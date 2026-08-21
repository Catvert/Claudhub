//! Watching a worktree's files.
//!
//! This is what keeps the review alive when the work happens elsewhere: an
//! agent writing in the built-in terminal, a `git commit` typed by hand, an
//! external editor. Without it you would have to press "refresh" after every
//! action, which, in a tool whose whole subject is watching what another
//! process makes, is the wrong default.
//!
//! Two event sources matter: the working tree, and `.git/HEAD` / `.git/index` —
//! without those two, a commit typed at the keyboard would leave the panel
//! showing files that are no longer modified.

use std::collections::HashSet;
use std::path::{Component, Path, PathBuf};
use std::sync::mpsc;
use std::sync::OnceLock;
use std::time::Duration;

use notify::RecursiveMode;
use notify_debouncer_full::{new_debouncer, DebouncedEvent};

/// What the watcher thread knows how to do.
enum Order {
    Watch(PathBuf),
    Unwatch(PathBuf),
    /// A folder on its own, without recursion and without going through git:
    /// a worktree's notes vault, which is not a repository and about which
    /// `ls-files` would say nothing.
    WatchDir(PathBuf),
    UnwatchDir(PathBuf),
}

/// Debounce window. A build touches thousands of files; refreshing on every
/// write would amount to running `git status` in a loop. A quarter of a second
/// stays imperceptible and reduces a burst to a single refresh.
const DEBOUNCE: Duration = Duration::from_millis(250);

/// The interface-side facade: all it does is send orders.
///
/// Setting up a recursive watch costs one system call per folder — nearly half
/// a second on a tree of forty thousand directories, which is half a second of
/// frozen window if done in the thread that draws. The order therefore goes
/// into a dedicated thread and the worktree picker returns immediately.
pub struct Watcher {
    orders: mpsc::Sender<Order>,
}

impl Watcher {
    /// Starts watching and returns the receiver of changed paths.
    ///
    /// What the receiver delivers is a **batch** of arbitrary paths under the
    /// watched worktrees — one batch per debounce window, which makes it a
    /// single `Evt` on the wire; it is up to the caller to attach each path to
    /// the worktree it knows, being the only one to know which are open.
    pub fn new() -> anyhow::Result<(Self, async_channel::Receiver<Vec<PathBuf>>)> {
        // Async channel: a gpui task drains it, and it cannot afford to wait on
        // a blocking `recv`.
        let (tx, rx) = async_channel::unbounded::<Vec<PathBuf>>();
        let (raw_tx, raw_rx) = mpsc::channel();
        let mut debouncer = new_debouncer(DEBOUNCE, None, raw_tx)?;
        let (order_tx, order_rx) = mpsc::channel::<Order>();

        // One thread to set up and remove the watches, long operations on a
        // large tree.
        std::thread::Builder::new()
            .name("claudhub-watch-orders".into())
            .spawn(move || {
                let mut watched: HashSet<PathBuf> = HashSet::new();
                while let Ok(order) = order_rx.recv() {
                    match order {
                        Order::Watch(path) => {
                            if !watched.insert(path.clone()) {
                                continue;
                            }
                            if on_windows_filesystem(&path) {
                                log::warn!(
                                    "{} is on a Windows drive: inotify reports \
                                     nothing there, the review will not refresh \
                                     by itself",
                                    path.display()
                                );
                            }
                            for (dir, mode) in watchable_directories(&path) {
                                if let Err(e) = debouncer.watch(&dir, mode) {
                                    log::warn!("cannot watch {}: {e}", dir.display());
                                }
                            }
                        }
                        Order::Unwatch(path) => {
                            if watched.remove(&path) {
                                for (dir, _) in watchable_directories(&path) {
                                    let _ = debouncer.unwatch(&dir);
                                }
                            }
                        }
                        Order::WatchDir(path) => {
                            // A folder that does not exist yet is not an error:
                            // it is a worktree that has not been annotated. It
                            // only enters `watched` if the watch was really set
                            // up, otherwise the order sent again after creating
                            // it would be taken for a duplicate and set up
                            // nothing.
                            if watched.contains(&path) || !path.is_dir() {
                                continue;
                            }
                            match debouncer.watch(&path, RecursiveMode::NonRecursive) {
                                Ok(()) => {
                                    watched.insert(path);
                                }
                                Err(e) => log::warn!("cannot watch {}: {e}", path.display()),
                            }
                        }
                        Order::UnwatchDir(path) => {
                            if watched.remove(&path) {
                                let _ = debouncer.unwatch(&path);
                            }
                        }
                    }
                }
            })?;

        // A translation thread: from a batch it keeps only the paths that change
        // something, deduplicated.
        std::thread::Builder::new()
            .name("claudhub-watch".into())
            .spawn(move || {
                while let Ok(result) = raw_rx.recv() {
                    let Ok(events) = result else { continue };
                    let mut seen: Vec<PathBuf> = Vec::new();
                    for event in &events {
                        for path in interesting_paths(event) {
                            if !seen.contains(&path) {
                                seen.push(path);
                            }
                        }
                    }
                    if !seen.is_empty() && tx.send_blocking(seen).is_err() {
                        return; // the window is gone
                    }
                }
            })?;

        Ok((Self { orders: order_tx }, rx))
    }

    /// Watches a worktree. Calling twice has no effect, and the call returns
    /// immediately: the work happens elsewhere.
    pub fn watch(&self, worktree: &Path) {
        let _ = self.orders.send(Order::Watch(worktree.to_path_buf()));
    }

    pub fn unwatch(&self, worktree: &Path) {
        let _ = self.orders.send(Order::Unwatch(worktree.to_path_buf()));
    }

    /// Watches a folder as it is, without recursion.
    ///
    /// That is what a notes vault needs: no git, no subfolders, and a single
    /// system call. What comes back goes through the same channel — the caller
    /// is the one who knows which worktree that folder belongs to.
    pub fn watch_dir(&self, dir: &Path) {
        let _ = self.orders.send(Order::WatchDir(dir.to_path_buf()));
    }

    pub fn unwatch_dir(&self, dir: &Path) {
        let _ = self.orders.send(Order::UnwatchDir(dir.to_path_buf()));
    }
}

/// The paths of an event that deserve a refresh.
fn interesting_paths(event: &DebouncedEvent) -> Vec<PathBuf> {
    if !changes_content(&event.kind) {
        return Vec::new();
    }
    event
        .paths
        .iter()
        .filter(|p| is_interesting(p))
        .cloned()
        .collect()
}

/// True for an event likely to change what `git status` answers.
///
/// The decisive filter is `Access`: inotify reports every **opening** of a
/// file, and we are the ones opening them. `git status` read the worktree, each
/// read produced an event, each event triggered a `git status` — a loop running
/// flat out, invisible as long as the list did not empty between two answers.
///
/// Metadata is left out for the same reason: an access time or a mode changing
/// changes nothing of what git sees. `Any` and `Other` are kept — that is how
/// `notify` reports an overflow of its queue, after which there is precisely
/// everything to re-read.
fn changes_content(kind: &notify::EventKind) -> bool {
    use notify::event::{EventKind, ModifyKind};
    match kind {
        EventKind::Access(_) => false,
        EventKind::Modify(ModifyKind::Metadata(_)) => false,
        EventKind::Create(_) | EventKind::Remove(_) | EventKind::Modify(_) => true,
        EventKind::Any | EventKind::Other => true,
    }
}

/// Everything under `.git/` is left out except the few references whose change
/// alters what `git status` answers: without this filter, every git command
/// would trigger a dozen refreshes for its lock files, its logs and its
/// freshly written objects.
fn is_interesting(path: &Path) -> bool {
    let text = path.to_string_lossy();
    let Some(pos) = text.find("/.git/") else {
        // Outside `.git/`: this is work, it counts — including a `Cargo.lock`,
        // which is a tracked file like any other.
        return !text.ends_with("/.git");
    };
    let inside = &text[pos + "/.git/".len()..];
    // Locks are created then destroyed by every git command: they announce a
    // write that has not happened yet.
    if inside.ends_with(".lock") {
        return false;
    }
    inside == "HEAD"
        || inside == "index"
        || inside == "ORIG_HEAD"
        || inside == "MERGE_HEAD"
        || inside.starts_with("refs/")
}

/// True if this path is on a Windows drive mounted by WSL.
///
/// It is the only case where watching fails **silently**: on drvfs (`/mnt/c`,
/// `/mnt/d`…), `notify` sets up its watches without error and never delivers an
/// event, because the WSL kernel has nothing to translate — the writes happen
/// on the Windows side. The whole promise of "the review follows without being
/// asked" then vanishes in silence, and it is up to the interface to say so.
///
/// A polling fallback would be worse: `git status` already costs several times
/// there what it costs on the Linux filesystem, and putting it on a timer would
/// make you pay permanently for what moving the repository to `~` removes at a
/// stroke.
pub fn on_windows_filesystem(path: &Path) -> bool {
    running_under_wsl() && is_windows_mount(path)
}

/// WSL's kernel carries "microsoft" in its version, under WSL1 as under WSL2;
/// that is how everybody recognises it, for want of another stable marker.
pub(crate) fn running_under_wsl() -> bool {
    static WSL: OnceLock<bool> = OnceLock::new();
    *WSL.get_or_init(|| {
        std::fs::read_to_string("/proc/sys/kernel/osrelease")
            .map(|release| release.to_ascii_lowercase().contains("microsoft"))
            .unwrap_or(false)
    })
}

/// `/mnt/c`, `/mnt/d`… — the way WSL mounts Windows drives.
///
/// The automount root is configurable (`/etc/wsl.conf`), so the recognition
/// misses installations that have moved it. That is accepted: this test only
/// serves to show a warning, and a missing warning is better than a lying one.
pub(crate) fn is_windows_mount(path: &Path) -> bool {
    let mut parts = path.components();
    if parts.next() != Some(Component::RootDir) {
        return false;
    }
    if parts.next() != Some(Component::Normal("mnt".as_ref())) {
        return false;
    }
    match parts.next() {
        Some(Component::Normal(drive)) => drive
            .to_str()
            .is_some_and(|d| d.len() == 1 && d.chars().all(|c| c.is_ascii_alphabetic())),
        _ => false,
    }
}

/// The folders of a worktree worth watching.
///
/// They are the ones git knows: the folders containing a tracked file, or a new
/// file that is not ignored. `git ls-files` is what gives them, in one command
/// and a few tens of milliseconds.
///
/// Watching the worktree wholesale would cost a hundred times more for nothing.
/// On the application that motivated this filter, forty thousand directories
/// exist but seven hundred and twenty-one contain code: the rest is `vendor/`,
/// `node_modules/` and above all `storage/`, which Laravel does not declare
/// ignored folder by folder and which a development server rewrites constantly.
/// Every one of those writes produced a wake-up, so a `git status`, so a reload
/// of the review — in a loop.
///
/// Each folder is watched **without recursion**: its subfolders are already in
/// the list if they contain anything, and a folder created later is reported by
/// its parent, which is enough to trigger the refresh that will discover it.
///
/// `.git` is added separately: its root for `HEAD` and `index`, and `refs/`
/// recursively since it is small. Taking it whole would bring back the
/// thousands of object directories.
fn watchable_directories(worktree: &Path) -> Vec<(PathBuf, RecursiveMode)> {
    let mut dirs: Vec<(PathBuf, RecursiveMode)> = Vec::new();

    match tracked_directories(worktree) {
        Some(tracked) => {
            dirs.extend(
                tracked
                    .into_iter()
                    .map(|dir| (dir, RecursiveMode::NonRecursive)),
            );
            dirs.push((worktree.to_path_buf(), RecursiveMode::NonRecursive));
        }
        // Without git to hand, watching everything is still correct, only slow.
        None => dirs.push((worktree.to_path_buf(), RecursiveMode::Recursive)),
    }

    if let Some(git_dir) = git_dir(worktree) {
        let refs = git_dir.join("refs");
        dirs.push((git_dir, RecursiveMode::NonRecursive));
        if refs.is_dir() {
            dirs.push((refs, RecursiveMode::Recursive));
        }
    }
    dirs
}

/// The folders containing a file git tracks, or a new file it does not ignore.
fn tracked_directories(worktree: &Path) -> Option<HashSet<PathBuf>> {
    use std::process::{Command, Stdio};

    let out = Command::new("git")
        .arg("-C")
        .arg(worktree)
        .args([
            "ls-files",
            "-z",
            "--cached",
            "--others",
            "--exclude-standard",
        ])
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }

    let text = String::from_utf8_lossy(&out.stdout);
    let mut dirs = HashSet::new();
    for file in text.split('\0').filter(|s| !s.is_empty()) {
        // Every ancestor, not only the parent: an intermediate folder holding
        // nothing but subfolders has to be watched too, otherwise creating a
        // file at that level would go unnoticed.
        let mut current = Path::new(file).parent();
        while let Some(dir) = current.filter(|d| !d.as_os_str().is_empty()) {
            if !dirs.insert(worktree.join(dir)) {
                break; // already seen: so are its ancestors
            }
            current = dir.parent();
        }
    }
    Some(dirs)
}

/// A checkout's git directory.
///
/// In the main repository it is `.git/`. In a linked worktree, `.git` is a
/// *file* pointing at `<main>/.git/worktrees/<name>`: that is where this
/// checkout's `HEAD` and `index` live, and watching them in the wrong place
/// amounts to watching nothing at all.
fn git_dir(worktree: &Path) -> Option<PathBuf> {
    let entry = worktree.join(".git");
    if entry.is_dir() {
        return Some(entry);
    }
    let text = std::fs::read_to_string(&entry).ok()?;
    let target = text.strip_prefix("gitdir:")?.trim();
    let path = PathBuf::from(target);
    let path = if path.is_absolute() {
        path
    } else {
        worktree.join(path)
    };
    path.is_dir().then_some(path)
}

#[cfg(test)]
mod tests {
    use notify::event::{AccessKind, CreateKind, EventKind, MetadataKind, ModifyKind};

    /// The flaw that made Claudhub loop: inotify reports every file opening,
    /// `git status` opens thousands of them, and each event restarted a `git
    /// status`.
    #[test]
    fn opening_a_file_is_not_a_change() {
        assert!(!changes_content(&EventKind::Access(AccessKind::Open(
            notify::event::AccessMode::Any
        ))));
        assert!(!changes_content(&EventKind::Access(AccessKind::Read)));
        // An access time teaches git nothing either.
        assert!(!changes_content(&EventKind::Modify(ModifyKind::Metadata(
            MetadataKind::AccessTime
        ))));

        assert!(changes_content(&EventKind::Create(CreateKind::File)));
        assert!(changes_content(&EventKind::Modify(ModifyKind::Data(
            notify::event::DataChange::Content
        ))));
        assert!(changes_content(&EventKind::Remove(
            notify::event::RemoveKind::File
        )));
        // The queue-overflow signal: there is everything to re-read.
        assert!(changes_content(&EventKind::Any));
    }

    use super::*;

    /// A repository on `/mnt/c` reports no event; this test is what holds the
    /// recognition, the "am I under WSL" part not being checkable anywhere but
    /// under WSL.
    #[test]
    fn windows_drives_are_recognised_by_their_mount_point() {
        assert!(is_windows_mount(Path::new("/mnt/c/Users/friend/project")));
        assert!(is_windows_mount(Path::new("/mnt/d")));
        assert!(!is_windows_mount(Path::new("/home/friend/project")));
        // A mount point of ours starting the same way is not one.
        assert!(!is_windows_mount(Path::new("/mnt/data/project")));
        assert!(!is_windows_mount(Path::new("/mnt")));
    }

    #[test]
    fn work_tree_changes_are_interesting() {
        assert!(is_interesting(Path::new("/repo/src/main.rs")));
        assert!(is_interesting(Path::new("/repo/assets/i18n/fr.json")));
        // A tracked file called `.lock` is still work.
        assert!(is_interesting(Path::new("/repo/Cargo.lock")));
    }

    #[test]
    fn git_internals_are_filtered_except_the_refs_that_matter() {
        assert!(is_interesting(Path::new("/repo/.git/HEAD")));
        assert!(is_interesting(Path::new("/repo/.git/index")));
        assert!(is_interesting(Path::new("/repo/.git/refs/heads/main")));

        // Noise from every git command.
        assert!(!is_interesting(Path::new("/repo/.git/index.lock")));
        assert!(!is_interesting(Path::new("/repo/.git/logs/HEAD")));
        assert!(!is_interesting(Path::new("/repo/.git/objects/ab/cdef")));
        assert!(!is_interesting(Path::new("/repo/.git/COMMIT_EDITMSG")));
    }

    #[test]
    fn a_linked_worktree_points_at_its_own_git_directory() {
        let root = std::env::temp_dir().join(format!("claudhub-gitdir-{}", std::process::id()));
        let main = root.join("repo/.git/worktrees/feature");
        let linked = root.join("feature");
        std::fs::create_dir_all(&main).unwrap();
        std::fs::create_dir_all(&linked).unwrap();
        // What git writes in a linked worktree.
        std::fs::write(linked.join(".git"), format!("gitdir: {}\n", main.display())).unwrap();

        assert_eq!(git_dir(&linked).as_deref(), Some(main.as_path()));

        // And the main repository, whose `.git` is a real directory.
        let plain = root.join("repo");
        assert_eq!(git_dir(&plain), Some(plain.join(".git")));

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn only_the_directories_git_knows_are_watched() {
        // A real little repository: tracked code, and a heavy ignored folder.
        let root = std::env::temp_dir().join(format!("claudhub-tracked-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("src/inner")).unwrap();
        std::fs::create_dir_all(root.join("vendor/package/deep")).unwrap();
        std::fs::write(root.join("src/inner/code.rs"), "fn main() {}").unwrap();
        std::fs::write(root.join("vendor/package/deep/big.php"), "<?php").unwrap();
        std::fs::write(root.join(".gitignore"), "vendor/\n").unwrap();
        for args in [
            vec!["init", "-q"],
            vec!["config", "user.email", "t@example.com"],
            vec!["config", "user.name", "T"],
            vec!["add", "."],
        ] {
            std::process::Command::new("git")
                .arg("-C")
                .arg(&root)
                .args(&args)
                .output()
                .unwrap();
        }

        let dirs = watchable_directories(&root);
        let watched: Vec<&Path> = dirs.iter().map(|(p, _)| p.as_path()).collect();

        assert!(
            watched.contains(&root.join("src/inner").as_path()),
            "a code folder must be watched: {watched:?}"
        );
        assert!(
            watched.contains(&root.join("src").as_path()),
            "the intermediate folders too"
        );
        assert!(
            !watched.iter().any(|p| p.starts_with(root.join("vendor"))),
            "nothing ignored must be watched: {watched:?}"
        );
        // No recursive watch on the working tree: that is what would bring back
        // the folders left out.
        assert!(
            dirs.iter()
                .filter(|(p, _)| !p.starts_with(root.join(".git")))
                .all(|(_, m)| matches!(m, RecursiveMode::NonRecursive)),
            "no recursive watch outside .git"
        );

        let _ = std::fs::remove_dir_all(&root);
    }

    /// The only test proving the whole chain works: a real filesystem watcher,
    /// a real write, and the path coming out at the other end. The two previous
    /// tests only validate the filter.
    #[test]
    fn a_real_write_reaches_the_receiver() {
        let dir = std::env::temp_dir().join(format!("claudhub-watch-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("temporary directory");

        let (watcher, changes) = Watcher::new().expect("watcher available");
        watcher.watch(&dir);

        let file = dir.join("written while watched.txt");

        // `watch` returns before the watch is set up — that is the whole point,
        // the interface thread must not wait for the thousands of system calls
        // that takes on a real project. The test therefore rewrites the file on
        // every round: as soon as the watch is in place, the next write is
        // seen.
        let deadline = std::time::Instant::now() + Duration::from_secs(10);
        let mut received = None;
        while std::time::Instant::now() < deadline {
            std::fs::write(&file, b"content").expect("write");
            std::thread::sleep(Duration::from_millis(100));
            if let Ok(batch) = changes.try_recv() {
                received = batch.into_iter().next();
                break;
            }
        }

        let _ = std::fs::remove_dir_all(&dir);
        assert_eq!(
            received.as_deref().and_then(Path::file_name),
            Some(file.file_name().unwrap()),
            "the write was not reported"
        );
    }
}
