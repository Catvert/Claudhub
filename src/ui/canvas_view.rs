//! The home screen's notes: files on disk (`crate::canvas`) shown as nodes,
//! written in by the hand and by agents alike.
//!
//! **The disk is the source of truth.** What an agent writes in
//! `.claudhub/notes/` appears at the next reading; what the hand types goes
//! back to the file after a pause, and only if the file is still the one read
//! — an agent may be writing it too, and a blind write would erase its work.
//! The view keeps a copy to paint from, and nothing else.
//!
//! **Shared or private is where the file is**: `.claudhub/notes/` in the
//! checkout, versioned with the code, or the worktree's vault folder. The
//! eye in a note's head moves it from one to the other.

use std::path::{Path, PathBuf};

use gpui_kit::component::{
    button::{Button, ButtonVariants as _},
    h_flex,
    input::{EditorState, InputEvent},
    menu::DropdownMenu as _,
    v_flex, ActiveTheme, Disableable as _, Sizable as _,
};
use gpui_kit::{
    div, prelude::*, px, AnyElement, App, Context, Entity, Focusable as _, MouseButton,
    SharedString, Window,
};

use super::app::ClaudhubApp;
use super::icons::icon;
use super::overview::{self, Node};
use crate::canvas::{self, Anchor};
use crate::runtime::Cmd;
use crate::tr;

/// A node file as the view holds it: where it is, what it says, and the
/// digest of the text read — the guard of the next write.
#[derive(Debug, Clone)]
pub(crate) struct CanvasEntry {
    pub path: PathBuf,
    pub private: bool,
    pub digest: u64,
    /// The file's stamp as read (`files::read_notes_since`): what the next
    /// reading is asked against. Zero once the hand has written the file —
    /// its text is then read again, whatever the stamp says.
    pub stamp: u64,
    /// Set by `set_node` only: `body` and `review` are read from it.
    pub node: canvas::Node,
    /// A picture dropped in the folder with no node file: shown as a
    /// diagram, titled by its name, with nothing to edit.
    pub standalone: bool,
    /// The body as the rendering takes it: shared, not copied at each frame.
    pub body: SharedString,
    /// A review's findings, read once per text and not at each frame.
    pub review: Option<std::rc::Rc<Review>>,
}

/// A review's body read (`canvas::findings`): its summary, its findings, and
/// how many are still open.
#[derive(Debug)]
pub(crate) struct Review {
    pub summary: SharedString,
    pub findings: Vec<canvas::Finding>,
    pub open: usize,
}

impl CanvasEntry {
    fn new(
        path: PathBuf,
        private: bool,
        digest: u64,
        stamp: u64,
        node: canvas::Node,
        standalone: bool,
    ) -> Self {
        let (body, review) = read_node(&node);
        Self {
            path,
            private,
            digest,
            stamp,
            node,
            standalone,
            body,
            review,
        }
    }

    /// Replaces the node, and what is read of it with it.
    fn set_node(&mut self, node: canvas::Node) {
        (self.body, self.review) = read_node(&node);
        self.node = node;
    }
}

/// What the rendering reads of a node: its body, and a review's findings.
fn read_node(node: &canvas::Node) -> (SharedString, Option<std::rc::Rc<Review>>) {
    let review = (node.kind == canvas::Kind::Review).then(|| {
        let (summary, findings) = canvas::findings(&node.body);
        let open = findings.iter().filter(|f| !f.resolved()).count();
        std::rc::Rc::new(Review {
            summary: summary.into(),
            findings,
            open,
        })
    });
    (SharedString::from(node.body.clone()), review)
}

/// What the readings of the node files leave beside the entries.
#[derive(Debug, Default)]
pub(crate) struct CanvasReads {
    /// The Markdown files of each worktree's folders that do not read as a
    /// node, by stamp: known too, or their text would cross at every reading.
    skipped: std::collections::HashMap<PathBuf, std::collections::HashMap<PathBuf, u64>>,
    /// Where each entry is: its worktree, and its rank in the list — the
    /// rendering looks a node up by its path, several times per frame.
    at: std::collections::HashMap<PathBuf, (PathBuf, usize)>,
    /// The digest of the last context sheet sent, by the sheet's path: one
    /// that has not changed is not sent again. In memory only: a restart
    /// sends each once.
    contexts: std::collections::HashMap<PathBuf, u64>,
}

impl CanvasReads {
    /// The server is gone: a new one may stand on another disk.
    pub(super) fn forget_sent(&mut self) {
        self.contexts.clear();
    }
}

/// A diagram's picture: the stamp it was read at, and the decoded image —
/// kept while a newer one is on its way, so a node never blinks empty.
pub(crate) struct CanvasPicture {
    pub stamp: u64,
    pub image: Option<std::sync::Arc<gpui_kit::Image>>,
}

/// A note open for writing.
pub(crate) struct NoteEditor {
    pub editor: Entity<EditorState>,
    _changes: gpui_kit::Subscription,
    /// Counts the keystrokes: a save waits for a pause, and a pause is a
    /// count that has not moved.
    generation: u64,
}

/// What a new note hangs from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum Hang {
    /// The repository, by its main checkout.
    Repo(PathBuf),
    Worktree(PathBuf),
}

/// The context sheet's name, in a worktree's vault folder — see `ui::context`.
pub(crate) const CONTEXT: &str = "context.md";

/// How long the hand has to rest before what it typed goes to the disk.
const SAVE_AFTER: std::time::Duration = std::time::Duration::from_millis(700);

/// Where a diagram's picture is: its `image:` beside the node file.
fn picture_of(entry: &CanvasEntry) -> Option<PathBuf> {
    let image = entry.node.image.as_deref()?;
    if entry.standalone {
        return Some(entry.path.clone());
    }
    let dir = entry.path.parent()?;
    Some(crate::wslpath::join(dir, image))
}

/// A note or a diagram an agent is writing: its terminal on the plane, and
/// the file it was asked for. Once the file is there and the agent's turn is
/// over, the terminal goes and the node takes its place.
pub(crate) struct Generation {
    pub terminal: gpui_kit::EntityId,
    pub worktree: PathBuf,
    pub target: PathBuf,
    /// A diagram's picture, which has to be there too.
    pub picture: Option<PathBuf>,
    pub ready: bool,
}

impl ClaudhubApp {
    /// Asks the workers for these worktrees' node files, against the stamps
    /// of what is already held: only what changed comes back with its text.
    pub(super) fn read_canvas(&mut self, worktrees: impl IntoIterator<Item = PathBuf>, cx: &App) {
        let now = std::time::Instant::now();
        for worktree in worktrees {
            let mut dirs = vec![(canvas::shared_dir(&worktree), false)];
            if let Some(vault) = self.notes_dir(&worktree, cx) {
                dirs.push((canvas::private_dir(&vault), true));
            }
            // What is known only while the entries are there: stamps without
            // their entries would leave the files out for good.
            let known: Vec<(PathBuf, u64)> = match self.canvas.get(&worktree) {
                Some(entries) => entries
                    .iter()
                    .filter(|entry| !entry.standalone && entry.stamp != 0)
                    .map(|entry| (entry.path.clone(), entry.stamp))
                    .chain(
                        self.canvas_reads
                            .skipped
                            .get(&worktree)
                            .into_iter()
                            .flatten()
                            .map(|(path, stamp)| (path.clone(), *stamp)),
                    )
                    .collect(),
                None => Vec::new(),
            };
            self.sweep.canvas_asked(worktree.clone(), now);
            self.git.send(Cmd::ReadCanvas {
                worktree,
                dirs,
                known,
            });
        }
    }

