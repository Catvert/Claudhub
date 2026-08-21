//! The gestures of annotated review: taking a note, putting it back into the
//! diff, listing it, sending it back to the agent.
//!
//! The model and everything testable without gpui live in `notes.rs`; here there
//! is only interface plumbing.
//!
//! Two things are not obvious in it:
//!
//! - **The anchoring is settled at the moment of the gesture**, not when the
//!   dialog is confirmed. An agent writes in the worktree while it is being
//!   reviewed, every write reloads the diff, and the selection does not survive
//!   the reload: deciding the anchoring at confirmation time would make the note
//!   apply to whatever arrived while it was being written.
//! - **The gutter markers are computed upstream**, when the diff arrives and on
//!   every change to the notes, never inside the virtualised list's closure:
//!   that one runs for every visible line on every frame, wheel animation
//!   included.

use std::path::{Path, PathBuf};

use gpui::{div, prelude::*, px, Context, SharedString, Window};
use gpui_component::{
    button::{Button, ButtonVariants},
    checkbox::Checkbox,
    h_flex,
    input::{Input, Textarea},
    v_flex, ActiveTheme, Disableable, Selectable, Sizable, WindowExt,
};

use crate::git::DiffRange;
use crate::runtime::protocol::Cmd;
use crate::tr;
use crate::ui::app::ClaudhubApp;
use crate::ui::icons::icon;
use crate::ui::notes::{self, Note, Side};

/// A note being written, with its anchoring already settled.
pub struct NoteDraft {
    /// Filled in when an existing note is being edited rather than created: the
    /// body changes, the anchoring does not move.
    pub editing: Option<u64>,
    pub range: DiffRange,
    pub path: PathBuf,
    pub side: Side,
    pub start: usize,
    pub end: usize,
    pub excerpt: String,
}

impl ClaudhubApp {
    // — Taking a note ——————————————————————————————————————————

    /// Opens the annotation dialog on the current selection.
    ///
    /// With no selection, nothing: a note applies to a **range**, and taking the
    /// whole file — which is what copying does for want of better — would give a
    /// remark that names nothing.
    pub(super) fn annotate_selection(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let split = crate::ui::settings::Settings::global(cx).diff_split;
        let Some(state) = self.active_review() else {
            return;
        };
        let (Some(diff), Some(path)) = (state.diff.clone(), state.selected.clone()) else {
            return;
        };
        let range = state.range.clone();
        // Copying and note-taking start from the same place: the unified list,
        // which alone carries the file's order.
        let Some((from, to)) = (match (split, state.diff_selection) {
            (true, Some((a, b))) => diff.unified_span(a, b),
            (false, Some((a, b))) => Some((a.min(b), a.max(b))),
            (_, None) => None,
        }) else {
            self.announce(tr!("note-needs-a-selection"), cx);
            return;
        };
        let Some((side, start, end)) = notes::anchor_selection(&diff, from, to) else {
            self.announce(tr!("note-needs-a-selection"), cx);
            return;
        };
        self.note_draft = Some(NoteDraft {
            editing: None,
            range,
            path,
            side,
            start,
            end,
            excerpt: diff.copy_text(from, to, false),
        });
        self.open_note_dialog(String::new(), window, cx);
    }

    /// Reopens a note to correct its text.
    pub(super) fn edit_note(&mut self, id: u64, window: &mut Window, cx: &mut Context<Self>) {
        let Some(note) = self.note(id).cloned() else {
            return;
        };
        self.note_draft = Some(NoteDraft {
            editing: Some(note.id),
            range: note.range,
            path: note.path,
            side: note.side,
            start: note.start,
            end: note.end,
            excerpt: note.excerpt,
        });
        self.open_note_dialog(note.body, window, cx);
    }

    /// The input dialog.
    ///
    /// A dialog and not a popover anchored to the row: the row belongs to a
    /// virtualised list, and the slightest scroll — the one opening the keyboard
    /// already causes — would carry the anchor away, and the popover with it.
    fn open_note_dialog(&mut self, body: String, window: &mut Window, cx: &mut Context<Self>) {
        let Some(draft) = self.note_draft.as_ref() else {
            return;
        };
        let title = SharedString::from(format!(
            "{}:{}",
            draft.path.display(),
            span_label(draft.start, draft.end)
        ));
        let excerpt = draft.excerpt.clone();
        let input = self.note_input.clone();
        let entity = cx.entity();
        input.update(cx, |input, cx| input.set_value(body, window, cx));
        let mono = cx.theme().mono_font_family.clone();
        window.open_dialog(cx, move |dialog, _window, _cx| {
            let (input, excerpt, mono) = (input.clone(), excerpt.clone(), mono.clone());
            let (on_ok, on_cancel) = (entity.clone(), entity.clone());
            dialog
                .title(tr!("note-title"))
                .child(
                    v_flex()
                        .gap_2()
                        .w(px(560.))
                        .child(
                            div()
                                .text_xs()
                                .font_family(mono.clone())
                                .child(title.clone()),
                        )
                        // The excerpt is recalled in front of you: one writes a
                        // remark *about* code, and the dialog covers precisely
                        // the code being looked at.
                        .child(
                            v_flex()
                                .id("note-excerpt")
                                .max_h(px(160.))
                                .overflow_y_scroll()
                                .p_2()
                                .rounded(px(4.))
                                .text_xs()
                                .font_family(mono)
                                .children(
                                    excerpt_lines(&excerpt, usize::MAX)
                                        .into_iter()
                                        .map(|line| div().whitespace_nowrap().child(line)),
                                ),
                        )
                        .child(Textarea::new(&input)),
                )
                .overlay_closable(false)
                .close_button(false)
                .on_ok(move |_, _window, cx| {
                    let body = input.read(cx).value().to_string();
                    on_ok.update(cx, |this, cx| this.save_note(body, cx));
                    true
                })
                .on_cancel(move |_, _window, cx| {
                    on_cancel.update(cx, |this, _| this.note_draft = None);
                    true
                })
        });
    }

