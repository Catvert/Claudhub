//! The three-pane merge: ours, the outcome, theirs.
//!
//! What a conflict costs is not the editing, it is the reading: git leaves the
//! file with markers through it, and the markers say what disagrees without
//! ever saying what either side *did*. So the file is shown three times side by
//! side — our version, the merged outcome, theirs — and every decision is one
//! click in the divider between two columns.
//!
//! **The middle column is written to, not only picked from.** Two buttons
//! answer most chunks; the ones they cannot answer are the ones where both
//! sides added something to the *same* line, and no combination of "take this
//! side" writes what the file wants. So a chunk's outcome can be typed, in a
//! real editor, in place — **per chunk**, which is what keeps three columns
//! aligned where one buffer over the whole file could not.
//!
//! Nothing is written to disk before it is asked for: the file keeps its
//! markers until "Resolve", which writes the outcome and stages it in one go.
//!
//! The decisions themselves live in `ui::merge`, which knows nothing of gpui.
//! This file is the plumbing.

use std::path::{Path, PathBuf};
use std::rc::Rc;

use gpui::{
    div, prelude::*, px, Context, Entity, Focusable, Pixels, ScrollStrategy, SharedString, Window,
};
use gpui_component::{
    button::{Button, ButtonVariants},
    h_flex,
    input::{Editor, EditorState, InputEvent},
    v_flex, ActiveTheme, Disableable, Sizable,
};

use crate::runtime::{Action, Cmd};
use crate::tr;
use crate::ui::app::ClaudhubApp;
use crate::ui::diff_view::line_height;
use crate::ui::icons::icon;
use crate::ui::merge::{Kind, Merge, Row, Side};
use crate::ui::theme::DiffColors;

/// The editor open on one chunk's outcome.
pub(super) struct ChunkEditor {
    pub chunk: usize,
    /// Created **once**, when editing starts: an `EditorState` rebuilt in a
    /// render loses its caret and its selection on the first keystroke.
    pub input: Entity<EditorState>,
}

/// The file being merged, and everything the three columns are painted from.
///
/// One at a time, and it lives on the application rather than in the review
/// state: it is what the centre of the Git screen shows *instead of* a diff, and
/// choosing another file is what ends it.
pub(super) struct MergeState {
    pub worktree: PathBuf,
    pub path: PathBuf,
    /// `None` while the three versions are being read out of the index.
    pub merge: Option<Merge>,
    /// The rows as the columns show them. Behind an `Rc` for the same reason
    /// the diff is: the virtualised list's closure captures it, and it is
    /// rebuilt on a gesture — never on a frame.
    pub rows: Rc<Vec<Row>>,
    /// What the list actually holds, which is not quite the rows: the chunk
    /// being edited is **one** item as tall as it needs to be.
    pub items: Rc<Vec<Item>>,
    /// The chunk the arrows are standing on, and the one the margin rule marks.
    pub current: usize,
    pub editor: Option<ChunkEditor>,
    /// Why there is nothing to show: a binary file, or a conflict where one side
    /// deleted the file. The two buttons of the conflicts panel remain the way
    /// out, and the message says so.
    pub error: Option<String>,
}

/// One entry of the list.
///
/// Rows, except where an editor is open: a chunk being written into is a single
/// entry carrying its three columns at once, because an editor is one element
/// and cannot be sliced across the rows it covers. That is also why the list is
/// a `v_virtual_list` and not a `uniform_list` — the entries no longer all have
/// the same height, exactly as in the diff's wrapped mode.
#[derive(Clone, Copy)]
pub(super) enum Item {
    Row(usize),
    /// The chunk being edited, and how many lines tall it is.
    Editing {
        chunk: usize,
        lines: usize,
    },
}

impl MergeState {
    fn shows(&self, worktree: &Path, path: &Path) -> bool {
        self.worktree == worktree && self.path == path
    }

    fn editing(&self) -> Option<usize> {
        self.editor.as_ref().map(|editor| editor.chunk)
    }

    /// The item the given chunk starts at, for the arrows to scroll to.
    fn item_of(&self, chunk: usize) -> Option<usize> {
        self.items.iter().position(|item| match item {
            Item::Row(row) => self.rows.get(*row).is_some_and(|row| row.chunk == chunk),
            Item::Editing { chunk: at, .. } => *at == chunk,
        })
    }
}

