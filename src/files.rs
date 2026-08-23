//! Reading, editing and filing a worktree's files.
//!
//! Everything touching the disk outside git: reading a file to edit it, writing
//! it, renames and deletions, and launching the external editor. Like the git
//! layer, nothing here may be called from the interface thread.
//!
//! **Writing is conditional.** An agent writes in the same files while we read
//! them: `expect` carries the digest of what we had in front of us, and the
//! write is refused if the file has changed since. It is the only way not to
//! erase an hour of an agent's work with a typo fix.

use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};

/// Past this, the built-in editor is not a good idea: `InputState` documents
/// fifty thousand lines, and a frozen window is a worse service than a
/// refusal.
pub const MAX_LINES: usize = 50_000;

/// What we read, and what is needed to check we are writing over it.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Content {
    pub text: String,
    /// Digest of the text read.
    ///
    /// It only serves to compare two reads within the same session: that is
    /// exactly the guarantee wanted — "has this file changed since I opened
    /// it?" — and it has no need to survive a restart.
    pub hash: u64,
}

/// A text's digest, for detecting concurrent writes.
///
/// FNV-1a written by hand, and not `DefaultHasher`: the digest is produced by
/// the worker and compared by the view, which will be two **binaries** once the
/// worker runs in the WSL server — and `DefaultHasher` promises nothing from
/// one process to the next. FNV-1a is defined by its two constants, and a test
/// pins a known value so no change goes unnoticed.
pub fn digest(text: &str) -> u64 {
    const OFFSET: u64 = 0xcbf2_9ce4_8422_2325;
    const PRIME: u64 = 0x0000_0100_0000_01b3;
    let mut hash = OFFSET;
    for byte in text.bytes() {
        hash ^= u64::from(byte);
        hash = hash.wrapping_mul(PRIME);
    }
    hash
}

/// True if this content is not text.
///
/// The null byte is git's own criterion, and on the first block: an executable
/// has one in its first bytes, a text never does.
pub fn looks_binary(bytes: &[u8]) -> bool {
    bytes.iter().take(8000).any(|byte| *byte == 0)
}

/// Past this, an image is not previewed: the bytes cross the wire whole — the
/// server reads the file and the window paints it — and a hundred-megabyte
/// texture answers a question nobody asked. The external editor is the way
/// out, as it is for a file over `MAX_LINES`.
pub const MAX_IMAGE_BYTES: u64 = 32 * 1024 * 1024;

/// The picture formats the preview knows how to paint.
///
/// **Ours and not gpui's**: this module belongs to the core, which the headless
/// server builds without the `ui` feature — and it is the server that reads the
/// file. `ui::explorer` translates it into `gpui::ImageFormat`, which is one
/// match and the only place the two vocabularies meet.
///
/// The list is gpui's, minus what it cannot decode. SVG is in it: gpui rasters
/// it through its own renderer, from the bytes, exactly like the others.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum Picture {
    Png,
    Jpeg,
    Gif,
    Webp,
    Bmp,
    Ico,
    Tiff,
    Svg,
}

impl Picture {
    /// What the preview's footer calls it.
    pub fn label(self) -> &'static str {
        match self {
            Self::Png => "PNG",
            Self::Jpeg => "JPEG",
            Self::Gif => "GIF",
            Self::Webp => "WEBP",
            Self::Bmp => "BMP",
            Self::Ico => "ICO",
            Self::Tiff => "TIFF",
            Self::Svg => "SVG",
        }
    }
}

/// The picture a file name announces, if it announces one.
///
/// **By extension and not by sniffing the bytes**, and that is deliberate: the
/// question is asked by the interface, before anything has been read, to know
/// which command to send — and a name is all it has. A `.png` holding something
/// else fails to decode, which is the honest outcome of a lie in the name.
pub fn picture_of(path: &Path) -> Option<Picture> {
    let extension = path.extension()?.to_str()?.to_ascii_lowercase();
    Some(match extension.as_str() {
        "png" => Picture::Png,
        "jpg" | "jpeg" => Picture::Jpeg,
        "gif" => Picture::Gif,
        "webp" => Picture::Webp,
        "bmp" => Picture::Bmp,
        "ico" => Picture::Ico,
        "tif" | "tiff" => Picture::Tiff,
        "svg" => Picture::Svg,
        _ => return None,
    })
}