    fn save_note(&mut self, body: String, cx: &mut Context<Self>) {
        let Some(draft) = self.note_draft.take() else {
            return;
        };
        if body.trim().is_empty() {
            return;
        }
        let Some(worktree) = self.active.clone() else {
            return;
        };
        let Some(state) = self.review.get_mut(&worktree) else {
            return;
        };
        match draft.editing {
            // Editing a note reopens the question: it goes back to unsent,
            // otherwise the corrected version would never go out.
            Some(id) => {
                if let Some(note) = state.notes.iter_mut().find(|note| note.id == id) {
                    note.body = body;
                    note.sent = false;
                }
            }
            None => {
                let id = state.next_note;
                state.next_note += 1;
                state.notes.push(Note {
                    id,
                    range: draft.range,
                    path: draft.path,
                    side: draft.side,
                    start: draft.start,
                    end: draft.end,
                    excerpt: draft.excerpt,
                    body,
                    sent: false,
                    done: false,
                });
            }
        }
        self.refresh_note_marks(&worktree);
        self.persist_review(&worktree, cx);
        cx.notify();
    }

    pub(super) fn delete_note(&mut self, id: u64, cx: &mut Context<Self>) {
        let Some(worktree) = self.active.clone() else {
            return;
        };
        if let Some(state) = self.review.get_mut(&worktree) {
            state.notes.retain(|note| note.id != id);
        }
        self.refresh_note_marks(&worktree);
        self.persist_review(&worktree, cx);
        cx.notify();
    }

    pub(super) fn set_note_done(&mut self, id: u64, done: bool, cx: &mut Context<Self>) {
        let Some(worktree) = self.active.clone() else {
            return;
        };
        if let Some(note) = self
            .review
            .get_mut(&worktree)
            .and_then(|state| state.notes.iter_mut().find(|note| note.id == id))
        {
            note.done = done;
        }
        self.refresh_note_marks(&worktree);
        self.persist_review(&worktree, cx);
        cx.notify();
    }

    // — The worktree's task list —————————————————————————————————

    /// Ticks or unticks a `TODO.md` task.
    ///
    /// The write is **conditional**, and that is the whole point: this file is
    /// the agent's, which ticks off what it has just finished while we read it.
    /// The digest of what we had in front of us goes out with the write, and a
    /// file that has moved since makes it be refused rather than overwriting its
    /// work. The displayed list, for its part, updates at once: the vault's
    /// watch will bring back the disk's truth a quarter of a second later
    /// anyway.
    pub(super) fn toggle_task(&mut self, line: usize, done: bool, cx: &mut Context<Self>) {
        let Some(worktree) = self.active.clone() else {
            return;
        };
        let Some(dir) = self.notes_dir(&worktree, cx) else {
            return;
        };
        let Some(state) = self.review.get_mut(&worktree) else {
            return;
        };
        let Some(todo) = state.todo.as_ref() else {
            return;
        };
        let Some(text) = crate::ui::vault::toggle_task(&todo.text, line, done) else {
            return;
        };
        let expect = Some(crate::files::digest(&todo.text));
        state.todo = Some(crate::ui::vault::parse_todo(&text));
        self.git.send(Cmd::WriteVaultFile {
            worktree,
            path: dir.join(crate::ui::vault::TODO),
            text,
            expect,
        });
        cx.notify();
    }

    /// Opens a task for editing, in its place in the list.
    ///
    /// An input on the row and not a dialog: a task list is corrected on the
    /// fly, and one dialog per correction would mean two clicks and a window to
    /// change one word.
    pub(super) fn edit_task(&mut self, line: usize, window: &mut Window, cx: &mut Context<Self>) {
        let Some(label) = self
            .active_review()
            .and_then(|state| state.todo.as_ref())
            .and_then(|todo| todo.tasks.iter().find(|task| task.line == line))
            .map(|task| task.label.clone())
        else {
            return;
        };
        self.task_editing = Some(line);
        self.task_edit_input
            .update(cx, |input, cx| input.set_value(label, window, cx));
        gpui::Focusable::focus_handle(&self.task_edit_input, cx).focus(window, cx);
        cx.notify();
    }

    /// Confirms the running edit. An empty label **deletes** the task.
    ///
    /// It is the convention of lists edited in place: clearing the text and
    /// confirming is the gesture by which a row is removed, and it saves one
    /// more button on every one of them.
    pub(super) fn commit_task_edit(&mut self, label: &str, cx: &mut Context<Self>) {
        let Some(line) = self.task_editing.take() else {
            return;
        };
        cx.notify();
        if label.trim().is_empty() {
            self.remove_task(line, cx);
            return;
        }
        self.rewrite_todo(cx, |text| {
            crate::ui::vault::set_task_label(text, line, label)
        });
    }