/// Lays the rows out into entries, folding the edited chunk into one.
fn layout(merge: &Merge, rows: &[Row], editing: Option<usize>) -> Vec<Item> {
    let mut items = Vec::with_capacity(rows.len());
    let mut at = 0;
    while at < rows.len() {
        let chunk = rows[at].chunk;
        if editing == Some(chunk) {
            let lines = rows[at..]
                .iter()
                .take_while(|row| row.chunk == chunk)
                .count();
            // The editor may hold more lines than the two sides do, and the
            // entry has to be as tall as the tallest of the three — the height
            // is what the list reserves, and a short one draws the next entry
            // over the editor's last lines.
            let typed = merge
                .chunks
                .get(chunk)
                .map(|chunk| chunk.result().len())
                .unwrap_or(0);
            items.push(Item::Editing {
                chunk,
                lines: lines.max(typed).max(1),
            });
            at += lines;
            continue;
        }
        items.push(Item::Row(at));
        at += 1;
    }
    items
}

impl ClaudhubApp {
    /// Opens a conflicted file in the three-pane view.
    pub(super) fn open_merge(
        &mut self,
        worktree: PathBuf,
        path: PathBuf,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(state) = self.review.get_mut(&worktree) {
            // The file is selected as any other would be — that is what lights
            // its row in the lists — but no diff is asked for: what a diff of a
            // conflicted file shows is the markers.
            state.selected = Some(path.clone());
            state.diff = None;
            state.diff_selection = None;
            state.range = crate::git::DiffRange::Working;
        }
        self.merging = Some(MergeState {
            worktree: worktree.clone(),
            path: path.clone(),
            merge: None,
            rows: Rc::new(Vec::new()),
            items: Rc::new(Vec::new()),
            current: 0,
            editor: None,
            error: None,
        });
        self.git.send(Cmd::ReadMerge { worktree, path });
        self.reveal(crate::ui::workspace::Workspace::Git, window, cx);
        cx.notify();
    }

    /// The three versions have arrived — or the reason there are not three.
    pub(super) fn merge_arrived(
        &mut self,
        worktree: PathBuf,
        path: PathBuf,
        result: Result<crate::git::Stages, String>,
        cx: &mut Context<Self>,
    ) {
        if !self
            .merging
            .as_ref()
            // An answer about a file one has already left: dropped, not shown.
            .is_some_and(|state| state.shows(&worktree, &path))
        {
            return;
        }
        match result {
            Ok(stages) => {
                let merge = Merge::new(&stages.base, &stages.ours, &stages.theirs);
                let rows = merge.rows();
                let items = layout(&merge, &rows, None);
                // Straight to the first thing to decide: a conflict is often
                // one chunk in the middle of a long file, and opening at the
                // top means scrolling for it before the work can start.
                let first = merge.step(0, 1).unwrap_or(0);
                if let Some(state) = self.merging.as_mut() {
                    state.rows = Rc::new(rows);
                    state.items = Rc::new(items);
                    state.merge = Some(merge);
                    state.current = first;
                    state.error = None;
                }
                self.merge_reveal(first);
            }
            Err(message) => {
                if let Some(state) = self.merging.as_mut() {
                    state.merge = None;
                    state.rows = Rc::new(Vec::new());
                    state.items = Rc::new(Vec::new());
                    state.editor = None;
                    state.error = Some(message);
                }
            }
        }
        cx.notify();
    }

    /// Rebuilds what the columns show. Called on a gesture, never on a frame.
    fn merge_rebuild(&mut self) {
        let Some(state) = self.merging.as_mut() else {
            return;
        };
        let Some(merge) = state.merge.as_ref() else {
            return;
        };
        let rows = merge.rows();
        let items = layout(merge, &rows, state.editor.as_ref().map(|e| e.chunk));
        state.rows = Rc::new(rows);
        state.items = Rc::new(items);
    }

    fn merge_reveal(&mut self, chunk: usize) {
        let Some(state) = self.merging.as_ref() else {
            return;
        };
        if let Some(item) = state.item_of(chunk) {
            self.merge_scroll
                .scroll_to_item(item, ScrollStrategy::Center);
        }
    }