/// An image as it travels: its format, and the bytes untouched.
///
/// Undecoded, and the decoding belongs at the far end: gpui caches a decoded
/// image by the digest of these very bytes, so decoding here would be work done
/// twice and a frame's worth of pixels on the wire instead of a file.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Image {
    pub kind: Picture,
    pub bytes: Vec<u8>,
}

/// Reads a file from the worktree to preview it as an image.
pub fn read_image(worktree: &Path, path: &Path) -> Result<Image> {
    let kind = picture_of(path).with_context(|| format!("{} is not an image", path.display()))?;
    let full = worktree.join(path);
    // Asked of the metadata rather than of what was read: refusing after having
    // loaded a hundred megabytes into memory refuses nothing.
    let size = std::fs::metadata(&full)
        .with_context(|| format!("cannot read {}", full.display()))?
        .len();
    if size > MAX_IMAGE_BYTES {
        bail!(
            "{} is over {} MB: open it in your editor",
            path.display(),
            MAX_IMAGE_BYTES / (1024 * 1024)
        );
    }
    let bytes = std::fs::read(&full).with_context(|| format!("cannot read {}", full.display()))?;
    Ok(Image { kind, bytes })
}

/// Reads a file from the worktree for editing.
pub fn read(worktree: &Path, path: &Path) -> Result<Content> {
    let full = worktree.join(path);
    let bytes = std::fs::read(&full).with_context(|| format!("cannot read {}", full.display()))?;
    if looks_binary(&bytes) {
        bail!("{} is a binary file", path.display());
    }
    let text = String::from_utf8(bytes)
        .map_err(|_| anyhow::anyhow!("{} is not UTF-8 text", path.display()))?;
    // Counted before returning: it is the only chance to refuse before the
    // window has started colouring five hundred thousand lines.
    if text.lines().count() > MAX_LINES {
        bail!(
            "{} is over {MAX_LINES} lines: open it in your editor",
            path.display()
        );
    }
    let hash = digest(&text);
    Ok(Content { text, hash })
}

/// Writes a file, unless somebody else has changed it since.
///
/// `expect` is the digest of what we had read. A vanished file counts as a
/// change: recreating it through a save would bring back what a `git restore`
/// had just removed.
pub fn write(worktree: &Path, path: &Path, text: &str, expect: Option<u64>) -> Result<()> {
    write_at(&worktree.join(path), text, expect)
}

/// The same conditional write, on an absolute path.
///
/// What is written outside the worktree has no relative path to give it: a
/// vault's `TODO.md` lives elsewhere, and the digest guard serves it just as
/// well — it is the agent writing in it while we watch.
pub fn write_at(full: &Path, text: &str, expect: Option<u64>) -> Result<()> {
    if let Some(expected) = expect {
        let current = std::fs::read_to_string(full).map(|text| digest(&text)).ok();
        if current != Some(expected) {
            bail!(
                "{} has changed since it was opened: reload it before saving",
                full.display()
            );
        }
    }
    if let Some(parent) = full.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(full, text).with_context(|| format!("cannot write {}", full.display()))
}

/// What we do to a file from the explorer.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum Op {
    Rename {
        from: PathBuf,
        to: PathBuf,
    },
    Delete {
        path: PathBuf,
    },
    NewFile {
        path: PathBuf,
    },
    NewDir {
        path: PathBuf,
    },
    /// Copies something that comes from outside the worktree into it — what a
    /// drop from the desktop's file manager is.
    ///
    /// `from` is **absolute and of the machine the workers run on**: on
    /// Windows the view translates it before sending, like the target of a CSV
    /// export. `to` is relative to the worktree, the name included.
    Import {
        from: PathBuf,
        to: PathBuf,
    },
}