    pub(super) fn remove_task(&mut self, line: usize, cx: &mut Context<Self>) {
        self.rewrite_todo(cx, |text| crate::ui::vault::remove_task(text, line));
    }

    /// Applies a transformation to the displayed worktree's `TODO.md`.
    ///
    /// The compulsory path of all three editing gestures: the transformation is
    /// pure and returns `None` when the targeted line is no longer what it was —
    /// the agent wrote in the meantime — and the write goes out with the digest
    /// of what we had in front of us.
    fn rewrite_todo(&mut self, cx: &mut Context<Self>, edit: impl FnOnce(&str) -> Option<String>) {
        let Some(worktree) = self.active.clone() else {
            return;
        };
        let Some(dir) = self.notes_dir(&worktree, cx) else {
            return;
        };
        let Some(current) = self
            .review
            .get(&worktree)
            .and_then(|state| state.todo.as_ref())
            .map(|todo| todo.text.clone())
        else {
            return;
        };
        let Some(text) = edit(&current) else {
            return;
        };
        let expect = Some(crate::files::digest(&current));
        if let Some(state) = self.review.get_mut(&worktree) {
            state.todo = Some(crate::ui::vault::parse_todo(&text));
        }
        self.git.send(Cmd::WriteVaultFile {
            worktree,
            path: dir.join(crate::ui::vault::TODO),
            text,
            expect,
        });
        cx.notify();
    }

    /// Adds a task to the worktree's `TODO.md`, creating it if there is none.
    pub(super) fn add_task(&mut self, label: &str, cx: &mut Context<Self>) {
        if label.trim().is_empty() {
            return;
        }
        let Some(worktree) = self.active.clone() else {
            return;
        };
        let Some(dir) = self.notes_dir(&worktree, cx) else {
            return;
        };
        let current = self
            .review
            .get(&worktree)
            .and_then(|state| state.todo.as_ref())
            .map(|todo| todo.text.clone());
        let expect = current.as_deref().map(crate::files::digest);
        let base = current.unwrap_or_else(|| crate::ui::vault::seed_todo(&worktree));
        let text = crate::ui::vault::append_task(&base, label);
        if let Some(state) = self.review.get_mut(&worktree) {
            state.todo = Some(crate::ui::vault::parse_todo(&text));
        }
        self.git.send(Cmd::WriteVaultFile {
            worktree,
            path: dir.join(crate::ui::vault::TODO),
            text,
            expect,
        });
        cx.notify();
    }

    /// Hands every file of a worktree back to be reviewed.
    ///
    /// The gesture was missing: one ticks file by file, and starting a review
    /// again from scratch took as many clicks as the branch has files, or a trip
    /// into the vault's Markdown.
    pub(super) fn clear_reviewed(&mut self, cx: &mut Context<Self>) {
        let Some(worktree) = self.active.clone() else {
            return;
        };
        if let Some(state) = self.review.get_mut(&worktree) {
            state.reviewed.clear();
        }
        self.persist_review(&worktree, cx);
        cx.notify();
    }

    pub(super) fn toggle_notes_filter(&mut self, cx: &mut Context<Self>) {
        self.notes_only_open = !self.notes_only_open;
        cx.notify();
    }

    fn note(&self, id: u64) -> Option<&Note> {
        self.active_review()?
            .notes
            .iter()
            .find(|note| note.id == id)
    }

    // — Putting the notes back into the diff ——————————————————

    /// Recomputes the annotated lines of the displayed diff.
    ///
    /// Called when a diff arrives and on every change to the notes, never during
    /// a render: `relocate` walks the whole diff per note.
    pub(super) fn refresh_note_marks(&mut self, worktree: &Path) {
        let Some(state) = self.review.get_mut(worktree) else {
            return;
        };
        let (Some(diff), Some(path)) = (state.diff.clone(), state.selected.clone()) else {
            state.note_marks = std::rc::Rc::new(notes::Marks::default());
            state.drifted.clear();
            return;
        };
        let range = state.range.clone();
        let mut spans = Vec::new();
        let mut drifted = std::collections::HashSet::new();
        for note in state
            .notes
            .iter()
            .filter(|note| note.path == path && note.range == range && !note.done)
        {
            match notes::relocate(&diff, note).rows() {
                Some(span) => spans.push(span),
                None => {
                    drifted.insert(note.id);
                }
            }
        }
        state.note_marks = std::rc::Rc::new(notes::marks(&diff, &spans));
        state.drifted = drifted;
    }

    /// Opens a note's file and brings it into view.
    pub(super) fn reveal_note(&mut self, id: u64, cx: &mut Context<Self>) {
        let Some((path, range)) = self
            .note(id)
            .map(|note| (note.path.clone(), note.range.clone()))
        else {
            return;
        };
        let Some(worktree) = self.active.clone() else {
            return;
        };
        let already = self
            .active_review()
            .is_some_and(|state| state.selected.as_deref() == Some(path.as_path()));
        if !already {
            // The diff is not here yet: the selection will be set when it
            // arrives, by `Evt::FileDiff`, as for an arrow overflow. The flag is
            // set **after** the opening, which is precisely what clears jumps
            // armed by an earlier gesture.
            self.open_file(worktree.clone(), path, range, cx);
            if let Some(state) = self.review.get_mut(&worktree) {
                state.pending_note = Some(id);
            }
            return;
        }
        self.select_note_rows(id, cx);
    }