    /// Takes a side into a conflict, or takes it back out.
    pub(super) fn merge_toggle(&mut self, chunk: usize, side: Side, cx: &mut Context<Self>) {
        let Some(state) = self.merging.as_mut() else {
            return;
        };
        let Some(merge) = state.merge.as_mut() else {
            return;
        };
        merge.toggle(chunk, side);
        // The buttons and the editor are two ways of answering the same
        // question, and picking a side is answering it again: what was typed is
        // gone (`Merge::toggle` drops it), so the editor showing it goes too
        // rather than sitting there holding a text that no longer counts.
        if state.editing() == Some(chunk) {
            state.editor = None;
        }
        state.current = chunk;
        self.merge_rebuild();
        cx.notify();
    }

    pub(super) fn merge_take_all(&mut self, side: Side, cx: &mut Context<Self>) {
        let Some(state) = self.merging.as_mut() else {
            return;
        };
        let Some(merge) = state.merge.as_mut() else {
            return;
        };
        merge.take_all(side);
        self.merge_rebuild();
        cx.notify();
    }

    /// Walks the conflicts, wrapping round. `delta` is a direction, not a count.
    pub(super) fn merge_step(&mut self, delta: isize, cx: &mut Context<Self>) {
        let Some(state) = self.merging.as_ref() else {
            return;
        };
        let Some(merge) = state.merge.as_ref() else {
            return;
        };
        let Some(next) = merge.step(state.current, delta) else {
            return;
        };
        if let Some(state) = self.merging.as_mut() {
            state.current = next;
        }
        self.merge_reveal(next);
        cx.notify();
    }

    /// Goes to the first chunk still waiting on a decision.
    ///
    /// "The first still open" and not "the next one down": what one walks here
    /// is a list that gets shorter, and after answering three chunks the fourth
    /// is where one wants to be, whichever order they were answered in.
    pub(super) fn merge_reveal_next(&mut self, cx: &mut Context<Self>) {
        let Some(state) = self.merging.as_ref() else {
            return;
        };
        let Some(merge) = state.merge.as_ref() else {
            return;
        };
        let Some(open) = merge.chunks.iter().position(|chunk| !chunk.resolved()) else {
            return;
        };
        if let Some(state) = self.merging.as_mut() {
            state.current = open;
        }
        self.merge_reveal(open);
        cx.notify();
    }

    /// Opens an editor on a chunk's outcome, seeded with what it says now.
    pub(super) fn merge_edit(&mut self, chunk: usize, window: &mut Window, cx: &mut Context<Self>) {
        let Some(state) = self.merging.as_ref() else {
            return;
        };
        let Some(merge) = state.merge.as_ref() else {
            return;
        };
        if state.editing() == Some(chunk) {
            return;
        }
        let text = merge.resolution(chunk);
        // The language of the file being merged: it is code in there, and the
        // same table the diff and the editor read.
        let language = crate::ui::highlight::language_for_path(&state.path).unwrap_or("text");
        let input = cx.new(|cx| {
            EditorState::new(window, cx)
                .language(language)
                .default_value(text)
        });
        // Every keystroke lands in the chunk: there is no "save" here, the
        // outcome being a text the "Resolve" button reads at the end. The rows
        // are rebuilt with it, which is what makes the entry grow as one types.
        cx.subscribe(&input, move |this, editor, event, cx| {
            if !matches!(event, InputEvent::Change) {
                return;
            }
            let text = editor.read(cx).value().to_string();
            if let Some(merge) = this.merging.as_mut().and_then(|state| state.merge.as_mut()) {
                merge.set_manual(chunk, &text);
            }
            this.merge_rebuild();
            cx.notify();
        })
        .detach();
        // And the caret goes there: an editor one has to click into before
        // typing is an editor that opened for nothing.
        let handle = input.read(cx).focus_handle(cx);
        window.focus(&handle, cx);
        if let Some(state) = self.merging.as_mut() {
            state.editor = Some(ChunkEditor { chunk, input });
            state.current = chunk;
        }
        self.merge_rebuild();
        self.merge_reveal(chunk);
        cx.notify();
    }

    /// Closes the editor. What was typed stays: it is the chunk's answer.
    pub(super) fn merge_stop_editing(&mut self, cx: &mut Context<Self>) {
        if let Some(state) = self.merging.as_mut() {
            state.editor = None;
        }
        self.merge_rebuild();
        cx.notify();
    }

    /// Gives a chunk back to the buttons, dropping what was typed into it.
    pub(super) fn merge_reset(&mut self, chunk: usize, cx: &mut Context<Self>) {
        if let Some(merge) = self.merging.as_mut().and_then(|state| state.merge.as_mut()) {
            merge.clear_manual(chunk);
        }
        if let Some(state) = self.merging.as_mut() {
            state.editor = None;
        }
        self.merge_rebuild();
        cx.notify();
    }