impl Op {
    /// The path the operation acts on, for the result message.
    pub fn target(&self) -> &Path {
        match self {
            Self::Rename { to, .. } | Self::Import { to, .. } => to,
            Self::Delete { path } | Self::NewFile { path } | Self::NewDir { path } => path,
        }
    }
}

/// Where something dropped on a folder lands, relative to the worktree.
///
/// `dir` is a folder of the worktree, empty for its root. `None` for a source
/// with no name of its own — `/`, or a path ending in `..`: there would be
/// nothing to call the copy.
pub fn drop_target(dir: &Path, source: &Path) -> Option<PathBuf> {
    let name = source.file_name()?;
    if Path::new(name).components().count() != 1 {
        return None;
    }
    Some(dir.join(name))
}

/// Runs an explorer operation.
///
/// Paths are **brought back inside the worktree**: a `../` typed in a rename
/// dialog would otherwise leave the repository, and a deletion there would do
/// damage git does not recover.
pub fn apply(worktree: &Path, op: &Op) -> Result<()> {
    match op {
        Op::Rename { from, to } => {
            let (from, to) = (inside(worktree, from)?, inside(worktree, to)?);
            if to.exists() {
                bail!("{} already exists", to.display());
            }
            if let Some(parent) = to.parent() {
                std::fs::create_dir_all(parent)?;
            }
            std::fs::rename(&from, &to).with_context(|| format!("cannot rename {}", from.display()))
        }
        Op::Delete { path } => {
            let full = inside(worktree, path)?;
            let result = if full.is_dir() {
                std::fs::remove_dir_all(&full)
            } else {
                std::fs::remove_file(&full)
            };
            result.with_context(|| format!("cannot delete {}", full.display()))
        }
        Op::NewFile { path } => {
            let full = inside(worktree, path)?;
            if full.exists() {
                bail!("{} already exists", path.display());
            }
            if let Some(parent) = full.parent() {
                std::fs::create_dir_all(parent)?;
            }
            std::fs::write(&full, "").with_context(|| format!("cannot create {}", full.display()))
        }
        Op::NewDir { path } => {
            let full = inside(worktree, path)?;
            std::fs::create_dir_all(&full)
                .with_context(|| format!("cannot create {}", full.display()))
        }
        Op::Import { from, to } => {
            let to = inside(worktree, to)?;
            if !from.is_absolute() {
                bail!("{}: an import comes from an absolute path", from.display());
            }
            // Nothing is overwritten, ever: a drop is one gesture of the hand,
            // and the file underneath may be an hour of work. The refusal is
            // said, and renaming is one right click away.
            if to.exists() {
                bail!("{} already exists", to.display());
            }
            // Copying a folder into itself never ends.
            if to.starts_with(from) {
                bail!("{} is inside {}", to.display(), from.display());
            }
            if let Some(parent) = to.parent() {
                std::fs::create_dir_all(parent)?;
            }
            copy_into(from, &to).with_context(|| format!("cannot copy {} here", from.display()))
        }
    }
}

/// Copies a file or a whole folder to a path that does not exist yet.
///
/// Written here rather than pulled in as a crate: the recursion is six lines,
/// and what a copy has to answer — a symlink, a socket, a device — is a
/// decision we would rather make ourselves. Anything that is neither a file nor
/// a folder is **skipped**, silently: a drop of a folder that holds a socket is
/// still a drop that should land.
fn copy_into(from: &Path, to: &Path) -> std::io::Result<()> {
    let kind = std::fs::metadata(from)?;
    if kind.is_dir() {
        std::fs::create_dir_all(to)?;
        for entry in std::fs::read_dir(from)? {
            let entry = entry?;
            copy_into(&entry.path(), &to.join(entry.file_name()))?;
        }
        Ok(())
    } else if kind.is_file() {
        std::fs::copy(from, to).map(|_| ())
    } else {
        Ok(())
    }
}