    /// Selects a note's lines in the diff already displayed.
    pub(super) fn select_note_rows(&mut self, id: u64, cx: &mut Context<Self>) {
        let split = crate::ui::settings::Settings::global(cx).diff_split;
        let Some(state) = self.active_review() else {
            return;
        };
        let (Some(diff), Some(note)) = (
            state.diff.clone(),
            state.notes.iter().find(|note| note.id == id).cloned(),
        ) else {
            return;
        };
        let Some((from, to)) = notes::relocate(&diff, &note).rows() else {
            self.announce(tr!("note-drifted"), cx);
            return;
        };
        // In two columns, the unified indices do not name the same entries: we
        // find the ones covering them.
        let shown = if split {
            split_span(&diff, from, to)
        } else {
            Some((from, to))
        };
        let Some((from, to)) = shown else { return };
        if let Some(state) = self.active_review_mut() {
            state.diff_selection = Some((from, to));
        }
        self.reveal_diff_row(from, gpui::ScrollStrategy::Center, cx);
        cx.notify();
    }

    // — Sending ————————————————————————————————————————————————

    /// Delivers notes to the worktree's agent.
    ///
    /// `only` names one note; without it, all those not yet handled. They move
    /// to `sent` and not to `done`: it is the review of the answer that closes
    /// them.
    pub(super) fn send_notes(
        &mut self,
        only: Option<u64>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(worktree) = self.active.clone() else {
            return;
        };
        let branch = self
            .active_worktree()
            .and_then(|w| w.branch.clone())
            .unwrap_or_else(|| tr!("branch-detached").to_string());
        let Some(state) = self.review.get(&worktree) else {
            return;
        };
        let chosen: Vec<Note> = state
            .notes
            .iter()
            .filter(|note| match only {
                Some(id) => note.id == id,
                None => !note.done,
            })
            .cloned()
            .collect();
        if chosen.is_empty() {
            self.announce(tr!("note-nothing-to-send"), cx);
            return;
        }
        let ids: Vec<u64> = chosen.iter().map(|note| note.id).collect();
        let text = notes::prompt(&branch, &chosen);
        self.confirm_prompt(worktree, ids, text, window, cx);
    }

    /// Shows the prompt before it goes out, and lets it be edited.
    ///
    /// What goes into a terminal cannot be taken back: an agent has read the
    /// paste before you have seen what you just sent. The dialog is also what
    /// recalls what a request looks like — you add in one sentence what the
    /// notes do not say.
    fn confirm_prompt(
        &mut self,
        worktree: PathBuf,
        ids: Vec<u64>,
        text: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let input = self.prompt_input.clone();
        let entity = cx.entity();
        input.update(cx, |input, cx| input.set_value(text, window, cx));
        window.open_dialog(cx, move |dialog, _window, _cx| {
            let input = input.clone();
            let entity = entity.clone();
            let (worktree, ids) = (worktree.clone(), ids.clone());
            dialog
                .title(tr!("note-prompt-title"))
                .child(
                    v_flex()
                        .gap_2()
                        .w(px(640.))
                        .child(div().text_xs().child(tr!("note-prompt-hint")))
                        .child(Textarea::new(&input)),
                )
                .overlay_closable(false)
                .close_button(false)
                .on_ok(move |_, window, cx| {
                    let text = input.read(cx).value().to_string();
                    entity.update(cx, |this, cx| {
                        this.send_prompt(worktree.clone(), ids.clone(), text, window, cx);
                    });
                    true
                })
        });
    }

    /// Delivers the prompt and marks the notes as sent.
    ///
    /// `sent` and not `done`: it is the review of the answer that closes a note,
    /// not its sending.
    fn send_prompt(
        &mut self,
        worktree: PathBuf,
        ids: Vec<u64>,
        text: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if text.trim().is_empty() {
            return;
        }
        let count = ids.len();
        self.deliver(worktree.clone(), text, window, cx);
        if let Some(state) = self.review.get_mut(&worktree) {
            for note in state.notes.iter_mut().filter(|note| ids.contains(&note.id)) {
                note.sent = true;
            }
        }
        self.persist_review(&worktree, cx);
        self.announce(tr!("note-sent", { count: count }), cx);
    }

    /// Asks a free question about the current selection.
    ///
    /// Without going through a note: it is the most frequent gesture in practice
    /// — you read, something puzzles you, you ask, and there is nothing to
    /// record.
    pub(super) fn ask_about_selection(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let split = crate::ui::settings::Settings::global(cx).diff_split;
        let Some(state) = self.active_review() else {
            return;
        };
        let (Some(diff), Some(path)) = (state.diff.clone(), state.selected.clone()) else {
            return;
        };
        // With no selection, the question is about the whole file: unlike a
        // note, it does not have to name a precise range.
        let (from, to) = match (split, state.diff_selection) {
            (true, Some((a, b))) => match diff.unified_span(a, b) {
                Some(span) => span,
                None => return,
            },
            (false, Some((a, b))) => (a.min(b), a.max(b)),
            (_, None) => (0, diff.rows.len().saturating_sub(1)),
        };
        let excerpt = diff.copy_text(from, to, false);
        let location = match notes::anchor_selection(&diff, from, to) {
            Some((_, start, end)) => format!("{}:{}", path.display(), span_label(start, end)),
            None => path.display().to_string(),
        };
        self.open_text_dialog(
            tr!("note-ask-title"),
            tr!("note-ask-placeholder"),
            window,
            cx,
            move |this, question, window, cx| {
                if question.trim().is_empty() {
                    return;
                }
                let Some(worktree) = this.active.clone() else {
                    return;
                };
                let text = notes::ask(&location, &path, &excerpt, &question);
                this.deliver(worktree, text, window, cx);
            },
        );
    }

