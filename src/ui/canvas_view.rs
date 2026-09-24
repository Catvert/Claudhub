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
    v_flex, ActiveTheme, Sizable as _, WindowExt as _,
};
use gpui_kit::{
    div, prelude::*, px, AnyElement, App, Context, Entity, Focusable as _, MouseButton,
    SharedString, WeakEntity, Window,
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
    pub node: canvas::Node,
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

/// The `+ note` of a git node's or a card's head.
pub(super) fn note_button(app: WeakEntity<ClaudhubApp>, hang: Hang) -> Button {
    let id = SharedString::from(format!("overview-add-note-{hang:?}"));
    Button::new(id)
        .ghost()
        .xsmall()
        .icon(icon("sticky-note"))
        .tooltip(tr!("overview-note-add"))
        .on_click(move |_, _, cx| {
            if let Some(app) = app.upgrade() {
                let hang = hang.clone();
                app.update(cx, |this, cx| this.add_home_note(hang, cx));
            }
        })
}

impl ClaudhubApp {
    /// Asks the workers for these worktrees' node files.
    pub(super) fn read_canvas(&self, worktrees: impl IntoIterator<Item = PathBuf>, cx: &App) {
        for worktree in worktrees {
            let mut dirs = vec![(canvas::shared_dir(&worktree), false)];
            if let Some(vault) = self.notes_dir(&worktree, cx) {
                dirs.push((canvas::private_dir(&vault), true));
            }
            self.git.send(Cmd::ReadCanvas { worktree, dirs });
        }
    }

    /// A worktree's node files, read. A note just created is opened for
    /// writing once it is there to open.
    pub(super) fn canvas_read(
        &mut self,
        worktree: PathBuf,
        files: Vec<(PathBuf, bool, String)>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let mut entries: Vec<CanvasEntry> = files
            .into_iter()
            .filter_map(|(path, private, text)| {
                let node = canvas::parse(&text)?;
                Some(CanvasEntry {
                    path,
                    private,
                    digest: crate::files::digest(&text),
                    node,
                })
            })
            .collect();
        // The order they were written in: the names start with their date.
        entries.sort_by(|a, b| a.path.file_name().cmp(&b.path.file_name()));
        self.canvas.insert(worktree, entries);
        if let Some(created) = self.canvas_created.take() {
            if self.canvas_entry(&created).is_some() {
                self.overview_reveal = Some(Node::Note(created.clone()));
                self.edit_home_note(&created, window, cx);
            } else {
                self.canvas_created = Some(created);
            }
        }
        cx.notify();
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

    /// The worktree a node file was read for, and what it holds.
    pub(super) fn canvas_entry(&self, path: &Path) -> Option<(&PathBuf, &CanvasEntry)> {
        self.canvas.iter().find_map(|(worktree, entries)| {
            entries
                .iter()
                .find(|entry| entry.path == path)
                .map(|entry| (worktree, entry))
        })
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
        let Some((worktree, entry)) = self.canvas.iter_mut().find_map(|(worktree, entries)| {
            entries
                .iter_mut()
                .find(|entry| entry.path == path)
                .map(|entry| (worktree.clone(), entry))
        }) else {
            return;
        };
        if entry.node.body == body {
            return;
        }
        let mut node = entry.node.clone();
        node.body = body;
        let text = canvas::render(&node);
        let expect = entry.digest;
        // What the disk will hold once this lands: the next save's guard.
        entry.digest = crate::files::digest(&text);
        entry.node = node;
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

    /// Deletes a note, once asked: nothing else keeps its text — unless it
    /// was committed, which git does.
    pub(super) fn delete_home_note(
        &mut self,
        path: &Path,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let entity = cx.entity();
        let path = path.to_path_buf();
        window.open_dialog(cx, move |dialog, _, _| {
            let (entity, path) = (entity.clone(), path.clone());
            dialog
                .title(tr!("overview-note-delete-title"))
                .child(div().text_sm().child(tr!("overview-note-delete-body")))
                .overlay_closable(false)
                .close_button(false)
                .footer(super::dialogs::confirm())
                .on_ok(move |_, _, cx| {
                    entity.update(cx, |this, cx| {
                        this.note_editors.remove(&path);
                        let node = Node::Note(path.clone());
                        this.forget_node(&node, cx);
                        if let Some((worktree, entry)) = this.canvas_entry(&path) {
                            let (worktree, digest) = (worktree.clone(), entry.digest);
                            this.git.send(Cmd::WriteCanvasFile {
                                worktree,
                                path: path.clone(),
                                text: String::new(),
                                expect: Some(digest),
                            });
                        }
                        cx.notify();
                    });
                    true
                })
        });
        // See `close_node`: the buttons dispatch from the focus.
        window.defer(cx, |window, cx| window.focus_dialog(cx));
    }

    /// Writes each worktree's context sheet, where its vault is: what the
    /// window knows of it and of its neighbours — see `ui::context`.
    pub(super) fn write_contexts(&self, cx: &App) {
        use super::context::{Checkout, NodeLine, Sheet};
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
                self.git.send(Cmd::WriteContext {
                    path: crate::wslpath::join(&vault, CONTEXT),
                    text: super::context::render(&sheet),
                });
            }
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
    pub(super) fn render_skill_button(&self, cx: &mut Context<Self>) -> impl IntoElement {
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
            .label(tr!("skill-button"))
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
        let (summary, found) = canvas::findings(&entry.node.body);
        let path = entry.path.clone();
        let worktree = self
            .canvas_entry(&entry.path)
            .map(|(worktree, _)| worktree.clone());
        let rows: Vec<AnyElement> =
            found
                .into_iter()
                .map(|finding| {
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
                    let open = (worktree.clone(), finding.clone());
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
                                            let (worktree, finding) = open.clone();
                                            if let Some(worktree) = worktree {
                                                this.open_finding(&worktree, &finding, window, cx);
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
                summary,
            ))
            .children(rows)
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
        let Some((worktree, entry)) = self.canvas.iter_mut().find_map(|(worktree, entries)| {
            entries
                .iter_mut()
                .find(|entry| entry.path == path)
                .map(|entry| (worktree.clone(), entry))
        }) else {
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
        entry.node = node;
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
        let entry = entry.clone();
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
        let open_findings = review.then(|| {
            let (_, found) = canvas::findings(&entry.node.body);
            (found.iter().filter(|f| !f.resolved()).count(), found.len())
        });
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
            .child(icon(if review { "file-text" } else { "sticky-note" }).text_color(muted))
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
            .when(detail, |el| {
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
                .child(self.window_controls(node.clone(), cx))
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
            None if review => self.render_review_body(&entry, cx),
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
                            entry.node.body.clone(),
                        ))
                    }
                })
                .into_any_element(),
        };
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
            .child(body)
            .into_any_element()
    }
}