/// Everything directly under a directory git has declared it does not descend
/// into, folders first.
///
/// A `readdir` and not a git command, and it is **exact**: git never walks into
/// an excluded directory, so nothing under one can be re-included — asking git
/// about `vendor/` answers `vendor/`. Reading one level costs two milliseconds
/// on the seven hundred entries of a `node_modules/`, where enumerating what is
/// under it costs a second.
///
/// This is the one place the explorer touches the disk, and the rule it seems
/// to break says why: what the tree must never do is **walk** forty thousand
/// directories to find the seven hundred that carry code. One level, on a
/// gesture, inside a folder git has given up on, is the opposite of that.
pub fn read_dir(worktree: &Path, dir: &Path) -> Result<(Vec<PathBuf>, Vec<PathBuf>)> {
    let full = inside(worktree, dir)?;
    let mut dirs = Vec::new();
    let mut files = Vec::new();
    for entry in std::fs::read_dir(&full)
        .with_context(|| format!("reading {}", full.display()))?
        .flatten()
    {
        let path = dir.join(entry.file_name());
        // `file_type` comes from the directory entry on Linux and costs no
        // extra syscall; a symlink is left where it is rather than followed.
        match entry.file_type() {
            Ok(kind) if kind.is_dir() => dirs.push(path),
            _ => files.push(path),
        }
    }
    dirs.sort_unstable();
    files.sort_unstable();
    Ok((dirs, files))
}

/// Resolves a relative path inside the worktree, refusing to leave it.
fn inside(worktree: &Path, path: &Path) -> Result<PathBuf> {
    if path.is_absolute() {
        bail!("{}: an absolute path is not accepted here", path.display());
    }
    if path
        .components()
        .any(|c| matches!(c, std::path::Component::ParentDir))
    {
        bail!("{}: cannot leave the worktree", path.display());
    }
    Ok(worktree.join(path))
}

/// Splits an external editor's command, substituting the file and the line.
///
/// `{path}` and `{line}` are replaced everywhere they appear, including in the
/// middle of an argument — `code -g {path}:{line}` and `zed {path}:{line}` are
/// written that way. Without `{path}`, the path is appended at the end: that is
/// what a bare `vim` expects.
pub fn editor_command(template: &str, path: &Path, line: usize) -> Option<(String, Vec<String>)> {
    let template = template.trim();
    if template.is_empty() {
        return None;
    }
    let path = path.display().to_string();
    let mut parts: Vec<String> = crate::cmdline::split_command(template)
        .into_iter()
        .map(|part| {
            part.replace("{path}", &path)
                .replace("{line}", &line.to_string())
        })
        .collect();
    if !template.contains("{path}") {
        parts.push(path);
    }
    let mut parts = parts.into_iter();
    let program = parts.next()?;
    Some((program, parts.collect()))
}

/// Launches the external editor and returns immediately.
///
/// Without waiting: a graphical editor only returns when it is closed, and a
/// `vim` launched into the void never would. The process is detached, its
/// outputs thrown away — it is a separate program, not a command whose result
/// we read.
pub fn open_external(worktree: &Path, template: &str, path: &Path, line: usize) -> Result<String> {
    let Some((program, args)) = editor_command(template, &worktree.join(path), line) else {
        bail!("no external editor configured");
    };
    std::process::Command::new(&program)
        .args(&args)
        .current_dir(worktree)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .with_context(|| format!("{program} could not be launched"))?;
    Ok(program)
}

// — The notes folder ————————————————————————————————————————————————

/// What `sync_notes` is allowed to erase, by the frontmatter's mark.
///
/// A vault folder contains its owner's notes, and a deleted review note must
/// not take the week's journal with it. The value counts as much as the key:
/// `claudhub: todo` carries our mark but **does not belong to us** — it is the
/// agent, or its reader, that keeps it, and writing a note must not take the
/// running task list away.
const OURS: [&str; 2] = ["\nclaudhub: note", "\nclaudhub: review"];