    /// The sweep's reading: the worktrees whose previous reading has not
    /// answered yet are left for the next pass — see `ui::sweep`.
    pub(super) fn sweep_canvas(&mut self, worktrees: Vec<PathBuf>, cx: &App) {
        let now = std::time::Instant::now();
        let free: Vec<PathBuf> = worktrees
            .into_iter()
            .filter(|worktree| self.sweep.canvas_free(worktree, now))
            .collect();
        self.read_canvas(free, cx);
    }

    /// A worktree's node files, read — the text of those that changed only.
    /// Nothing changed, nothing is redrawn: `false`, and no notification.
    /// A note just created is opened for writing once it is there to open.
    pub(super) fn canvas_read(
        &mut self,
        worktree: PathBuf,
        files: Vec<(PathBuf, bool, u64, Option<String>)>,
        pictures: Vec<(PathBuf, bool, u64)>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        self.sweep.canvas_answered(&worktree);
        // A first reading is news even when it finds nothing.
        let mut changed = !self.canvas.contains_key(&worktree);
        let mut before: std::collections::HashMap<PathBuf, CanvasEntry> = self
            .canvas
            .remove(&worktree)
            .unwrap_or_default()
            .into_iter()
            .map(|entry| (entry.path.clone(), entry))
            .collect();
        let skipped_before = self
            .canvas_reads
            .skipped
            .remove(&worktree)
            .unwrap_or_default();
        let mut skipped = std::collections::HashMap::new();
        let mut entries: Vec<CanvasEntry> = Vec::new();
        for (path, private, stamp, text) in files {
            let held = before.remove(&path).filter(|entry| !entry.standalone);
            let Some(text) = text else {
                // Unchanged since the stamp given: what was held stays.
                match held {
                    Some(entry) => entries.push(entry),
                    None if skipped_before.contains_key(&path) => {
                        skipped.insert(path, stamp);
                    }
                    // Neither held nor skipped: not given, so not answered
                    // for — the next reading asks for its text.
                    None => {}
                }
                continue;
            };
            let digest = crate::files::digest(&text);
            match held {
                // The same text — read again for a young stamp, or the
                // hand's own write come back.
                Some(mut entry) if entry.digest == digest => {
                    entry.stamp = stamp;
                    entries.push(entry);
                }
                held => {
                    changed |= held.is_some();
                    match canvas::parse(&text) {
                        Some(node) => {
                            changed = true;
                            entries
                                .push(CanvasEntry::new(path, private, digest, stamp, node, false));
                        }
                        None => {
                            skipped.insert(path, stamp);
                        }
                    }
                }
            }
        }
        // The pictures the diagrams name, and the SVGs no node names — each
        // a node of its own, titled by its file.
        let named: Vec<PathBuf> = entries.iter().filter_map(picture_of).collect();
        for (path, private, _) in &pictures {
            let svg = path
                .extension()
                .is_some_and(|ext| ext.eq_ignore_ascii_case("svg"));
            if svg && !named.contains(path) {
                if let Some(entry) = before
                    .remove(path)
                    .filter(|entry| entry.standalone && entry.private == *private)
                {
                    entries.push(entry);
                    continue;
                }
                changed = true;
                let title = path
                    .file_stem()
                    .map(|stem| stem.to_string_lossy().into_owned());
                let image = path
                    .file_name()
                    .map(|name| name.to_string_lossy().into_owned());
                entries.push(CanvasEntry::new(
                    path.clone(),
                    *private,
                    0,
                    0,
                    canvas::Node {
                        kind: canvas::Kind::Diagram,
                        anchor: Anchor::Worktree,
                        title,
                        author: None,
                        agent: None,
                        created: None,
                        status: None,
                        image,
                        body: String::new(),
                    },
                    true,
                ));
            }
        }
        // The bytes of what changed, and only of that.
        for picture in entries.iter().filter_map(picture_of) {
            let stamp = pictures
                .iter()
                .find(|(path, _, _)| *path == picture)
                .map_or(0, |(_, _, stamp)| *stamp);
            let known = self.canvas_pictures.get(&picture).map(|p| p.stamp);
            if known != Some(stamp) {
                self.canvas_pictures
                    .entry(picture.clone())
                    .or_insert(CanvasPicture { stamp, image: None })
                    .stamp = stamp;
                self.git.send(Cmd::ReadCanvasPicture {
                    worktree: worktree.clone(),
                    path: picture,
                });
            }
        }
        // The order they were written in: the names start with their date.
        entries.sort_by(|a, b| a.path.file_name().cmp(&b.path.file_name()));
        // A file an agent was asked for has arrived — its picture with it,
        // for a diagram: its terminal may go once the agent's turn is over.
        for generation in self
            .generations
            .iter_mut()
            .filter(|g| g.worktree == worktree)
        {
            let written = entries.iter().any(|entry| entry.path == generation.target);
            let drawn = generation
                .picture
                .as_ref()
                .is_none_or(|picture| pictures.iter().any(|(path, _, _)| path == picture));
            generation.ready = written && drawn;
        }
        // Whatever was held and is no longer there is gone.
        changed |= !before.is_empty();
        self.canvas_reads
            .at
            .retain(|_, (held, _)| *held != worktree);
        for (rank, entry) in entries.iter().enumerate() {
            self.canvas_reads
                .at
                .insert(entry.path.clone(), (worktree.clone(), rank));
        }
        self.canvas.insert(worktree.clone(), entries);
        self.canvas_reads.skipped.insert(worktree.clone(), skipped);
        // The worktree on show marks its reviews' findings in its diff.
        if changed && self.active.as_deref() == Some(worktree.as_path()) {
            self.refresh_note_marks(&worktree);
        }
        if let Some(created) = self.canvas_created.take() {
            if self.canvas_entry(&created).is_some() {
                self.overview_reveal = Some(Node::Note(created.clone()));
                self.edit_home_note(&created, window, cx);
            } else {
                self.canvas_created = Some(created);
            }
        }
        if changed {
            cx.notify();
        }
        changed
    }

    /// A picture's bytes arrived: decoded by gpui from them, once per stamp.
    pub(super) fn canvas_picture(
        &mut self,
        path: PathBuf,
        stamp: u64,
        bytes: Vec<u8>,
        cx: &mut Context<Self>,
    ) {
        let Some(kind) = crate::files::picture_of(&path) else {
            return;
        };
        let image = std::sync::Arc::new(gpui_kit::Image::from_bytes(
            super::preview::format_of(kind),
            bytes,
        ));
        self.canvas_pictures.insert(
            path,
            CanvasPicture {
                stamp,
                image: Some(image),
            },
        );
        cx.notify();
    }

    /// Asks for the request a generated note or diagram is written from.
    pub(super) fn ask_generation(
        &mut self,
        hang: Hang,
        kind: canvas::Kind,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let (title, placeholder) = match kind {
            canvas::Kind::Diagram => (
                tr!("generate-diagram-title"),
                tr!("generate-diagram-placeholder"),
            ),
            _ => (tr!("generate-note-title"), tr!("generate-note-placeholder")),
        };
        self.open_text_dialog(
            title,
            placeholder,
            window,
            cx,
            move |this, request, window, cx| {
                if !request.trim().is_empty() {
                    this.generate_node(hang.clone(), kind, request, window, cx);
                }
            },
        );
    }