    /// Delivers a text to the agent, opening the terminals panel.
    fn deliver(
        &mut self,
        worktree: PathBuf,
        text: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let group = self.terminal_group(&worktree, window, cx);
        group.update(cx, |group, cx| group.send_to_agent(text, window, cx));
        self.show_terminal_panel(window, cx);
    }

    // — The panel  ——————————————————————————————————————————————

    /// The "Notes" panel: the worktree's vault, in three sections.
    ///
    /// Three things are handled there, and they already live in the same folder
    /// — the tasks, the remarks, the reviewed files. Splitting them into three
    /// panels would make three tabs for a single subject; putting them in
    /// sub-tabs would need a click to know where the agent stands. Collapsible
    /// sections in a single scroll keep all three counts in view, and give the
    /// height back to whichever is being read.
    pub(super) fn render_notes(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let notes_scroll = self.scroll_of("notes");
        let find = self.render_find(crate::ui::find::Pane::Notes, cx);
        if self.active_review().is_none() {
            return empty_notes(tr!("no-worktree"), cx).into_any_element();
        }
        let bar = self.render_vault_bar(cx);
        let todo = self.render_todo_section(cx);
        let journal = self.render_journal_section(cx);
        let notes = self.render_notes_section(cx);
        let reviewed = self.render_reviewed_section(cx);

        v_flex()
            .size_full()
            .child(bar)
            .children(find)
            .child(
                div().flex_1().min_h_0().child(
                    self.scrolled(
                        "notes-bar",
                        &notes_scroll,
                        crate::ui::motion::Axes::Vertical,
                        window,
                        v_flex()
                            .id("notes-list")
                            .size_full()
                            .overflow_y_scroll()
                            .track_scroll(&notes_scroll)
                            .child(todo)
                            .child(journal)
                            .child(notes)
                            .children(reviewed),
                        cx,
                    ),
                ),
            )
            .into_any_element()
    }

    /// The panel's bar: what the three sections are about, that is, the vault's
    /// folder, and what is needed to open it where it is.
    ///
    /// The path appeared nowhere else, and a vault you cannot find again is a
    /// vault you do not open in Obsidian.
    fn render_vault_bar(&mut self, cx: &mut Context<Self>) -> impl IntoElement {
        let dir = self
            .active
            .clone()
            .and_then(|worktree| self.notes_dir(&worktree, cx));
        let label = dir
            .as_ref()
            .map(|dir| SharedString::from(dir.display().to_string()))
            .unwrap_or_else(|| tr!("note-no-vault"));
        h_flex()
            .h(crate::ui::theme::bar_height(cx))
            .w_full()
            .px_2()
            .gap_2()
            .items_center()
            .border_b_1()
            .border_color(cx.theme().border)
            .child(icon("book-open").xsmall())
            .child(
                div()
                    .flex_1()
                    .text_xs()
                    .text_color(cx.theme().muted_foreground)
                    .truncate()
                    .child(label.clone()),
            )
            .when_some(dir, |el, dir| {
                el.child(
                    Button::new("vault-open")
                        .ghost()
                        .xsmall()
                        .icon(icon("external-link"))
                        .tooltip(tr!("note-open-vault"))
                        .on_click(move |_, _window, cx| {
                            // `file://` rather than an editor: the gesture is
                            // "show me this folder", and it is the desktop's
                            // business to decide with what — the same path as a
                            // worktree's address button.
                            //
                            // The path is the server's; here, and here only,
                            // does it go back into the user's world — Windows
                            // Explorer knows nothing of `/home/…`, but it opens
                            // `\\wsl.localhost\…` perfectly well.
                            let dir = if cfg!(windows) {
                                let distro =
                                    crate::ui::settings::Settings::global(cx).wsl_distro.clone();
                                crate::wslpath::to_windows(&dir, &distro)
                            } else {
                                dir.clone()
                            };
                            cx.open_url(&format!("file://{}", dir.display()));
                        }),
                )
            })
    }

    /// A section's header: the chevron, the title, the count, the actions.
    ///
    /// The collapse is **in memory** and is not persisted: it is a reading
    /// posture, which changes several times during a review, not a preference
    /// one expects back the next day.
    fn section_header(
        &mut self,
        key: &'static str,
        glyph: &'static str,
        title: SharedString,
        count: SharedString,
        cx: &mut Context<Self>,
    ) -> gpui::Div {
        let collapsed = self.notes_collapsed.contains(key);
        h_flex()
            .w_full()
            .px_2()
            .py_1()
            .gap_2()
            .items_center()
            .bg(cx.theme().secondary)
            // The collapse is carried by the title, not by the whole row: the
            // section's buttons live on it, and a click on "send" would also
            // collapse what has just been acted on.
            .child(
                h_flex()
                    .id(SharedString::from(format!("notes-section-{key}")))
                    .flex_1()
                    .gap_2()
                    .items_center()
                    .cursor_pointer()
                    .on_click(cx.listener(move |this, _, _window, cx| {
                        if !this.notes_collapsed.remove(key) {
                            this.notes_collapsed.insert(key);
                        }
                        cx.notify();
                    }))
                    .child(
                        icon(if collapsed {
                            "chevron-right"
                        } else {
                            "chevron-down"
                        })
                        .xsmall(),
                    )
                    .child(icon(glyph).xsmall())
                    .child(div().text_xs().child(title))
                    .child(
                        div()
                            .flex_1()
                            .text_xs()
                            .text_color(cx.theme().muted_foreground)
                            .child(count),
                    ),
            )
    }