/// True for a file Claudhub writes whole, and may therefore erase.
fn is_ours(text: &str) -> bool {
    text.starts_with("---") && OURS.iter().any(|mark| text.contains(mark))
}

/// Writes a vault file, or erases it if its text is empty.
///
/// Empty means absent: an empty file in a vault is a shell nobody opens twice,
/// and the free note of a worktree that is emptied should disappear rather than
/// sit in the way of a search.
///
/// The digest guards the erasure as it guards the write: the file may have
/// changed since we read it.
pub fn write_vault_file(path: &Path, text: &str, expect: Option<u64>) -> Result<()> {
    if !text.trim().is_empty() {
        return write_at(path, text, expect);
    }
    match std::fs::read_to_string(path) {
        Ok(current) => {
            if expect.is_some_and(|expected| digest(&current) != expected) {
                bail!(
                    "{} has changed since it was opened: reload it before saving",
                    path.display()
                );
            }
            std::fs::remove_file(path).with_context(|| format!("cannot delete {}", path.display()))
        }
        // Nothing to erase: that is the state we were asking for.
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e).with_context(|| format!("reading {}", path.display())),
    }
}

/// The Markdown files of a notes folder, name and content.
///
/// A missing folder is not an error: it is the state of a worktree that has not
/// been annotated yet.
pub fn read_notes(dir: &Path) -> Result<Vec<(String, String)>> {
    let entries = match std::fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(e).with_context(|| format!("reading {}", dir.display())),
    };
    let mut out = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("md") {
            continue;
        }
        let Ok(text) = std::fs::read_to_string(&path) else {
            continue;
        };
        let Some(name) = path.file_name().map(|n| n.to_string_lossy().into_owned()) else {
            continue;
        };
        out.push((name, text));
    }
    out.sort();
    Ok(out)
}