    /// Writes the outcome and stages it, which is what marks the file resolved.
    pub(super) fn merge_apply(&mut self, cx: &mut Context<Self>) {
        let Some(state) = self.merging.as_ref() else {
            return;
        };
        let Some(merge) = state.merge.as_ref() else {
            return;
        };
        if merge.unresolved() > 0 {
            return;
        }
        let (worktree, path) = (state.worktree.clone(), state.path.clone());
        let cmd = Cmd::ResolveWith {
            worktree: worktree.clone(),
            path: path.clone(),
            content: merge.text(),
        };
        self.start(Some(worktree.clone()), Action::Resolve, cmd, cx);
        // And the centre goes back to being a diff: the file is staged, so what
        // it now shows is the merge one has just made, read the way every other
        // change of this window is read.
        self.merging = None;
        self.open_file(worktree, path, crate::git::DiffRange::Working, cx);
    }

    /// Whether the three-pane view is what the centre is showing, which is what
    /// makes the review's four moves belong to it.
    pub(super) fn merging_shown(&self) -> bool {
        let Some(state) = self.merging.as_ref() else {
            return false;
        };
        self.active.as_deref() == Some(state.worktree.as_path())
            && self
                .active_review()
                .and_then(|review| review.selected.clone())
                .is_some_and(|selected| selected == state.path)
    }

    /// The three-pane view, when a conflicted file is open.
    pub(super) fn render_merge(&mut self, cx: &mut Context<Self>) -> Option<gpui::AnyElement> {
        if !self.merging_shown() {
            return None;
        }
        let state = self.merging.as_ref()?;
        let path = state.path.clone();
        let rows = state.rows.clone();
        let items = state.items.clone();
        let current = state.current;
        let editor = state.editor.as_ref().map(|editor| editor.input.clone());
        let editing = state.editing();
        let error = state.error.clone();
        let (unresolved, conflicts) = state
            .merge
            .as_ref()
            .map(|merge| (merge.unresolved(), merge.conflicts()))
            .unwrap_or((0, 0));
        let loading = state.merge.is_none() && error.is_none();

        let edited_current = state
            .merge
            .as_ref()
            .and_then(|merge| merge.chunks.get(current))
            .is_some_and(|chunk| chunk.edited());
        let header = self.render_merge_header(
            &path,
            Progress {
                unresolved,
                conflicts,
                editing,
                edited_current,
                usable: error.is_none(),
            },
            cx,
        );
        let font_size = px(crate::ui::settings::Settings::global(cx).diff_font_size);
        let line = line_height(font_size);
        let mono = cx.theme().mono_font_family.clone();

        if let Some(message) = error {
            return Some(
                v_flex()
                    .size_full()
                    .child(header)
                    .child(crate::ui::diff_view::hint(SharedString::from(message), cx))
                    .into_any_element(),
            );
        }
        if loading {
            return Some(
                v_flex()
                    .size_full()
                    .child(header)
                    .child(crate::ui::diff_view::hint(tr!("review-loading"), cx))
                    .into_any_element(),
            );
        }

        let colors = DiffColors::of(cx);
        let entity = cx.entity();
        let number_width = px(48.);
        let titles = self.render_merge_titles(number_width, cx);
        let rule = cx.theme().primary;
        let sizes = Rc::new(
            items
                .iter()
                .map(|item| {
                    let lines = match item {
                        Item::Row(_) => 1,
                        Item::Editing { lines, .. } => *lines,
                    };
                    gpui::size(px(0.), line * lines as f32)
                })
                .collect::<Vec<_>>(),
        );
        let paint = Paint {
            rows,
            items,
            colors,
            number_width,
            line,
            current,
            rule,
            editor,
            font: mono.clone(),
            font_size,
        };
        let scroll = self.merge_scroll.clone();
        let list = crate::ui::scroll::vertical(
            "merge-lines",
            &scroll,
            gpui_component::v_virtual_list(
                cx.entity(),
                "merge-rows",
                sizes,
                move |_, range, _window, cx| {
                    range
                        .map(|index| render_item(&paint, index, &entity, cx))
                        .collect::<Vec<_>>()
                },
            )
            .size_full()
            .font_family(mono)
            .text_size(font_size)
            .track_scroll(&scroll),
        );

        Some(
            v_flex()
                .size_full()
                .child(header)
                .child(titles)
                .child(div().flex_1().min_h_0().child(list))
                .into_any_element(),
        )
    }