    fn collapsed(&self, key: &'static str) -> bool {
        self.notes_collapsed.contains(key)
    }

    /// The remarks, grouped by file.
    fn render_notes_section(&mut self, cx: &mut Context<Self>) -> impl IntoElement {
        let query = self.query(crate::ui::find::Pane::Notes, cx);
        let only_open = self.notes_only_open;
        let Some(state) = self.active_review() else {
            return div().into_any_element();
        };
        let drifted = state.drifted.clone();
        // The search covers the remark, the quoted code and the path: the three
        // things a note is found by.
        let notes: Vec<Note> = state
            .notes
            .iter()
            .filter(|note| !only_open || !note.done)
            .filter(|note| {
                crate::ui::find::matches(&query, &note.body)
                    || crate::ui::find::matches(&query, &note.excerpt)
                    || crate::ui::find::matches(&query, &note.path.to_string_lossy())
            })
            .cloned()
            .collect();
        let total = state.notes.len();
        let pending = state.notes.iter().filter(|note| !note.done).count();
        let mono = cx.theme().mono_font_family.clone();

        let header = self
            .section_header(
                "notes",
                "reply",
                tr!("panel-notes"),
                tr!("note-count", { count: pending }),
                cx,
            )
            .child(
                Button::new("notes-filter")
                    .ghost()
                    .xsmall()
                    .icon(icon(if only_open { "eye-off" } else { "eye" }))
                    .selected(only_open)
                    .tooltip(tr!("note-only-open"))
                    .on_click(cx.listener(|this, _, _window, cx| this.toggle_notes_filter(cx))),
            )
            .child(
                Button::new("notes-send-all")
                    .ghost()
                    .xsmall()
                    .icon(icon("send"))
                    .tooltip(tr!("note-send-all"))
                    .disabled(pending == 0)
                    .on_click(cx.listener(|this, _, window, cx| this.send_notes(None, window, cx))),
            );

        if self.collapsed("notes") {
            return v_flex().w_full().child(header).into_any_element();
        }

        if notes.is_empty() {
            let message = if total == 0 {
                tr!("note-empty")
            } else {
                tr!("note-all-done")
            };
            return v_flex()
                .w_full()
                .child(header)
                .child(section_empty(message, cx))
                .into_any_element();
        }

        // Grouped by file, in the order the notes were taken: a review is read
        // back in the order it was made.
        let mut groups: Vec<(PathBuf, Vec<Note>)> = Vec::new();
        for note in notes {
            match groups.last_mut() {
                Some((path, bucket)) if *path == note.path => bucket.push(note),
                _ => groups.push((note.path.clone(), vec![note])),
            }
        }

        // The rows are built ahead of time and not in a lazy closure:
        // `render_note` borrows the view *and* the context, which an iterator
        // consumed later in the same expression does not allow.
        let muted = cx.theme().muted_foreground;
        let mut sections = Vec::new();
        for (path, bucket) in groups {
            let mut rows = Vec::new();
            for note in bucket {
                rows.push(
                    self.render_note(note, &drifted, mono.clone(), cx)
                        .into_any_element(),
                );
            }
            sections.push(
                v_flex()
                    .child(
                        div()
                            .px_2()
                            .py_1()
                            .text_xs()
                            .font_family(mono.clone())
                            .text_color(muted)
                            .truncate()
                            .child(path.display().to_string()),
                    )
                    .children(rows),
            );
        }

        v_flex()
            .w_full()
            .child(header)
            .children(sections)
            .into_any_element()
    }

    /// The worktree's free note: `NOTES.md`, editable in place.
    ///
    /// Always there, with no gesture to open it: it is a scratchpad, and a
    /// scratchpad you have to create serves nobody. Empty, it does not exist on
    /// disk — the input therefore does not claim a file is waiting somewhere, it
    /// simply waits for something to be written in it.
    fn render_journal_section(&mut self, cx: &mut Context<Self>) -> impl IntoElement {
        let header = self.section_header(
            "journal",
            "file-text",
            tr!("journal-title"),
            SharedString::default(),
            cx,
        );
        if self.collapsed("journal") {
            return v_flex().w_full().child(header);
        }
        v_flex()
            .w_full()
            .child(header)
            .child(div().p_2().child(Textarea::new(&self.journal_input)))
    }