/// Aligns the folder on this list, and on it alone.
///
/// Three rules, and each is paid for if forgotten:
///
/// - **We do not rewrite what has not changed.** A vault is often synced, and
///   touching a file's date on every click would make the sync work for
///   nothing.
/// - **We only erase what we write whole** (`is_ours`). The folder may contain
///   its owner's notes, and a `TODO.md` the agent keeps up to date.
/// - **What is no longer in the list disappears**, including under another
///   name: that is how a deleted note goes away, and how a file renamed in the
///   vault does not leave a duplicate behind.
pub fn sync_notes(dir: &Path, files: &[(String, String)]) -> Result<()> {
    std::fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
    for (name, content) in files {
        let path = dir.join(name);
        if std::fs::read_to_string(&path).is_ok_and(|old| old == *content) {
            continue;
        }
        std::fs::write(&path, content).with_context(|| format!("writing {}", path.display()))?;
    }
    for (name, text) in read_notes(dir)? {
        if files.iter().any(|(kept, _)| *kept == name) {
            continue;
        }
        if is_ours(&text) {
            let _ = std::fs::remove_file(dir.join(name));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The digest is compared between two processes — the view and the server —
    /// so it has to be identical from one binary to the next. These values are
    /// FNV-1a 64-bit's; if this test breaks, the algorithm has changed, and
    /// every digest a running session holds is a lie.
    #[test]
    fn the_digest_is_stable_across_binaries() {
        assert_eq!(digest(""), 0xcbf2_9ce4_8422_2325);
        assert_eq!(digest("a"), 0xaf63_dc4c_8601_ec8c);
        assert_eq!(digest("Claudhub\n"), digest("Claudhub\n"));
        assert_ne!(digest("Claudhub\n"), digest("Claudhub"));
    }

    /// One level, folders apart from files, and never a step outside — the
    /// path comes from a tree row, and the tree is what a listing built.
    #[test]
    fn one_level_of_a_directory_comes_back_sorted_and_split() {
        let root = std::env::temp_dir().join(format!("claudhub-readdir-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("vendor/laravel")).unwrap();
        std::fs::create_dir_all(root.join("vendor/psr")).unwrap();
        std::fs::write(root.join("vendor/autoload.php"), "<?php").unwrap();

        let (dirs, files) = read_dir(&root, Path::new("vendor")).unwrap();
        assert_eq!(
            dirs,
            vec![PathBuf::from("vendor/laravel"), PathBuf::from("vendor/psr")]
        );
        assert_eq!(files, vec![PathBuf::from("vendor/autoload.php")]);
        // Nothing deeper: opening `vendor/` says what is directly in it, which
        // is the whole point of stopping there.
        assert!(!files.iter().any(|p| p.ends_with("composer.json")));

        assert!(
            read_dir(&root, Path::new("../elsewhere")).is_err(),
            "a path that leaves the worktree is refused"
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    /// The whole chain of the notes folder: what we write reads back, what is
    /// no longer in the list goes away, and what we did not write stays. The
    /// only test in this module that touches the disk, like the watcher's: it
    /// is the only way to prove the erasure.
    #[test]
    fn the_notes_folder_keeps_what_is_not_ours() {
        let dir = std::env::temp_dir().join(format!("claudhub-notes-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);

        let ours = (
            "0001 a.rs.md".to_string(),
            "---\nclaudhub: note\n---\n\nx\n".to_string(),
        );
        sync_notes(&dir, std::slice::from_ref(&ours)).expect("write");
        std::fs::write(dir.join("Journal.md"), "---\ntags: [me]\n---\n\nMine.\n").unwrap();
        // Our mark, but not our file: the task list belongs to the agent that
        // ticks it, and a deleted note does not take it away.
        std::fs::write(dir.join("TODO.md"), "---\nclaudhub: todo\n---\n\n- [ ] x\n").unwrap();

        let read = read_notes(&dir).expect("read");
        assert_eq!(read.len(), 3);

        // The note goes away, the journal and the list stay.
        sync_notes(&dir, &[]).expect("erase");
        let read = read_notes(&dir).expect("read");
        let names: Vec<&str> = read.iter().map(|(name, _)| name.as_str()).collect();
        assert_eq!(names, ["Journal.md", "TODO.md"]);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn an_editor_command_places_the_file_and_the_line() {
        let path = Path::new("/p/src/main.rs");
        assert_eq!(
            editor_command("code -g {path}:{line}", path, 42),
            Some(("code".into(), vec!["-g".into(), "/p/src/main.rs:42".into()]))
        );
        assert_eq!(
            editor_command("phpstorm --line {line} {path}", path, 7),
            Some((
                "phpstorm".into(),
                vec!["--line".into(), "7".into(), "/p/src/main.rs".into()]
            ))
        );
        // Without `{path}`, the path goes at the end: that is what an editor
        // knowing nothing of lines expects.
        assert_eq!(
            editor_command("gedit", path, 1),
            Some(("gedit".into(), vec!["/p/src/main.rs".into()]))
        );
        assert_eq!(editor_command("   ", path, 1), None);
    }

    #[test]
    fn an_editor_path_may_contain_a_space() {
        // The flaw `split_command` fixes, here too.
        assert_eq!(
            editor_command(r#""/opt/my editor/bin/ed" {path}"#, Path::new("/a b.rs"), 1),
            Some(("/opt/my editor/bin/ed".into(), vec!["/a b.rs".into()]))
        );
    }

    /// The name decides, and it decides before anything is read.
    #[test]
    fn a_picture_is_recognised_by_its_extension() {
        assert_eq!(picture_of(Path::new("assets/logo.PNG")), Some(Picture::Png));
        assert_eq!(picture_of(Path::new("a/b/photo.jpeg")), Some(Picture::Jpeg));
        // Text that is drawn: read as a picture, and gpui rasters it.
        assert_eq!(picture_of(Path::new("icons/x.svg")), Some(Picture::Svg));
        assert_eq!(picture_of(Path::new("src/main.rs")), None);
        // No extension at all, and an extension that only looks like one.
        assert_eq!(picture_of(Path::new("Makefile")), None);
        assert_eq!(picture_of(Path::new("archive.png.gz")), None);
    }

    #[test]
    fn a_path_cannot_leave_the_worktree() {
        let worktree = Path::new("/p/repo");
        assert!(inside(worktree, Path::new("src/main.rs")).is_ok());
        assert!(inside(worktree, Path::new("../elsewhere")).is_err());
        assert!(inside(worktree, Path::new("/etc/passwd")).is_err());
    }

    #[test]
    fn what_is_dropped_keeps_its_own_name() {
        assert_eq!(
            drop_target(Path::new("src"), Path::new("/tmp/logo.png")),
            Some(PathBuf::from("src/logo.png"))
        );
        // The worktree's root is the empty folder.
        assert_eq!(
            drop_target(Path::new(""), Path::new("/tmp/assets")),
            Some(PathBuf::from("assets"))
        );
        // Nothing to call the copy.
        assert_eq!(drop_target(Path::new("src"), Path::new("/")), None);
        assert_eq!(drop_target(Path::new("src"), Path::new("/tmp/..")), None);
    }

    /// An import copies, it never overwrites, and a folder comes whole.
    #[test]
    fn an_import_copies_a_tree_and_spares_what_is_there() {
        let root = std::env::temp_dir().join(format!("claudhub-import-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let (worktree, outside) = (root.join("repo"), root.join("elsewhere"));
        std::fs::create_dir_all(worktree.join("src")).unwrap();
        std::fs::create_dir_all(outside.join("assets/img")).unwrap();
        std::fs::write(outside.join("assets/a.css"), "a{}").unwrap();
        std::fs::write(outside.join("assets/img/logo.svg"), "<svg/>").unwrap();

        apply(
            &worktree,
            &Op::Import {
                from: outside.join("assets"),
                to: PathBuf::from("src/assets"),
            },
        )
        .expect("import");
        assert_eq!(
            std::fs::read_to_string(worktree.join("src/assets/img/logo.svg")).unwrap(),
            "<svg/>"
        );

        // What is already there is not touched, and the refusal is said.
        std::fs::write(worktree.join("src/assets/a.css"), "mine").unwrap();
        assert!(apply(
            &worktree,
            &Op::Import {
                from: outside.join("assets/a.css"),
                to: PathBuf::from("src/assets/a.css"),
            },
        )
        .is_err());
        assert_eq!(
            std::fs::read_to_string(worktree.join("src/assets/a.css")).unwrap(),
            "mine"
        );

        // And a drop cannot leave the worktree.
        assert!(apply(
            &worktree,
            &Op::Import {
                from: outside.join("assets/a.css"),
                to: PathBuf::from("../a.css"),
            },
        )
        .is_err());

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn a_null_byte_gives_away_a_binary() {
        assert!(looks_binary(b"\x7fELF\0\0"));
        assert!(!looks_binary(b"fn main() {}\n"));
        // Past the first block we do not look: that is git's criterion.
        let mut long = vec![b'a'; 9000];
        long.push(0);
        assert!(!looks_binary(&long));
    }

    #[test]
    fn writing_refuses_when_the_file_changed_underneath() {
        let dir = std::env::temp_dir().join(format!("claudhub-files-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = Path::new("note.txt");
        std::fs::write(dir.join(path), "one").unwrap();

        let read_back = read(&dir, path).unwrap();
        assert_eq!(read_back.text, "one");
        // An agent writes while we edit.
        std::fs::write(dir.join(path), "two").unwrap();
        assert!(write(&dir, path, "three", Some(read_back.hash)).is_err());
        // And the file has not moved: that is the whole point.
        assert_eq!(std::fs::read_to_string(dir.join(path)).unwrap(), "two");
        // With no expectation, the write goes through.
        assert!(write(&dir, path, "three", None).is_ok());

        let _ = std::fs::remove_dir_all(&dir);
    }
}