    /// Starts the configured agent on a request, in a terminal of the
    /// worktree — on the plane, where it can be watched working — told the
    /// exact file to write, in the format a node reads.
    fn generate_node(
        &mut self,
        hang: Hang,
        kind: canvas::Kind,
        request: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(profile) = super::settings::Settings::global(cx)
            .terminal
            .default_profile()
            .cloned()
        else {
            self.announce_error(tr!("generate-no-agent"), cx);
            return;
        };
        let (worktree, anchor) = match hang {
            Hang::Repo(main) => (main, Anchor::Repo),
            Hang::Worktree(worktree) => (worktree, Anchor::Worktree),
        };
        let dir = canvas::shared_dir(&worktree);
        let now = chrono::Local::now();
        let taken: Vec<String> = self
            .canvas
            .get(&worktree)
            .into_iter()
            .flatten()
            .filter_map(|entry| {
                entry
                    .path
                    .file_name()
                    .map(|n| n.to_string_lossy().into_owned())
            })
            .collect();
        let name = canvas::file_name(&now.format("%Y-%m-%d-%H%M").to_string(), &request, &taken);
        let target = crate::wslpath::join(&dir, &name);
        let picture = (kind == canvas::Kind::Diagram)
            .then(|| crate::wslpath::join(&dir, name.replace(".md", ".svg")));
        let prompt = canvas::generation_prompt(
            kind,
            anchor,
            &target.display().to_string(),
            picture.as_ref().map(|p| p.display().to_string()).as_deref(),
            &now.to_rfc3339(),
            &request,
        );
        let mut launch = super::terminal_view::Launch::agent(&profile);
        if let Some((program, args)) = launch.command.as_mut() {
            // Writing the file it was asked for is the whole job: a
            // permission asked for it would stop the agent on a question.
            if super::revive::is_claude(program) {
                args.extend(["--permission-mode".to_string(), "acceptEdits".to_string()]);
            }
            args.push(prompt);
        }
        let before = self.terminals.len();
        self.open_terminal(&worktree, launch, window, cx);
        if self.terminals.len() == before {
            return;
        }
        let Some(opened) = self.terminals.last_mut() else {
            return;
        };
        opened.name = Some(match kind {
            canvas::Kind::Diagram => tr!("generate-diagram-running"),
            _ => tr!("generate-note-running"),
        });
        // A run with a purpose, not a terminal to bring back: kept, it would
        // be started again at the next launch, request and all.
        opened.relaunch = None;
        let terminal = opened.view.entity_id();
        self.overview_reveal = Some(Node::Terminal(terminal.as_u64()));
        self.generations.push(Generation {
            terminal,
            worktree,
            target,
            picture,
            ready: false,
        });
        cx.notify();
    }

    /// The agent's turn is over and its file is there: the terminal goes,
    /// and the node it wrote takes its place on screen. A terminal the hand
    /// closed first drops its wait.
    pub(super) fn settle_generations(
        &mut self,
        processes: &[crate::agent::ClaudeProcess],
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let mut done = Vec::new();
        self.generations.retain(|generation| {
            let Some(terminal) = self
                .terminals
                .iter()
                .find(|t| t.view.entity_id() == generation.terminal)
            else {
                return false;
            };
            if !generation.ready {
                return true;
            }
            let view = terminal.view.read(cx);
            let idle = view.has_exited()
                || view.child().is_some_and(|pid| {
                    processes
                        .iter()
                        .any(|p| p.pid == pid && p.status.as_deref() == Some("idle"))
                });
            if idle {
                done.push((generation.terminal, generation.target.clone()));
            }
            !idle
        });
        for (terminal, target) in done {
            self.close_terminal(terminal, window, cx);
            self.overview_reveal = Some(Node::Note(target));
        }
    }

    /// A node file was written: read the worktree again, and remember the
    /// note just made.
    pub(super) fn canvas_written(
        &mut self,
        worktree: PathBuf,
        created: Option<PathBuf>,
        cx: &mut Context<Self>,
    ) {
        if created.is_some() {
            self.canvas_created = created;
        }
        self.read_canvas([worktree], cx);
    }

    /// The worktree a node file was read for, and what it holds — by the
    /// index `canvas_read` keeps.
    pub(super) fn canvas_entry(&self, path: &Path) -> Option<(&PathBuf, &CanvasEntry)> {
        let (worktree, rank) = self.canvas_reads.at.get(path)?;
        let (worktree, entries) = self.canvas.get_key_value(worktree)?;
        entries
            .get(*rank)
            .filter(|entry| entry.path == path)
            .map(|entry| (worktree, entry))
    }

    /// `canvas_entry`, to change what the view holds of it.
    fn canvas_entry_mut(&mut self, path: &Path) -> Option<(PathBuf, &mut CanvasEntry)> {
        let (worktree, rank) = self.canvas_reads.at.get(path)?;
        let entry = self
            .canvas
            .get_mut(worktree)?
            .get_mut(*rank)
            .filter(|entry| entry.path == path)?;
        Some((worktree.clone(), entry))
    }

    /// A new note, in the checkout's shared folder: the worker names and
    /// signs it, and `canvas_read` opens it.
    pub(super) fn add_home_note(&mut self, hang: Hang, cx: &mut Context<Self>) {
        let (worktree, anchor) = match hang {
            Hang::Repo(main) => (main, Anchor::Repo),
            Hang::Worktree(worktree) => (worktree, Anchor::Worktree),
        };
        let dir = canvas::shared_dir(&worktree);
        self.git.send(Cmd::CreateCanvasNote {
            worktree,
            dir,
            anchor,
        });
        cx.notify();
    }

    /// Opens a note for writing: a field of the window's editor, whose text
    /// goes back to the file after each pause.
    pub(super) fn edit_home_note(
        &mut self,
        path: &Path,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(open) = self.note_editors.get(path) {
            super::dialogs::focus_field(&open.editor, window, cx);
            return;
        }
        let Some((_, entry)) = self.canvas_entry(path) else {
            return;
        };
        let body = entry.node.body.clone();
        let editor = cx.new(|cx| super::surface::plain_editor(window, cx));
        editor.update(cx, |editor, cx| editor.set_value(body, window, cx));
        let watched = path.to_path_buf();
        let changes = cx.subscribe(&editor, move |this, _, event: &InputEvent, cx| {
            if matches!(event, InputEvent::Change) {
                this.schedule_note_save(&watched, cx);
            }
        });
        super::dialogs::focus_field(&editor, window, cx);
        self.note_editors.insert(
            path.to_path_buf(),
            NoteEditor {
                editor,
                _changes: changes,
                generation: 0,
            },
        );
        cx.notify();
    }

    fn schedule_note_save(&mut self, path: &Path, cx: &mut Context<Self>) {
        let Some(open) = self.note_editors.get_mut(path) else {
            return;
        };
        open.generation += 1;
        let generation = open.generation;
        let path = path.to_path_buf();
        cx.spawn(async move |this, cx| {
            cx.background_executor().timer(SAVE_AFTER).await;
            let _ = this.update(cx, |this, cx| {
                let still = this
                    .note_editors
                    .get(&path)
                    .is_some_and(|open| open.generation == generation);
                if still {
                    this.save_home_note(&path, cx);
                }
            });
        })
        .detach();
    }