    /// The bar: the file, **what is left to decide**, and the gestures that act
    /// on the file as a whole.
    fn render_merge_header(
        &self,
        path: &Path,
        state: Progress,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let Progress {
            unresolved,
            conflicts,
            editing,
            edited_current,
            usable,
        } = state;
        let (warning, success) = (cx.theme().warning, cx.theme().success);
        let mono = cx.theme().mono_font_family.clone();
        let for_editor = path.to_path_buf();
        let current = self
            .merging
            .as_ref()
            .map(|state| state.current)
            .unwrap_or(0);
        // What is left, in words and not in a colour: a view one leaves half
        // done writes a file one has not finished reading.
        let progress = h_flex()
            .flex_shrink_0()
            .gap_1()
            .items_center()
            .text_xs()
            .when(unresolved > 0, |el| {
                el.text_color(warning)
                    .child(icon("alert-circle").xsmall())
                    .child(tr!("merge-left-to-decide", { count: unresolved }))
            })
            .when(unresolved == 0 && usable, |el| {
                el.text_color(success)
                    .child(icon("check").xsmall())
                    .child(tr!("merge-all-decided", { count: conflicts }))
            });

        h_flex()
            .h(crate::ui::theme::bar_height(cx))
            .w_full()
            .px_2()
            .gap_2()
            .items_center()
            .overflow_hidden()
            .border_b_1()
            .border_color(cx.theme().border)
            .child(icon("git-merge").xsmall().text_color(warning))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .text_sm()
                    .font_family(mono)
                    .truncate()
                    .child(path.display().to_string()),
            )
            .child(progress)
            .child(
                h_flex()
                    .flex_shrink_0()
                    .gap_1()
                    .when(usable && conflicts > 0, |el| {
                        // The two moves, and they are the ones the review's
                        // arrows make elsewhere in this window: the same
                        // gesture, on what has taken the diff's place.
                        el.child(
                            Button::new("merge-prev")
                                .ghost()
                                .xsmall()
                                .icon(icon("arrow-up"))
                                .tooltip(tr!("merge-previous-conflict"))
                                .on_click(
                                    cx.listener(|this, _, _window, cx| this.merge_step(-1, cx)),
                                ),
                        )
                        .child(
                            Button::new("merge-next")
                                .ghost()
                                .xsmall()
                                .icon(icon("arrow-down"))
                                .tooltip(tr!("merge-next-conflict"))
                                .on_click(
                                    cx.listener(|this, _, _window, cx| this.merge_step(1, cx)),
                                ),
                        )
                        .when(unresolved > 0, |el| {
                            el.child(
                                Button::new("merge-open")
                                    .ghost()
                                    .xsmall()
                                    .icon(icon("crosshair"))
                                    .tooltip(tr!("merge-next-open"))
                                    .on_click(cx.listener(|this, _, _window, cx| {
                                        this.merge_reveal_next(cx)
                                    })),
                            )
                        })
                    })
                    .when(usable, |el| {
                        el.child(
                            Button::new("merge-all-ours")
                                .ghost()
                                .xsmall()
                                .label(tr!("merge-take-all-ours"))
                                .on_click(cx.listener(|this, _, _window, cx| {
                                    this.merge_take_all(Side::Ours, cx)
                                })),
                        )
                        .child(
                            Button::new("merge-all-theirs")
                                .ghost()
                                .xsmall()
                                .label(tr!("merge-take-all-theirs"))
                                .on_click(cx.listener(|this, _, _window, cx| {
                                    this.merge_take_all(Side::Theirs, cx)
                                })),
                        )
                        // Writing the outcome by hand is a first-class answer,
                        // so it has a button and not only a double click.
                        .when(edited_current, |el| {
                            el.child(
                                Button::new("merge-reset")
                                    .ghost()
                                    .xsmall()
                                    .icon(icon("eraser"))
                                    .tooltip(tr!("merge-reset-chunk"))
                                    .on_click(cx.listener(move |this, _, _window, cx| {
                                        this.merge_reset(current, cx)
                                    })),
                            )
                        })
                        .child(match editing {
                            Some(_) => Button::new("merge-write")
                                .primary()
                                .xsmall()
                                .icon(icon("pencil"))
                                .tooltip(tr!("merge-stop-editing"))
                                .on_click(
                                    cx.listener(|this, _, _window, cx| this.merge_stop_editing(cx)),
                                ),
                            None => Button::new("merge-write")
                                .ghost()
                                .xsmall()
                                .icon(icon("pencil"))
                                .tooltip(tr!("merge-edit-chunk"))
                                .on_click(cx.listener(move |this, _, window, cx| {
                                    this.merge_edit(current, window, cx)
                                })),
                        })
                    })
                    .child(
                        Button::new("merge-external")
                            .ghost()
                            .xsmall()
                            .icon(icon("external-link"))
                            .tooltip(tr!("merge-open-in-editor"))
                            .on_click(cx.listener(move |this, _, _window, cx| {
                                // The markers are still on disk, which is what
                                // one wants there: finishing by hand outside
                                // this view means reading them.
                                this.merging = None;
                                this.open_in_editor(for_editor.clone(), cx);
                            })),
                    )
                    .when(usable, |el| {
                        el.child(
                            Button::new("merge-apply")
                                .primary()
                                .xsmall()
                                .disabled(unresolved > 0)
                                .label(tr!("merge-resolve"))
                                .on_click(cx.listener(|this, _, _window, cx| this.merge_apply(cx))),
                        )
                    }),
            )
    }

    /// Which version each column holds. Three words, and they are needed: left
    /// and right are only "ours" and "theirs" if something says so.
    fn render_merge_titles(
        &self,
        number_width: Pixels,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let muted = cx.theme().muted_foreground;
        let title = |key: &'static str| {
            div()
                .flex_1()
                .min_w_0()
                .px_1()
                .text_xs()
                .text_color(muted)
                .truncate()
                .child(tr!(key))
        };
        h_flex()
            .w_full()
            .py_0p5()
            .items_center()
            .border_b_1()
            .border_color(cx.theme().border)
            .child(div().w(RULE).flex_none())
            .child(div().w(number_width).flex_none())
            .child(title("merge-column-ours"))
            .child(div().w(DIVIDER).flex_none())
            .child(div().w(number_width).flex_none())
            .child(title("merge-column-result"))
            .child(div().w(DIVIDER).flex_none())
            .child(div().w(number_width).flex_none())
            .child(title("merge-column-theirs"))
    }
}