    /// The files ticked as reviewed, and what is needed to hand them back.
    ///
    /// They are ticked in the file lists; they could only be **unticked** there,
    /// file by file, or in the vault's Markdown. A review one wants to restart
    /// from scratch therefore took as many clicks as it has files.
    fn render_reviewed_section(&mut self, cx: &mut Context<Self>) -> Option<impl IntoElement> {
        let state = self.active_review()?;
        if state.reviewed.is_empty() {
            return None;
        }
        let mut reviewed = state.reviewed.clone();
        reviewed.sort_by(|a, b| a.path.cmp(&b.path));
        let worktree = self.active.clone()?;
        let mono = cx.theme().mono_font_family.clone();
        let muted = cx.theme().muted_foreground;

        let header = self
            .section_header(
                "reviewed",
                "check-check",
                tr!("note-reviewed"),
                SharedString::from(reviewed.len().to_string()),
                cx,
            )
            .child(
                Button::new("reviewed-clear")
                    .ghost()
                    .xsmall()
                    .icon(icon("delete"))
                    .tooltip(tr!("note-reviewed-clear"))
                    .on_click(cx.listener(|this, _, _window, cx| this.clear_reviewed(cx))),
            );

        if self.collapsed("reviewed") {
            return Some(v_flex().w_full().child(header));
        }

        let rows: Vec<_> = reviewed
            .into_iter()
            .enumerate()
            .map(|(index, item)| {
                let (worktree, range, path) =
                    (worktree.clone(), item.range.clone(), item.path.clone());
                h_flex()
                    .w_full()
                    .px_2()
                    .py_0p5()
                    .gap_2()
                    .items_center()
                    .child(
                        div()
                            .flex_1()
                            .text_xs()
                            .font_family(mono.clone())
                            .text_color(muted)
                            .truncate()
                            .child(item.path.display().to_string()),
                    )
                    .child(
                        div()
                            .text_xs()
                            .text_color(muted)
                            .child(format!("+{} −{}", item.added, item.removed)),
                    )
                    .child(
                        Button::new(("reviewed-undo", index))
                            .ghost()
                            .xsmall()
                            .icon(icon("check-check"))
                            .tooltip(tr!("action-unreview"))
                            .on_click(cx.listener(move |this, _, _window, cx| {
                                this.set_reviewed(
                                    worktree.clone(),
                                    range.clone(),
                                    vec![path.clone()],
                                    false,
                                    cx,
                                );
                            })),
                    )
            })
            .collect();

        Some(v_flex().w_full().child(header).children(rows))
    }

    /// The worktree's task list.
    ///
    /// It is **at the top** of the panel: it is what one looks at to know where
    /// the agent stands, and putting it under a three-hundred-note review would
    /// amount to never seeing it. Without a `TODO.md`, the section stays and
    /// carries the button that creates one — an empty state that says what to do
    /// beats an absent section nobody knows could exist.
    fn render_todo_section(&mut self, cx: &mut Context<Self>) -> impl IntoElement {
        let todo = self.active_review().and_then(|state| state.todo.clone());
        let count = match &todo {
            Some(todo) => tr!("todo-progress", { done: todo.done(), total: todo.tasks.len() }),
            None => tr!("todo-none"),
        };
        let header = self
            .section_header("todo", "check-check", tr!("todo-title"), count, cx)
            .child(
                Button::new("todo-add")
                    .ghost()
                    .xsmall()
                    .icon(icon("plus"))
                    .tooltip(tr!("todo-add"))
                    // The button only gives focus to the input row that is
                    // already there: two ways of adding a task that do not end
                    // up in the same place would be one too many.
                    .on_click(cx.listener(|this, _, window, cx| {
                        this.notes_collapsed.remove("todo");
                        gpui::Focusable::focus_handle(&this.task_input, cx).focus(window, cx);
                        cx.notify();
                    })),
            );
        if self.collapsed("todo") {
            return v_flex().w_full().child(header);
        }

        let muted = cx.theme().muted_foreground;
        let editing = self.task_editing;
        let rows: Vec<_> = todo
            .iter()
            .flat_map(|todo| todo.tasks.iter())
            .map(|task| {
                let (line, done) = (task.line, task.done);
                h_flex()
                    .w_full()
                    .px_2()
                    .py_0p5()
                    .gap_2()
                    .items_center()
                    // Two spaces of indentation in the file are worth one step
                    // here: an agent's subtask is one.
                    .pl(px(8. + 12. * task.depth.min(4) as f32))
                    .child(
                        Checkbox::new(("todo", line))
                            .checked(done)
                            .on_click(cx.listener(move |this, checked: &bool, _window, cx| {
                                this.toggle_task(line, *checked, cx)
                            })),
                    )
                    // A click on the label replaces it with its input, in its
                    // place in the list: it is the gesture of task lists
                    // everywhere else, and there is nothing to learn.
                    .child(match editing == Some(line) {
                        true => div()
                            .flex_1()
                            .child(Input::new(&self.task_edit_input).xsmall())
                            .into_any_element(),
                        false => div()
                            .id(("todo-label", line))
                            .flex_1()
                            .text_xs()
                            .cursor_text()
                            .when(done, |el| el.text_color(muted).line_through())
                            .on_click(cx.listener(move |this, _, window, cx| {
                                this.edit_task(line, window, cx)
                            }))
                            .child(task.label.clone())
                            .into_any_element(),
                    })
                    .child(
                        Button::new(("todo-remove", line))
                            .ghost()
                            .xsmall()
                            .icon(icon("trash-2"))
                            .tooltip(tr!("todo-remove"))
                            .on_click(
                                cx.listener(move |this, _, _window, cx| this.remove_task(line, cx)),
                            ),
                    )
            })
            .collect();

        v_flex()
            .w_full()
            .child(header)
            .children(rows)
            // The input row is **always** there, at the bottom of the list: it
            // is what replaces the dialog, and a task list is filled in one go
            // without picking the mouse back up in between.
            .child(
                h_flex()
                    .w_full()
                    .px_2()
                    .py_1()
                    .gap_2()
                    .items_center()
                    .child(icon("plus").xsmall().text_color(muted))
                    .child(div().flex_1().child(Input::new(&self.task_input).xsmall())),
            )
    }