    /// Writes what the field holds, if it differs from the file — on the
    /// condition the file is still the one read.
    fn save_home_note(&mut self, path: &Path, cx: &mut Context<Self>) {
        let Some(open) = self.note_editors.get(path) else {
            return;
        };
        let body = open.editor.read(cx).value().to_string();
        let Some((worktree, entry)) = self.canvas_entry_mut(path) else {
            return;
        };
        if entry.node.body == body {
            return;
        }
        let mut node = entry.node.clone();
        node.body = body;
        let text = canvas::render(&node);
        let expect = entry.digest;
        // What the disk will hold once this lands: the next save's guard —
        // and the text read again at the next reading, which says whether
        // it did.
        entry.digest = crate::files::digest(&text);
        entry.stamp = 0;
        entry.set_node(node);
        self.git.send(Cmd::WriteCanvasFile {
            worktree,
            path: path.to_path_buf(),
            text,
            expect: Some(expect),
        });
    }

    /// Back to the rendered note, with what was typed on the disk.
    pub(super) fn done_home_note(
        &mut self,
        path: &Path,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.save_home_note(path, cx);
        self.note_editors.remove(path);
        self.focus_handle(cx).focus(window, cx);
        cx.notify();
    }

    /// Moves a note between the checkout and the vault: shared with the
    /// team, or kept to oneself. Same name, same text; its place on the plane
    /// follows it.
    fn toggle_note_privacy(&mut self, path: &Path, cx: &mut Context<Self>) {
        self.save_home_note(path, cx);
        let Some((worktree, entry)) = self.canvas_entry(path) else {
            return;
        };
        let (worktree, entry) = (worktree.clone(), entry.clone());
        let target = if entry.private {
            canvas::shared_dir(&worktree)
        } else {
            let Some(vault) = self.notes_dir(&worktree, cx) else {
                self.announce_error(tr!("overview-note-no-vault"), cx);
                return;
            };
            canvas::private_dir(&vault)
        };
        let Some(name) = entry.path.file_name() else {
            return;
        };
        let moved = crate::wslpath::join(&target, name);
        let text = canvas::render(&entry.node);
        self.git.send(Cmd::WriteCanvasFile {
            worktree: worktree.clone(),
            path: moved.clone(),
            text,
            expect: Some(crate::files::ABSENT),
        });
        self.git.send(Cmd::WriteCanvasFile {
            worktree,
            path: entry.path.clone(),
            text: String::new(),
            expect: Some(entry.digest),
        });
        self.note_editors.remove(path);
        self.rename_node(&Node::Note(entry.path.clone()), Node::Note(moved), cx);
        cx.notify();
    }

    /// Puts a closed review away: moved to `.claudhub/archive/` — still in
    /// the repository's history and its files, no longer on the plane.
    fn archive_home_note(&mut self, path: &Path, cx: &mut Context<Self>) {
        let Some((worktree, entry)) = self.canvas_entry(path) else {
            return;
        };
        let (worktree, entry) = (worktree.clone(), entry.clone());
        let target = if entry.private {
            let Some(vault) = self.notes_dir(&worktree, cx) else {
                return;
            };
            crate::wslpath::join(&canvas::private_dir(&vault), "archive")
        } else {
            canvas::archive_dir(&worktree)
        };
        let Some(name) = entry.path.file_name() else {
            return;
        };
        self.git.send(Cmd::WriteCanvasFile {
            worktree: worktree.clone(),
            path: crate::wslpath::join(&target, name),
            text: canvas::render(&entry.node),
            expect: None,
        });
        self.git.send(Cmd::WriteCanvasFile {
            worktree,
            path: entry.path.clone(),
            text: String::new(),
            expect: Some(entry.digest),
        });
        self.note_editors.remove(path);
        self.forget_node(&Node::Note(entry.path.clone()), cx);
        cx.notify();
    }

    /// A review done with: every finding resolved, or its branch merged into
    /// its base — it has nothing left to ask of anyone.
    fn review_closed(&self, entry: &CanvasEntry) -> bool {
        if entry.node.kind != canvas::Kind::Review {
            return false;
        }
        if entry.node.status.as_deref() == Some("resolved") {
            return true;
        }
        let Some((worktree, _)) = self.canvas_entry(&entry.path) else {
            return false;
        };
        let main = self.repos.worktree(worktree).is_some_and(|w| w.is_main);
        !main
            && self
                .outlines
                .get(worktree)
                .is_some_and(|o| o.base.is_some() && o.ahead_of_base == 0)
    }