/// Where the merge stands, which is what the bar says and what its buttons act
/// on. A struct rather than five arguments in a row: they are read together.
#[derive(Clone, Copy)]
struct Progress {
    unresolved: usize,
    conflicts: usize,
    /// The chunk whose editor is open, if one is.
    editing: Option<usize>,
    /// The chunk the arrows stand on has been written into by hand.
    edited_current: bool,
    /// There are three columns to show. False for a binary file, or a conflict
    /// where one side deleted the file: the bar then carries only the way out.
    usable: bool,
}

/// The width of a divider: a column of its own, holding one button per conflict
/// and nothing else. Fixed, because it is what the buttons need and because a
/// divider that grew with the window would put them out of reach of the text
/// they act on.
const DIVIDER: Pixels = px(26.);
/// The margin rule that marks the chunk the arrows are standing on. Always
/// there, transparent elsewhere: a width that appeared would shift every line.
const RULE: Pixels = px(3.);

/// Everything an entry is painted from, gathered once per frame rather than
/// looked up per entry.
struct Paint {
    rows: Rc<Vec<Row>>,
    items: Rc<Vec<Item>>,
    colors: DiffColors,
    number_width: Pixels,
    line: Pixels,
    current: usize,
    rule: gpui::Hsla,
    editor: Option<Entity<EditorState>>,
    font: SharedString,
    font_size: Pixels,
}

/// Which column a cell belongs to, which is all that decides how a chunk tints
/// it.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Column {
    Ours,
    Result,
    Theirs,
}

fn render_item(
    paint: &Paint,
    index: usize,
    entity: &Entity<ClaudhubApp>,
    cx: &mut gpui::App,
) -> gpui::AnyElement {
    match paint.items.get(index) {
        Some(Item::Row(row)) => render_row(paint, *row, entity, cx),
        Some(Item::Editing { chunk, lines }) => render_editing(paint, *chunk, *lines, entity, cx),
        None => div().into_any_element(),
    }
}

/// What a chunk's two sides say, taken off its rows: the entry that carries an
/// editor paints them itself, the list having folded them into one.
fn chunk_rows(paint: &Paint, chunk: usize) -> Vec<&Row> {
    paint.rows.iter().filter(|row| row.chunk == chunk).collect()
}