    fn render_note(
        &mut self,
        note: Note,
        drifted: &std::collections::HashSet<u64>,
        mono: SharedString,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let id = note.id;
        let is_drifted = drifted.contains(&id);
        let muted = cx.theme().muted_foreground;
        v_flex()
            .id(("note", id as usize))
            .px_2()
            .py_1()
            .gap_1()
            .border_b_1()
            .border_color(cx.theme().border)
            .child(
                h_flex()
                    .gap_1()
                    .items_center()
                    .child(
                        Checkbox::new(("note-done", id as usize))
                            .checked(note.done)
                            .on_click({
                                let entity = cx.entity();
                                let done = note.done;
                                move |_, _window, cx| {
                                    entity.update(cx, |this, cx| this.set_note_done(id, !done, cx));
                                }
                            }),
                    )
                    .child(
                        div()
                            .id(("note-loc", id as usize))
                            .flex_1()
                            .text_xs()
                            .font_family(mono.clone())
                            .text_color(muted)
                            .cursor_pointer()
                            .truncate()
                            .on_click(cx.listener(move |this, _, _window, cx| {
                                this.reveal_note(id, cx);
                            }))
                            .child(span_label(note.start, note.end)),
                    )
                    // A note whose code can no longer be found stays in the
                    // list: losing it in silence would be worse than not having
                    // taken it.
                    .when(is_drifted, |el| {
                        el.child(
                            div()
                                .text_xs()
                                .text_color(cx.theme().warning)
                                .child(tr!("note-drifted")),
                        )
                    })
                    .when(note.sent, |el| {
                        el.child(div().text_color(muted).child(icon("check").xsmall()))
                    })
                    .child(
                        Button::new(("note-send", id as usize))
                            .ghost()
                            .xsmall()
                            .icon(icon("send"))
                            .tooltip(tr!("note-send"))
                            .on_click(cx.listener(move |this, _, window, cx| {
                                this.send_notes(Some(id), window, cx);
                            })),
                    )
                    .child(
                        Button::new(("note-edit", id as usize))
                            .ghost()
                            .xsmall()
                            .icon(icon("pencil"))
                            .tooltip(tr!("note-edit"))
                            .on_click(cx.listener(move |this, _, window, cx| {
                                this.edit_note(id, window, cx);
                            })),
                    )
                    .child(
                        Button::new(("note-delete", id as usize))
                            .ghost()
                            .xsmall()
                            .icon(icon("trash-2"))
                            .tooltip(tr!("note-delete"))
                            .on_click(cx.listener(move |this, _, _window, cx| {
                                this.delete_note(id, cx);
                            })),
                    ),
            )
            // The excerpt, truncated to a few lines: the panel is for finding a
            // note again, not for reading the file.
            .child(
                v_flex()
                    .text_xs()
                    .font_family(mono)
                    .text_color(muted)
                    .children(
                        excerpt_lines(&note.excerpt, EXCERPT_LINES)
                            .into_iter()
                            .map(|line| div().truncate().child(line)),
                    ),
            )
            .child(
                div()
                    .text_sm()
                    .when(note.done, |el| el.text_color(muted).line_through())
                    .child(SharedString::from(note.body.clone())),
            )
    }
}

/// Excerpt lines shown in the panel: enough to recognise the note, not enough
/// to read the file — the diff is there for that.
const EXCERPT_LINES: usize = 4;

/// Splits an excerpt into lines, truncated to `limit`.
///
/// One line per element and not a single text: gpui does not break a text on its
/// `\n`, and a six-line excerpt would show on one.
fn excerpt_lines(excerpt: &str, limit: usize) -> Vec<SharedString> {
    let mut lines: Vec<SharedString> = excerpt
        .lines()
        .take(limit)
        .map(|line| SharedString::from(line.to_string()))
        .collect();
    if excerpt.lines().count() > limit {
        lines.push(SharedString::new_static("…"));
    }
    lines
}

/// `120` or `120-134`: the form everybody can read.
fn span_label(start: usize, end: usize) -> String {
    if start == end {
        start.to_string()
    } else {
        format!("{start}-{end}")
    }
}

/// The column-view entries covering a unified range.
fn split_span(
    diff: &crate::ui::diff_view::Rendered,
    from: usize,
    to: usize,
) -> Option<(usize, usize)> {
    let mut bounds: Option<(usize, usize)> = None;
    for (index, row) in diff.split.iter().enumerate() {
        if row
            .unified()
            .any(|unified| unified >= from && unified <= to)
        {
            bounds = Some(match bounds {
                Some((a, b)) => (a.min(index), b.max(index)),
                None => (index, index),
            });
        }
    }
    bounds
}

/// The panel's empty state: an icon and a sentence, as everywhere else.
/// A section's empty state, inside the panel.
///
/// A grey line and not a full-height empty state: three sections share this
/// scroll, and the one with nothing must not push the other two out of sight.
fn section_empty(message: SharedString, cx: &Context<ClaudhubApp>) -> impl IntoElement {
    div()
        .px_2()
        .py_2()
        .text_xs()
        .text_color(cx.theme().muted_foreground)
        .child(message)
}

fn empty_notes(message: SharedString, cx: &Context<ClaudhubApp>) -> impl IntoElement {
    v_flex()
        .size_full()
        .items_center()
        .justify_center()
        .gap_2()
        .text_color(cx.theme().muted_foreground)
        .child(icon("reply"))
        .child(div().text_sm().child(message))
}