    /// The band a closed review carries: archive it, or delete it.
    fn render_closed_band(&self, entry: &CanvasEntry, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme().clone();
        let (archive, delete) = (entry.path.clone(), entry.path.clone());
        h_flex()
            .flex_none()
            .px_2()
            .py_1()
            .gap_1()
            .items_center()
            .bg(theme.success.opacity(0.12))
            .text_xs()
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .text_color(theme.success)
                    .child(tr!("review-closed")),
            )
            .child(
                Button::new(SharedString::from(format!(
                    "review-archive-{}",
                    entry.path.display()
                )))
                .ghost()
                .xsmall()
                .label(tr!("review-archive"))
                .tooltip(tr!("review-archive-hint"))
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.archive_home_note(&archive, cx);
                })),
            )
            .child(
                Button::new(SharedString::from(format!(
                    "review-delete-{}",
                    entry.path.display()
                )))
                .ghost()
                .xsmall()
                .label(tr!("review-delete"))
                .on_click(cx.listener(move |this, _, window, cx| {
                    this.delete_home_note(&delete, window, cx);
                })),
            )
            .into_any_element()
    }

    /// Merges a worktree's branch into its base, once asked — the gesture of
    /// the worktree picker's « Integrate », from the card that shows how far
    /// ahead the branch is.
    pub(super) fn confirm_merge(
        &mut self,
        worktree: &Path,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let branch = self
            .repos
            .worktree(worktree)
            .and_then(|w| w.branch.clone())
            .unwrap_or_default();
        let base = self
            .outlines
            .get(worktree)
            .and_then(|o| o.base.clone())
            .unwrap_or_default();
        let ahead = self.outlines.get(worktree).map_or(0, |o| o.ahead_of_base);
        let target = worktree.to_path_buf();
        super::dialogs::ask(
            cx.entity(),
            tr!("merge-title", { branch: branch, base: base }),
            move || {
                div()
                    .text_sm()
                    .child(tr!("merge-body", { count: ahead }))
                    .into_any_element()
            },
            super::dialogs::confirm,
            move |this, _, cx| this.integrate(target.clone(), cx),
            window,
            cx,
        );
    }

    /// Deletes a note, once asked: nothing else keeps its text — unless it
    /// was committed, which git does.
    pub(super) fn delete_home_note(
        &mut self,
        path: &Path,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let path = path.to_path_buf();
        super::dialogs::ask(
            cx.entity(),
            tr!("overview-note-delete-title"),
            || {
                div()
                    .text_sm()
                    .child(tr!("overview-note-delete-body"))
                    .into_any_element()
            },
            super::dialogs::confirm,
            move |this, _, cx| this.delete_note_files(&path, cx),
            window,
            cx,
        );
    }

    /// The cross of a note, a review or a diagram: take it off the plane and
    /// keep its file — the « Hidden » menu brings it back — or delete the
    /// file, its picture with it.
    pub(super) fn close_home_note(
        &mut self,
        path: &Path,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some((_, entry)) = self.canvas_entry(path) else {
            return;
        };
        let name = entry
            .node
            .heading()
            .unwrap_or_else(|| path.display().to_string());
        let shared = !entry.private;
        let (entity, path) = (cx.entity(), path.to_path_buf());
        let delete = path.clone();
        super::dialogs::ask(
            cx.entity(),
            tr!("overview-close-note-title"),
            move || {
                v_flex()
                    .gap_1()
                    .child(div().text_sm().child(SharedString::from(name.clone())))
                    .child(div().text_xs().child(if shared {
                        tr!("overview-close-note-shared")
                    } else {
                        tr!("overview-close-note-private")
                    }))
                    .into_any_element()
            },
            move || {
                let (entity, path) = (entity.clone(), delete.clone());
                super::dialogs::choose(
                    tr!("overview-hide-button"),
                    tr!("overview-delete-file"),
                    move |_, cx| {
                        entity.update(cx, |this, cx| this.delete_note_files(&path, cx));
                    },
                )
            },
            move |this, _, cx| {
                let node = Node::Note(path.clone());
                this.note_editors.remove(&path);
                this.overview_hand.hidden.insert(node.clone());
                this.remember_folds(&node, cx);
                cx.notify();
            },
            window,
            cx,
        );
    }

    /// Deletes a node's file — and a diagram's picture with it: left behind,
    /// it would come back as a node of its own.
    fn delete_note_files(&mut self, path: &Path, cx: &mut Context<Self>) {
        self.note_editors.remove(path);
        let node = Node::Note(path.to_path_buf());
        self.overview_hand.hidden.remove(&node);
        self.forget_node(&node, cx);
        if let Some((worktree, entry)) = self.canvas_entry(path) {
            let worktree = worktree.clone();
            // A picture of its own has no text read to guard the delete with:
            // asked, it goes.
            let expect = (!entry.standalone).then_some(entry.digest);
            let picture = picture_of(entry).filter(|p| p != path);
            self.git.send(Cmd::WriteCanvasFile {
                worktree: worktree.clone(),
                path: path.to_path_buf(),
                text: String::new(),
                expect,
            });
            if let Some(picture) = picture {
                self.git.send(Cmd::DeleteCanvasPicture {
                    worktree,
                    path: picture,
                });
            }
        }
        cx.notify();
    }

    /// Writes each worktree's context sheet, where its vault is: what the
    /// window knows of it and of its neighbours — see `ui::context`.
    pub(super) fn write_contexts(&mut self, cx: &App) {
        use super::context::{Checkout, NodeLine, Sheet};
        let mut sheets = Vec::new();
        let checkout = |worktree: &crate::git::Worktree| Checkout {
            label: worktree.label(),
            path: worktree.path.display().to_string(),
            branch: worktree.branch.clone(),
            changes: self
                .summaries
                .get(&worktree.path)
                .map(|s| (s.files, s.added, s.removed)),
            agent: self.agents.get(&worktree.path).map(|agent| {
                use crate::agent::Activity;
                match &agent.activity {
                    Activity::Working => "working".to_string(),
                    Activity::Finished => "finished its turn".to_string(),
                    Activity::Waiting(message) if message.is_empty() => {
                        "waiting for an answer".into()
                    }
                    Activity::Waiting(message) => format!("waiting: {message}"),
                    Activity::Idle => "idle".to_string(),
                }
            }),
        };
        for repo in self.repos.iter() {
            for worktree in repo.worktrees.iter().filter(|w| !w.prunable) {
                let Some(vault) = self.notes_dir(&worktree.path, cx) else {
                    continue;
                };
                let outline = self.outlines.get(&worktree.path);
                let sheet = Sheet {
                    repo: repo.name.clone(),
                    here: checkout(worktree),
                    base: outline
                        .filter(|o| o.ahead_of_base > 0)
                        .and_then(|o| o.base.clone()),
                    ahead_of_base: outline.map_or(0, |o| o.ahead_of_base),
                    upstream: outline.and_then(|o| o.upstream),
                    commits: outline
                        .map(|o| {
                            o.commits
                                .iter()
                                .map(|c| (c.short.clone(), c.subject.clone()))
                                .collect()
                        })
                        .unwrap_or_default(),
                    terminals: self
                        .terminals
                        .iter()
                        .filter(|t| t.worktree == worktree.path)
                        .map(|t| {
                            let label = t
                                .name
                                .clone()
                                .unwrap_or_else(|| t.view.read(cx).label())
                                .to_string();
                            let agent =
                                matches!(t.relaunch, Some(super::store::Relaunch::Agent { .. }))
                                    || t.typed.is_some();
                            if agent {
                                format!("{label} (agent)")
                            } else {
                                label
                            }
                        })
                        .collect(),
                    nodes: self
                        .canvas
                        .get(&worktree.path)
                        .into_iter()
                        .flatten()
                        .map(|entry| NodeLine {
                            kind: match entry.node.kind {
                                canvas::Kind::Note => "note".into(),
                                canvas::Kind::Review => "review".into(),
                                canvas::Kind::Diagram => "diagram".into(),
                            },
                            title: entry.node.heading().unwrap_or_else(|| "(untitled)".into()),
                            file: entry
                                .path
                                .strip_prefix(&worktree.path)
                                .unwrap_or(&entry.path)
                                .display()
                                .to_string(),
                            private: entry.private,
                            author: entry
                                .node
                                .agent
                                .clone()
                                .or_else(|| entry.node.author.clone()),
                        })
                        .collect(),
                    others: repo
                        .worktrees
                        .iter()
                        .filter(|other| other.path != worktree.path && !other.prunable)
                        .map(checkout)
                        .collect(),
                };
                sheets.push((
                    crate::wslpath::join(&vault, CONTEXT),
                    super::context::render(&sheet),
                ));
            }
        }
        // Only what changed since it was last sent: a sheet per worktree
        // every ten seconds, and the worker reading each file back to compare.
        for (path, text) in sheets {
            let digest = crate::files::digest(&text);
            if self.canvas_reads.contexts.get(&path) == Some(&digest) {
                continue;
            }
            self.canvas_reads.contexts.insert(path.clone(), digest);
            self.git.send(Cmd::WriteContext { path, text });
        }
    }

    /// The checkout the skill button speaks for: the one on show when it
    /// is in the project on the plane, the project's main one otherwise.
    pub(super) fn skill_worktree(&self) -> Option<PathBuf> {
        let shown = self.overview_repos();
        if let Some(active) = self.active.clone() {
            if shown
                .iter()
                .any(|repo| repo.worktrees.iter().any(|w| w.path == active))
            {
                return Some(active);
            }
        }
        shown.first().map(|repo| repo.main.clone())
    }

    /// Asks where the skill is, for the checkout the button speaks for.
    pub(super) fn ask_skill_status(&self) {
        if let Some(worktree) = self.skill_worktree() {
            self.git.send(Cmd::SkillStatus { worktree });
        }
    }

    /// The skill's button: its state at a glance — a dot, green when this
    /// build's version is installed somewhere, amber when only an older one
    /// is, none when it is nowhere — and the gestures in its menu.
    /// `compact`, its glyph alone — on the sidebar's rail.
    pub(super) fn render_skill_button(
        &self,
        compact: bool,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        use crate::skill::Scope;
        let worktree = self.skill_worktree();
        let status = worktree
            .as_ref()
            .and_then(|w| self.skill_status.get(w))
            .copied()
            .unwrap_or_default();
        let current = crate::skill::version();
        let ours = |v: Option<u32>| v.filter(|v| *v > 0);
        let fresh = [status.repo, status.user]
            .iter()
            .any(|v| ours(*v) == Some(current));
        let stale = !fresh
            && [status.repo, status.user]
                .iter()
                .any(|v| ours(*v).is_some());
        let theme = cx.theme().clone();
        let dot = if fresh {
            Some(theme.success)
        } else if stale {
            Some(theme.warning)
        } else {
            None
        };
        let describe = |v: Option<u32>| -> SharedString {
            match v {
                None => tr!("skill-absent"),
                Some(0) => tr!("skill-foreign"),
                Some(v) if v == current => tr!("skill-current", { version: v }),
                Some(v) => tr!("skill-old", { version: v }),
            }
        };
        let (repo_line, user_line) = (
            tr!("skill-in-repo", { state: describe(status.repo) }),
            tr!("skill-for-me", { state: describe(status.user) }),
        );
        let app = cx.entity().downgrade();
        Button::new("overview-skill")
            .ghost()
            .small()
            .icon(icon("sparkles"))
            .when(!compact, |button| button.label(tr!("skill-button")))
            .when_some(dot, |button, color| {
                button.child(div().size(px(7.)).rounded_full().bg(color))
            })
            .tooltip(tr!("skill-tooltip"))
            .dropdown_menu(move |menu, _, _| {
                let Some(worktree) = worktree.clone() else {
                    return menu;
                };
                let entry = |label: SharedString, scope: Scope, install: bool| {
                    let (app, worktree) = (app.clone(), worktree.clone());
                    gpui_kit::component::menu::PopupMenuItem::new(label).on_click(
                        move |_, _, cx| {
                            if let Some(app) = app.upgrade() {
                                let worktree = worktree.clone();
                                app.update(cx, |this, cx| {
                                    this.git.send(Cmd::SetSkill {
                                        worktree,
                                        scope,
                                        install,
                                    });
                                    cx.notify();
                                });
                            }
                        },
                    )
                };
                let mut menu = menu
                    .label(repo_line.clone())
                    .label(user_line.clone())
                    .separator();
                let install = |v: Option<u32>| match v {
                    Some(v) if v > 0 && v < current => Some(true),
                    None => Some(false),
                    _ => None,
                };
                if let Some(update) = install(status.repo) {
                    menu = menu.item(entry(
                        if update {
                            tr!("skill-update-repo")
                        } else {
                            tr!("skill-install-repo")
                        },
                        Scope::Repo,
                        true,
                    ));
                }
                if let Some(update) = install(status.user) {
                    menu = menu.item(entry(
                        if update {
                            tr!("skill-update-user")
                        } else {
                            tr!("skill-install-user")
                        },
                        Scope::User,
                        true,
                    ));
                }
                if ours(status.repo).is_some() {
                    menu = menu.item(entry(tr!("skill-remove-repo"), Scope::Repo, false));
                }
                if ours(status.user).is_some() {
                    menu = menu.item(entry(tr!("skill-remove-user"), Scope::User, false));
                }
                menu
            })
    }

    /// A review read: its summary, then its findings — each a place one
    /// clicks to open and a tick that resolves it in the file.
    fn render_review_body(&self, entry: &CanvasEntry, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme().clone();
        let Some(review) = entry.review.clone() else {
            return div().into_any_element();
        };
        let path = entry.path.clone();
        let worktree = self
            .canvas_entry(&entry.path)
            .map(|(worktree, _)| worktree.clone());
        let rows: Vec<AnyElement> =
            review
                .findings
                .iter()
                .enumerate()
                .map(|(index, finding)| {
                    let color = match finding.severity.as_deref() {
                        Some("blocker") => theme.danger,
                        Some("warning") => theme.warning,
                        Some("suggestion") => theme.info,
                        _ => theme.muted_foreground,
                    };
                    let place = if finding.start == finding.end {
                        format!("{}:{}", finding.path, finding.start)
                    } else {
                        format!("{}:{}-{}", finding.path, finding.start, finding.end)
                    };
                    let resolved = finding.resolved();
                    let open = (worktree.clone(), review.clone(), index);
                    let tick = (path.clone(), finding.at);
                    let first = finding.text.lines().next().unwrap_or_default().to_string();
                    v_flex()
                        .gap_0p5()
                        .py_1()
                        .border_t_1()
                        .border_color(theme.border)
                        .when(resolved, |el| el.opacity(0.5))
                        .child(
                            h_flex()
                                .gap_1p5()
                                .items_center()
                                .text_xs()
                                .child(div().flex_none().size(px(7.)).rounded_full().bg(color))
                                .child(
                                    div()
                                        .id(SharedString::from(format!(
                                            "review-open-{}-{}",
                                            path.display(),
                                            finding.at
                                        )))
                                        .flex_1()
                                        .min_w_0()
                                        .truncate()
                                        .font_family(theme.mono_font_family.clone())
                                        .text_color(theme.link)
                                        .cursor_pointer()
                                        .child(SharedString::from(place))
                                        .on_click(cx.listener(move |this, _, window, cx| {
                                            let (worktree, review, index) = open.clone();
                                            if let Some(worktree) = worktree {
                                                let finding = &review.findings[index];
                                                this.open_finding(&worktree, finding, window, cx);
                                            }
                                        })),
                                )
                                .child(
                                    Button::new(SharedString::from(format!(
                                        "review-tick-{}-{}",
                                        path.display(),
                                        finding.at
                                    )))
                                    .ghost()
                                    .xsmall()
                                    .icon(icon(if resolved { "undo-2" } else { "check" }))
                                    .tooltip(if resolved {
                                        tr!("review-reopen")
                                    } else {
                                        tr!("review-resolve")
                                    })
                                    .on_click(cx.listener(move |this, _, _, cx| {
                                        let (path, at) = tick.clone();
                                        let status = if resolved { "open" } else { "resolved" };
                                        this.set_finding_status(&path, at, status, cx);
                                    })),
                                ),
                        )
                        .when(!first.is_empty(), |el| {
                            el.child(
                                div()
                                    .text_xs()
                                    .text_color(theme.muted_foreground)
                                    .child(SharedString::from(first)),
                            )
                        })
                        .into_any_element()
                })
                .collect();
        div()
            .id(SharedString::from(format!(
                "overview-review-{}",
                entry.path.display()
            )))
            .flex_1()
            .min_h_0()
            .p_2()
            .overflow_y_scroll()
            .text_sm()
            .child(gpui_kit::component::text::TextView::markdown(
                SharedString::from(format!("overview-review-summary-{}", entry.path.display())),
                review.summary.clone(),
            ))
            .children(rows)
            .into_any_element()
    }

    /// The open findings of a worktree's reviews, with the review file each
    /// comes from.
    pub(super) fn open_findings(&self, worktree: &Path) -> Vec<(PathBuf, canvas::Finding)> {
        self.canvas
            .get(worktree)
            .into_iter()
            .flatten()
            .filter_map(|entry| Some((entry, entry.review.as_ref()?)))
            .flat_map(|(entry, review)| {
                review
                    .findings
                    .iter()
                    .filter(|finding| !finding.resolved())
                    .map(|finding| (entry.path.clone(), finding.clone()))
            })
            .collect()
    }

    /// How many findings of a worktree's reviews are still open — what a
    /// card counts at each frame, without copying them.
    pub(super) fn open_finding_count(&self, worktree: &Path) -> usize {
        self.canvas
            .get(worktree)
            .into_iter()
            .flatten()
            .filter_map(|entry| entry.review.as_ref())
            .map(|review| review.open)
            .sum()
    }

    /// The Notes panel's section of the worktree's reviews: their open
    /// findings, each to open, resolve or hand to the agent — the notes'
    /// gestures, for remarks that live in a shared file rather than in the
    /// vault. `None` where there is no review.
    pub(super) fn render_reviews_section(&mut self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let worktree = self.active.clone()?;
        // What the rows need, and not the whole entries: a review's body is
        // not copied at each frame.
        let reviews: Vec<(PathBuf, String, String, std::rc::Rc<Review>)> = self
            .canvas
            .get(&worktree)?
            .iter()
            .filter_map(|entry| {
                let review = entry.review.clone()?;
                let title = entry
                    .node
                    .heading()
                    .unwrap_or_else(|| entry.path.display().to_string());
                let byline = entry
                    .node
                    .agent
                    .clone()
                    .into_iter()
                    .chain(entry.node.author.clone())
                    .collect::<Vec<_>>()
                    .join(" · ");
                Some((entry.path.clone(), title, byline, review))
            })
            .collect();
        if reviews.is_empty() {
            return None;
        }
        let open: usize = reviews.iter().map(|(_, _, _, review)| review.open).sum();
        let header = self.section_header(
            "reviews",
            "file-text",
            tr!("panel-reviews"),
            tr!("note-count", { count: open }),
            cx,
        );
        let send_all = worktree.clone();
        let header = header.child(
            Button::new("reviews-send-all")
                .ghost()
                .small()
                .icon(icon("send"))
                .tooltip(tr!("review-send-all"))
                .disabled(open == 0)
                .on_click(cx.listener(move |this, _, window, cx| {
                    this.send_findings(&send_all, None, window, cx);
                })),
        );
        if self.collapsed("reviews") {
            return Some(v_flex().w_full().child(header).into_any_element());
        }
        let theme = cx.theme().clone();
        let groups = reviews.into_iter().map(|(path, title, byline, review)| {
            let rows = review
                .findings
                .iter()
                .enumerate()
                .filter(|(_, f)| !f.resolved())
                .map(|(index, finding)| {
                    let color = match finding.severity.as_deref() {
                        Some("blocker") => theme.danger,
                        Some("warning") => theme.warning,
                        Some("suggestion") => theme.info,
                        _ => theme.muted_foreground,
                    };
                    let place = if finding.start == finding.end {
                        format!("{}:{}", finding.path, finding.start)
                    } else {
                        format!("{}:{}-{}", finding.path, finding.start, finding.end)
                    };
                    let key = format!("{}-{}", path.display(), finding.at);
                    let (open, tick, send) = (
                        (worktree.clone(), review.clone(), index),
                        (path.clone(), finding.at),
                        (worktree.clone(), path.clone(), finding.at),
                    );
                    let first = finding.text.lines().next().unwrap_or_default().to_string();
                    v_flex()
                        .w_full()
                        .px_2()
                        .py_1()
                        .gap_0p5()
                        .child(
                            h_flex()
                                .gap_1p5()
                                .items_center()
                                .text_xs()
                                .child(div().flex_none().size(px(7.)).rounded_full().bg(color))
                                .child(
                                    div()
                                        .id(SharedString::from(format!("reviews-open-{key}")))
                                        .flex_1()
                                        .min_w_0()
                                        .truncate()
                                        .font_family(theme.mono_font_family.clone())
                                        .text_color(theme.link)
                                        .cursor_pointer()
                                        .child(SharedString::from(place))
                                        .on_click(cx.listener(move |this, _, window, cx| {
                                            let (worktree, review, index) = open.clone();
                                            let finding = &review.findings[index];
                                            this.open_finding(&worktree, finding, window, cx);
                                        })),
                                )
                                .child(
                                    Button::new(SharedString::from(format!("reviews-send-{key}")))
                                        .ghost()
                                        .xsmall()
                                        .icon(icon("send"))
                                        .tooltip(tr!("review-send"))
                                        .on_click(cx.listener(move |this, _, window, cx| {
                                            let (worktree, file, at) = send.clone();
                                            this.send_findings(
                                                &worktree,
                                                Some((file, at)),
                                                window,
                                                cx,
                                            );
                                        })),
                                )
                                .child(
                                    Button::new(SharedString::from(format!("reviews-tick-{key}")))
                                        .ghost()
                                        .xsmall()
                                        .icon(icon("check"))
                                        .tooltip(tr!("review-resolve"))
                                        .on_click(cx.listener(move |this, _, _, cx| {
                                            let (path, at) = tick.clone();
                                            this.set_finding_status(&path, at, "resolved", cx);
                                        })),
                                ),
                        )
                        .when(!first.is_empty(), |el| {
                            el.child(
                                div()
                                    .text_xs()
                                    .text_color(theme.muted_foreground)
                                    .child(SharedString::from(first)),
                            )
                        })
                });
            v_flex()
                .w_full()
                .child(
                    h_flex()
                        .px_2()
                        .pt_1()
                        .gap_1p5()
                        .text_xs()
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .truncate()
                                .font_weight(gpui_kit::FontWeight::SEMIBOLD)
                                .child(SharedString::from(title)),
                        )
                        .when(!byline.is_empty(), |el| {
                            el.child(
                                div()
                                    .flex_none()
                                    .text_color(theme.muted_foreground)
                                    .child(SharedString::from(byline)),
                            )
                        }),
                )
                .children(rows)
        });
        Some(
            v_flex()
                .w_full()
                .child(header)
                .children(groups)
                .into_any_element(),
        )
    }

    /// Hands findings to the worktree's agent — one, or every open one —
    /// through the notes' prompt dialog: what is sent is seen and can be
    /// added to first.
    fn send_findings(
        &mut self,
        worktree: &Path,
        only: Option<(PathBuf, usize)>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let open = self.open_findings(worktree);
        let chosen: Vec<&(PathBuf, canvas::Finding)> = open
            .iter()
            .filter(|(file, finding)| {
                only.as_ref()
                    .is_none_or(|(path, at)| path == file && *at == finding.at)
            })
            .collect();
        if chosen.is_empty() {
            return;
        }
        // One prompt per review file: each names the file the agent ticks.
        let mut text = String::new();
        let mut files: Vec<&PathBuf> = chosen.iter().map(|(file, _)| file).collect();
        files.dedup();
        for file in files {
            let findings: Vec<&canvas::Finding> = chosen
                .iter()
                .filter(|(f, _)| f == file)
                .map(|(_, finding)| finding)
                .collect();
            let shown = file
                .strip_prefix(worktree)
                .unwrap_or(file)
                .display()
                .to_string();
            if !text.is_empty() {
                text.push_str("\n---\n\n");
            }
            text.push_str(&canvas::finding_prompt(&shown, &findings));
        }
        self.confirm_prompt(worktree.to_path_buf(), Vec::new(), text, false, window, cx);
    }

    /// A diagram: its picture, scaled into the node, and its caption under it.
    fn render_diagram_body(&self, entry: &CanvasEntry, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme().clone();
        let picture = picture_of(entry)
            .and_then(|path| self.canvas_pictures.get(&path))
            .and_then(|picture| picture.image.clone());
        v_flex()
            .flex_1()
            .min_h_0()
            .p_2()
            .gap_1()
            .child(
                div()
                    .flex_1()
                    .min_h_0()
                    .flex()
                    .items_center()
                    .justify_center()
                    .rounded(theme.radius)
                    // A diagram drawn for a light page stays readable on a
                    // dark theme: it keeps the page it was drawn on.
                    .bg(gpui_kit::white())
                    .overflow_hidden()
                    .map(|el| match picture {
                        Some(image) => el.child(
                            gpui_kit::img(image)
                                .size_full()
                                .object_fit(gpui_kit::ObjectFit::Contain),
                        ),
                        None => el.child(
                            div()
                                .text_xs()
                                .text_color(theme.muted_foreground)
                                .child(tr!("overview-diagram-loading")),
                        ),
                    }),
            )
            .when(!entry.node.body.trim().is_empty(), |el| {
                el.child(div().flex_none().max_h(px(120.)).text_xs().child(
                    gpui_kit::component::text::TextView::markdown(
                        SharedString::from(format!(
                            "overview-diagram-caption-{}",
                            entry.path.display()
                        )),
                        entry.body.clone(),
                    ),
                ))
            })
            .into_any_element()
    }

    /// Goes to a finding: its worktree, its file, its line.
    fn open_finding(
        &mut self,
        worktree: &Path,
        finding: &canvas::Finding,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.work_in_worktree(worktree, window, cx);
        self.open_at(
            PathBuf::from(&finding.path),
            Some(super::explorer::Landing::Position {
                line: finding.start.saturating_sub(1),
                character: 0,
            }),
            cx,
        );
    }

    /// Resolves or reopens one finding, in the file — where a teammate's
    /// pull will see it — and the review with it once none is left open.
    fn set_finding_status(&mut self, path: &Path, at: usize, status: &str, cx: &mut Context<Self>) {
        let Some((worktree, entry)) = self.canvas_entry_mut(path) else {
            return;
        };
        let mut node = entry.node.clone();
        node.body = canvas::set_status(&node.body, at, status);
        let (_, found) = canvas::findings(&node.body);
        node.status = Some(if found.iter().all(canvas::Finding::resolved) {
            "resolved".into()
        } else {
            "open".into()
        });
        let text = canvas::render(&node);
        let expect = entry.digest;
        entry.digest = crate::files::digest(&text);
        entry.stamp = 0;
        entry.set_node(node);
        self.git.send(Cmd::WriteCanvasFile {
            worktree,
            path: path.to_path_buf(),
            text,
            expect: Some(expect),
        });
        cx.notify();
    }

    /// A note: rendered Markdown, or the field it is written in.
    pub(super) fn render_home_note(
        &self,
        path: &Path,
        zoom: f32,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let Some((_, entry)) = self.canvas_entry(path) else {
            return div().into_any_element();
        };
        let theme = cx.theme().clone();
        let muted = theme.muted_foreground;
        let detail = zoom >= super::overview_view::DETAIL;
        let editor = self.note_editors.get(path).map(|open| open.editor.clone());
        let editing = editor.is_some();
        let title = entry
            .node
            .heading()
            .map(SharedString::from)
            .unwrap_or_else(|| tr!("overview-note"));
        let review = entry.node.kind == canvas::Kind::Review;
        let open_findings = entry
            .review
            .as_ref()
            .map(|read| (read.open, read.findings.len()));
        // Who wrote it, in a shared file: the point of sharing is reading a
        // colleague's.
        let byline = entry
            .node
            .agent
            .clone()
            .into_iter()
            .chain(entry.node.author.clone())
            .collect::<Vec<_>>()
            .join(" · ");
        let app = cx.entity().downgrade();
        let node = Node::Note(path.to_path_buf());
        let (edit, private, twice) = (path.to_path_buf(), path.to_path_buf(), path.to_path_buf());
        let tint = if review { theme.info } else { theme.warning };
        let head = h_flex()
            .flex_none()
            .h(px(overview::HEAD * zoom))
            .px_2()
            .gap_1p5()
            .items_center()
            .bg(tint.opacity(0.18))
            .cursor_grab()
            .on_mouse_down(
                MouseButton::Left,
                super::overview_view::grab(app.clone(), node.clone()),
            )
            .child(
                icon(match entry.node.kind {
                    canvas::Kind::Review => "file-text",
                    canvas::Kind::Diagram => "image",
                    canvas::Kind::Note => "sticky-note",
                })
                .text_color(muted),
            )
            .when(entry.private, |el| {
                el.child(icon("eye-off").text_color(muted))
            })
            // A review says how much of it is still to do.
            .when_some(open_findings, |el, (open, all)| {
                el.child(
                    div()
                        .flex_none()
                        .text_xs()
                        .text_color(if open > 0 {
                            theme.warning
                        } else {
                            theme.success
                        })
                        .child(tr!("review-open-count", { open: open, all: all })),
                )
            })
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .font_weight(gpui_kit::FontWeight::SEMIBOLD)
                    .child(title),
            )
            .when(detail && !byline.is_empty(), |el| {
                el.child(
                    div()
                        .flex_none()
                        .max_w(px(160. * zoom.max(0.5)))
                        .truncate()
                        .text_xs()
                        .text_color(muted)
                        .child(SharedString::from(byline.clone())),
                )
            })
            .when(detail && !entry.standalone, |el| {
                el.child(
                    Button::new(SharedString::from(format!(
                        "overview-note-edit-{}",
                        path.display()
                    )))
                    .ghost()
                    .xsmall()
                    .icon(icon(if editing { "check" } else { "pencil" }))
                    .tooltip(if editing {
                        tr!("overview-note-done")
                    } else {
                        tr!("overview-note-edit")
                    })
                    .on_click(cx.listener(move |this, _, window, cx| {
                        if editing {
                            this.done_home_note(&edit, window, cx);
                        } else {
                            this.edit_home_note(&edit, window, cx);
                        }
                    })),
                )
                .child(
                    Button::new(SharedString::from(format!(
                        "overview-note-private-{}",
                        path.display()
                    )))
                    .ghost()
                    .xsmall()
                    .icon(icon(if entry.private { "users" } else { "eye-off" }))
                    .tooltip(if entry.private {
                        tr!("overview-note-share")
                    } else {
                        tr!("overview-note-keep")
                    })
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.toggle_note_privacy(&private, cx);
                    })),
                )
            })
            .when(detail, |el| {
                el.child(self.window_controls(node.clone(), cx))
            });
        let body = match editor {
            Some(editor) => v_flex()
                .flex_1()
                .min_h_0()
                .p_1()
                .child(
                    gpui_kit::component::input::Editor::new(&editor)
                        .text_sm()
                        .h_full(),
                )
                .into_any_element(),
            None if review => self.render_review_body(entry, cx),
            None if entry.node.kind == canvas::Kind::Diagram => self.render_diagram_body(entry, cx),
            None => div()
                .id(SharedString::from(format!(
                    "overview-note-body-{}",
                    path.display()
                )))
                .flex_1()
                .min_h_0()
                .p_2()
                .overflow_y_scroll()
                .text_sm()
                .cursor_text()
                // Twice to write in it, the gesture of every sticky note.
                .on_click(
                    cx.listener(move |this, event: &gpui_kit::ClickEvent, window, cx| {
                        if event.click_count() >= 2 {
                            this.edit_home_note(&twice, window, cx);
                        }
                    }),
                )
                .map(|el| {
                    if entry.node.body.trim().is_empty() {
                        el.text_color(muted).child(tr!("overview-note-empty"))
                    } else {
                        el.child(gpui_kit::component::text::TextView::markdown(
                            SharedString::from(format!("overview-note-text-{}", path.display())),
                            entry.body.clone(),
                        ))
                    }
                })
                .into_any_element(),
        };
        let band =
            (!editing && self.review_closed(entry)).then(|| self.render_closed_band(entry, cx));
        v_flex()
            .size_full()
            .overflow_hidden()
            .rounded(theme.radius_lg)
            .border_1()
            .border_color(tint.opacity(0.45))
            .bg(theme.background)
            // A note is written in and read, not dragged by its body.
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .child(head)
            .children(band)
            .child(body)
            .into_any_element()
    }
}