/// The chunk being written into: its two sides, and an editor between them.
fn render_editing(
    paint: &Paint,
    chunk: usize,
    lines: usize,
    entity: &Entity<ClaudhubApp>,
    cx: &mut gpui::App,
) -> gpui::AnyElement {
    let rows = chunk_rows(paint, chunk);
    let Some(first) = rows.first().copied() else {
        return div().into_any_element();
    };
    let (mine, yours) = picked(entity, chunk, cx);
    let height = paint.line * lines as f32;
    let editor = paint.editor.clone();
    let ours = side(paint, &rows, Column::Ours, mine, yours, height, cx);
    let theirs = side(paint, &rows, Column::Theirs, mine, yours, height, cx);
    h_flex()
        .id(("merge-editing", chunk))
        .w_full()
        .h(height)
        .items_start()
        .child(rule(paint, chunk, height))
        .child(ours)
        .child(divider(
            first,
            Side::Ours,
            mine,
            paint.line,
            entity,
            "merge-take-ours",
        ))
        .child(
            div()
                .flex_1()
                .min_w_0()
                .h(height)
                .border_1()
                .border_color(cx.theme().primary)
                .children(editor.map(|input| {
                    Editor::new(&input)
                        .appearance(false)
                        .font_family(paint.font.clone())
                        .text_size(paint.font_size)
                        // The line height is said explicitly, as everywhere
                        // else code is shown: `Input`'s is rem-based and
                        // therefore deaf to the text size, and here it would
                        // also put the editor's lines out of step with the two
                        // columns beside it — which is the one thing this view
                        // exists to keep.
                        .line_height(paint.line)
                        .h_full()
                })),
        )
        .child(divider(
            first,
            Side::Theirs,
            yours,
            paint.line,
            entity,
            "merge-take-theirs",
        ))
        .child(theirs)
        .into_any_element()
}

/// One side of the chunk being edited, its lines stacked: the entry paints them
/// itself, the list having folded the chunk's rows into one.
#[allow(clippy::too_many_arguments)]
fn side(
    paint: &Paint,
    rows: &[&Row],
    column: Column,
    mine: bool,
    yours: bool,
    height: Pixels,
    cx: &mut gpui::App,
) -> impl IntoElement {
    let mut stack = v_flex().flex_1().min_w_0().h(height);
    for row in rows {
        stack = stack.child(cell(paint, row, column, mine, yours, cx));
    }
    stack
}

/// One row of the three columns.
fn render_row(
    paint: &Paint,
    index: usize,
    entity: &Entity<ClaudhubApp>,
    cx: &mut gpui::App,
) -> gpui::AnyElement {
    let Some(row) = paint.rows.get(index) else {
        return div().into_any_element();
    };
    let (mine, yours) = picked(entity, row.chunk, cx);
    let chunk = row.chunk;
    let for_edit = entity.clone();
    h_flex()
        .id(("merge-row", index))
        .h(paint.line)
        .w_full()
        .items_start()
        .whitespace_nowrap()
        // A double click on the outcome opens an editor on that chunk: it is
        // the gesture one makes at a text one wants to change, and the button
        // in the bar is there for whoever would rather be told.
        .on_mouse_down(gpui::MouseButton::Left, move |event, window, cx| {
            if event.click_count < 2 {
                return;
            }
            for_edit.update(cx, |this, cx| this.merge_edit(chunk, window, cx));
        })
        .child(rule(paint, row.chunk, paint.line))
        .child(cell(paint, row, Column::Ours, mine, yours, cx))
        .child(divider(
            row,
            Side::Ours,
            mine,
            paint.line,
            entity,
            "merge-take-ours",
        ))
        .child(cell(paint, row, Column::Result, mine, yours, cx))
        .child(divider(
            row,
            Side::Theirs,
            yours,
            paint.line,
            entity,
            "merge-take-theirs",
        ))
        .child(cell(paint, row, Column::Theirs, mine, yours, cx))
        .into_any_element()
}

/// Which sides a chunk has been given, read off the application: it changes on
/// a click, and the closure below runs for every visible entry of every frame.
fn picked(entity: &Entity<ClaudhubApp>, chunk: usize, cx: &gpui::App) -> (bool, bool) {
    let chunk = entity
        .read(cx)
        .merging
        .as_ref()
        .and_then(|state| state.merge.as_ref())
        .and_then(|merge| merge.chunks.get(chunk));
    match chunk {
        Some(chunk) => (chunk.takes(Side::Ours), chunk.takes(Side::Theirs)),
        None => (false, false),
    }
}

/// The margin rule of the chunk one is standing on.
fn rule(paint: &Paint, chunk: usize, height: Pixels) -> impl IntoElement {
    div()
        .w(RULE)
        .flex_none()
        .h(height)
        .when(chunk == paint.current, |el| el.bg(paint.rule))
}

/// One column of one row: its line number and its text, tinted by what became
/// of the chunk it belongs to.
fn cell(
    paint: &Paint,
    row: &Row,
    column: Column,
    mine: bool,
    yours: bool,
    cx: &mut gpui::App,
) -> gpui::AnyElement {
    let content = match column {
        Column::Ours => row.ours.as_ref(),
        Column::Result => row.result.as_ref(),
        Column::Theirs => row.theirs.as_ref(),
    };
    let background = tint(row, column, &paint.colors, mine, yours);
    let number = div()
        .w(paint.number_width)
        .flex_none()
        .h(paint.line)
        .px_1()
        .text_right()
        .text_color(paint.colors.line_number)
        .child(match content {
            Some(line) => line.number.to_string(),
            None => String::new(),
        });
    let text = div()
        .flex_1()
        .min_w_0()
        .h(paint.line)
        .px_1()
        .overflow_hidden()
        .when_some(background, |el, colour| el.bg(colour))
        // A column with nothing on this row is not an empty line, and the two
        // have to be told apart: the tint is what says "this side has nothing
        // here", which is exactly what one is deciding about.
        .when(content.is_none(), |el| el.bg(paint.colors.absent_bg))
        .when(
            content.is_none() && row.kind.is_conflict() && column == Column::Result,
            |el| el.text_color(cx.theme().muted_foreground),
        )
        .child(match content {
            Some(line) => SharedString::from(line.text.clone()),
            None => SharedString::default(),
        });
    h_flex()
        .flex_1()
        .min_w_0()
        .items_start()
        .child(number)
        .child(text)
        .into_any_element()
}

/// What tints a cell, and it is the whole legend of this view: green where a
/// side is taken, red where a conflict is still open, blue where the merge
/// settled it on its own.
fn tint(
    row: &Row,
    column: Column,
    colors: &DiffColors,
    mine: bool,
    yours: bool,
) -> Option<gpui::Hsla> {
    match row.kind {
        Kind::Stable => None,
        Kind::Ours => match column {
            Column::Theirs => None,
            _ => Some(colors.added_bg),
        },
        Kind::Theirs => match column {
            Column::Ours => None,
            _ => Some(colors.added_bg),
        },
        Kind::Both => Some(colors.hunk_bg),
        Kind::Conflict => {
            let answered = mine || yours || row.edited;
            match column {
                // A side that has been picked is lit; the one left out goes
                // back to being ordinary text, which is what says it was left
                // out on purpose rather than not yet looked at.
                Column::Ours if answered => mine.then_some(colors.added_bg),
                Column::Theirs if answered => yours.then_some(colors.added_bg),
                Column::Result if answered => Some(colors.added_bg),
                _ => Some(colors.removed_bg),
            }
        }
    }
}

/// The gap between two columns: a button on a conflict's first row, nothing
/// anywhere else.
fn divider(
    row: &Row,
    side: Side,
    taken: bool,
    line: Pixels,
    entity: &Entity<ClaudhubApp>,
    tooltip: &'static str,
) -> gpui::AnyElement {
    let mut wrapper = div().w(DIVIDER).flex_none().h(line);
    if !row.kind.is_conflict() || !row.first {
        return wrapper.into_any_element();
    }
    let chunk = row.chunk;
    let entity = entity.clone();
    let name = match side {
        Side::Ours => "chevron-right",
        Side::Theirs => "chevron-left",
    };
    wrapper = wrapper.child(
        Button::new(("merge-take", chunk * 2 + usize::from(side == Side::Theirs)))
            .xsmall()
            .when(taken, |button| button.primary())
            .when(!taken, |button| button.ghost())
            .icon(icon(name))
            .tooltip(tr!(tooltip))
            .on_click(move |_, _window, cx| {
                entity.update(cx, |this, cx| this.merge_toggle(chunk, side, cx));
            }),
    );
    wrapper.into_any_element()
}
