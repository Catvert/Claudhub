//! The "Stashes" panel: the work put aside, and the four ways of taking it
//! back.
//!
//! It is a tab beside "History" and "Tags", and it belongs there for the same
//! reason they do: those three answer "what happened", where the tabs above
//! choose the file being read. A stash is a commit like the others — clicking a
//! row shows its diff in the centre, exactly as clicking a tag or a commit
//! does.
//!
//! **The stack is shared by every worktree**, because `refs/stash` lives in the
//! common `.git`; the list is therefore filed under the main repository, as the
//! tags are. What is *not* shared is where a stash lands: restoring one is a
//! gesture of the checkout being looked at, and the panel says which.
//!
//! **Every gesture carries the commit hash the row was showing.** `stash@{1}`
//! becomes `stash@{0}` the moment anything — another worktree, a terminal
//! alongside — drops the entry above it, and git addresses a drop by nothing
//! but that name. The check lives in `git::stash`; what matters here is that
//! the panel never sends a bare position.

use std::path::PathBuf;
use std::rc::Rc;

use gpui::{div, prelude::*, px, uniform_list, App, Context, Entity, SharedString, Window};
use gpui_component::{
    button::{Button, ButtonVariants},
    checkbox::Checkbox,
    h_flex,
    input::{Input, InputState},
    menu::{ContextMenuExt, PopupMenu, PopupMenuItem},
    v_flex, ActiveTheme, Disableable, Sizable, WindowExt,
};

use crate::git::Stash;
use crate::runtime::{Action, Cmd};
use crate::tr;
use crate::ui::app::ClaudhubApp;
use crate::ui::find::Pane;
use crate::ui::icons::icon;

/// What is known of a repository's stashes.
#[derive(Default)]
pub struct StashesState {
    /// Behind an `Rc` for the tags' reason: the row closure runs for every
    /// visible row on every frame and cannot read the application back, so it
    /// captures the list.
    pub stashes: Rc<Vec<Stash>>,
    /// A read has gone out and has not come back — the guard without which
    /// every frame would restart the command, the panel being what asks and
    /// asking at render time.
    pub pending: bool,
    /// The list has come back at least once. An empty stack and a stack never
    /// read are the same thing to look at and two different things to act on.
    pub loaded: bool,
}

/// The stash being made, while the dialog is open.
///
/// An entity of its own and not a field of `ClaudhubApp`, like `TagDraft`: the
/// closure `open_dialog` keeps is called back from the root view's render, in
/// the middle of a borrow of the application.
pub struct StashDraft {
    pub message: Entity<InputState>,
    /// Take the files git does not know yet.
    ///
    /// **On by default**, which is where this differs from the command line: a
    /// stash that leaves the new files on the disk is the surprise everyone has
    /// had once — the tree reads as clean, the build still fails, and the
    /// reason is a file the stash politely left behind.
    pub untracked: bool,
    /// Leave what was staged staged: for putting aside only what is not going
    /// into the commit being written.
    pub keep_index: bool,
}

impl Render for StashDraft {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let (untracked, keep_index) = (self.untracked, self.keep_index);
        v_flex()
            .w(px(420.))
            .gap_2()
            .child(Input::new(&self.message))
            .child(
                div()
                    .text_xs()
                    .text_color(cx.theme().muted_foreground)
                    .child(tr!("stash-message-help")),
            )
            .child(
                Checkbox::new("stash-untracked")
                    .label(tr!("stash-untracked"))
                    .checked(untracked)
                    .on_click(cx.listener(|this, _, _window, cx| {
                        this.untracked = !this.untracked;
                        cx.notify();
                    })),
            )
            .child(
                Checkbox::new("stash-keep-index")
                    .label(tr!("stash-keep-index"))
                    .checked(keep_index)
                    .on_click(cx.listener(|this, _, _window, cx| {
                        this.keep_index = !this.keep_index;
                        cx.notify();
                    })),
            )
    }
}

impl ClaudhubApp {
    /// The stashes of the repository the selected worktree belongs to.
    pub(super) fn stashes_of_active(&self) -> Option<&StashesState> {
        self.stashes.get(&self.active_main()?)
    }

    /// Asks for the list, once, at render time — the tags' guard, for the tags'
    /// reason: opening a worktree must not pay for a read nobody will look at.
    fn ensure_stashes(&mut self, main: PathBuf, cx: &mut Context<Self>) {
        let state = self.stashes.entry(main.clone()).or_default();
        if state.pending || state.loaded {
            return;
        }
        state.pending = true;
        self.git.send(Cmd::LoadStashes { main });
        cx.notify();
    }

    pub(super) fn stashes_arrived(
        &mut self,
        main: PathBuf,
        stashes: Vec<Stash>,
        cx: &mut Context<Self>,
    ) {
        let state = self.stashes.entry(main).or_default();
        state.stashes = Rc::new(stashes);
        state.pending = false;
        state.loaded = true;
        cx.notify();
    }

    pub(super) fn refresh_stashes(&mut self, cx: &mut Context<Self>) {
        let Some(main) = self.active_main() else {
            return;
        };
        let state = self.stashes.entry(main.clone()).or_default();
        state.pending = true;
        self.git.send(Cmd::LoadStashes { main });
        cx.notify();
    }

    /// Opens the dialog that puts the changes aside.
    ///
    /// Reachable from two places, and deliberately: the panel's own bar, and the
    /// changes bar right above the list of what would be stashed — which is
    /// where the gesture is actually made.
    pub(super) fn prompt_stash(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.active.is_none() {
            return;
        }
        let draft = cx.new(|cx| StashDraft {
            message: cx.new(|cx| {
                InputState::new(window, cx).placeholder(tr!("stash-message-placeholder"))
            }),
            untracked: true,
            keep_index: false,
        });
        let entity = cx.entity();
        let field = draft.read(cx).message.clone();
        window.open_dialog(cx, move |dialog, _window, _cx| {
            let (entity, draft) = (entity.clone(), draft.clone());
            dialog
                .title(tr!("stash-new-title"))
                .child(draft.clone())
                .overlay_closable(false)
                .close_button(false)
                .footer(super::dialogs::confirm())
                .on_ok(move |_, _window, cx| {
                    // Read on click, where the borrow has been given back: the
                    // closure above runs inside the application's own render.
                    let draft = draft.read(cx);
                    let message = draft.message.read(cx).value().trim().to_string();
                    let (untracked, keep_index) = (draft.untracked, draft.keep_index);
                    entity.update(cx, |this, cx| {
                        this.stash_push(message, untracked, keep_index, cx);
                    });
                    true
                })
        });
        super::dialogs::focus_field(&field, window, cx);
    }

    fn stash_push(
        &mut self,
        message: String,
        untracked: bool,
        keep_index: bool,
        cx: &mut Context<Self>,
    ) {
        let Some(worktree) = self.active.clone() else {
            return;
        };
        self.start(
            Some(worktree.clone()),
            Action::Stash,
            Cmd::StashPush {
                worktree,
                message: (!message.is_empty()).then_some(message),
                untracked,
                keep_index,
            },
            cx,
        );
    }

    /// Restores a stash here, keeping it (`pop: false`) or taking it off the
    /// stack.
    ///
    /// `index` is not offered as a checkbox but as a second menu entry, and only
    /// for `pop`: restoring the staging is what one wants when the stash was
    /// made mid-commit, and that is a gesture, not a setting.
    fn restore_stash(
        &mut self,
        name: String,
        hash: String,
        pop: bool,
        index: bool,
        cx: &mut Context<Self>,
    ) {
        let Some(worktree) = self.active.clone() else {
            return;
        };
        self.start(
            Some(worktree.clone()),
            Action::Stash,
            Cmd::StashRestore {
                worktree,
                name,
                hash,
                pop,
                index,
            },
            cx,
        );
    }

    /// Drops a stash, after asking.
    ///
    /// The one gesture here with no way back: git keeps the commit in the object
    /// database for a while, and nothing on screen can point at it any more.
    fn confirm_drop_stash(&mut self, stash: &Stash, window: &mut Window, cx: &mut Context<Self>) {
        let entity = cx.entity();
        let (name, hash) = (stash.name.clone(), stash.hash.clone());
        let label = SharedString::from(label_of(stash));
        window.open_dialog(cx, move |dialog, _window, _cx| {
            let (entity, name, hash, label) =
                (entity.clone(), name.clone(), hash.clone(), label.clone());
            dialog
                .title(tr!("stash-drop-title"))
                .child(
                    v_flex()
                        .gap_1()
                        .child(div().text_sm().child(label.clone()))
                        .child(div().text_xs().child(tr!("stash-drop-warning"))),
                )
                .overlay_closable(false)
                .close_button(false)
                .footer(super::dialogs::confirm())
                .on_ok(move |_, _window, cx| {
                    let (name, hash) = (name.clone(), hash.clone());
                    entity.update(cx, |this, cx| {
                        let Some(worktree) = this.active.clone() else {
                            return;
                        };
                        this.start(
                            Some(worktree.clone()),
                            Action::Stash,
                            Cmd::StashDrop {
                                worktree,
                                name,
                                hash,
                            },
                            cx,
                        );
                    });
                    true
                })
        });
    }

    /// Empties the stack, after asking. The count is in the question: "delete 7
    /// stashes" is a different decision from "delete this one".
    fn confirm_clear_stashes(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let count = self
            .stashes_of_active()
            .map(|state| state.stashes.len())
            .unwrap_or(0);
        if count == 0 {
            return;
        }
        let entity = cx.entity();
        window.open_dialog(cx, move |dialog, _window, _cx| {
            let entity = entity.clone();
            dialog
                .title(tr!("stash-clear-title", { n: count }))
                .child(div().text_xs().child(tr!("stash-drop-warning")))
                .overlay_closable(false)
                .close_button(false)
                .footer(super::dialogs::confirm())
                .on_ok(move |_, _window, cx| {
                    entity.update(cx, |this, cx| {
                        let Some(worktree) = this.active.clone() else {
                            return;
                        };
                        this.start(
                            Some(worktree.clone()),
                            Action::Stash,
                            Cmd::StashClear { worktree },
                            cx,
                        );
                    });
                    true
                })
        });
    }

    /// Creates a branch at the commit the stash was made on and restores it
    /// there.
    ///
    /// git's own way out of the one case a plain apply cannot serve: the tree
    /// has moved since, and the stash no longer fits. The field opens on the
    /// stash's own words, cut down to something that can be a ref name — a
    /// suggestion, not a decision.
    fn prompt_stash_branch(
        &mut self,
        name: String,
        hash: String,
        subject: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.open_text_dialog_with(
            tr!("stash-branch-title"),
            tr!("stash-branch-placeholder"),
            suggest_branch(&subject),
            window,
            cx,
            move |this, branch, _window, cx| {
                let branch = branch.trim().to_string();
                if branch.is_empty() {
                    return;
                }
                if !crate::git::tags::is_valid_name(&branch) {
                    this.announce_error(tr!("stash-branch-invalid"), cx);
                    return;
                }
                let Some(worktree) = this.active.clone() else {
                    return;
                };
                this.start(
                    Some(worktree.clone()),
                    Action::Stash,
                    Cmd::StashBranch {
                        worktree,
                        name: name.clone(),
                        hash: hash.clone(),
                        branch,
                    },
                    cx,
                );
            },
        );
    }

    /// Shows what a stash holds, in the centre.
    ///
    /// `<hash>^` is the commit the work was taken from — a stash's first parent
    /// — and the diff between the two is the tracked changes it carries. What
    /// this does not show is the untracked files: git files those under a
    /// *third* parent of their own, and a diff that quietly folded them in
    /// would say a stash contains something no `git stash show` reports.
    fn open_stash_diff(&mut self, hash: String, cx: &mut Context<Self>) {
        let Some(worktree) = self.active.clone() else {
            return;
        };
        let range = crate::git::DiffRange::Commit {
            id: hash.clone(),
            parent: Some(format!("{hash}^")),
        };
        let Some(state) = self.review.get_mut(&worktree) else {
            return;
        };
        state.commit = Some(hash.clone());
        state.commit_detail = None;
        state.range = range.clone();
        state.selected = None;
        state.diff = None;
        state.diff_selection = None;
        state
            .files
            .retain(|kept, _| !matches!(kept, crate::git::DiffRange::Commit { .. }));
        state
            .pending_files
            .retain(|kept| !matches!(kept, crate::git::DiffRange::Commit { .. }));
        // The block above the diff: a stash's message says what the work was
        // taken from, which is exactly what one asks a stash.
        self.git
            .send(crate::runtime::Cmd::LoadCommitDetail { worktree, id: hash });
        self.ensure_files(range, cx);
        cx.notify();
    }

    // — Rendering ———————————————————————————————————————————————————

    pub(super) fn render_stashes(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let Some(main) = self.active_main() else {
            return v_flex()
                .size_full()
                .items_center()
                .justify_center()
                .text_color(cx.theme().muted_foreground)
                .child(div().text_sm().child(tr!("no-worktree")))
                .into_any_element();
        };
        self.ensure_stashes(main.clone(), cx);
        let query = self.query(Pane::Stashes, cx);
        let find = self.render_find(Pane::Stashes, cx);
        let bar = self.render_stashes_bar(cx);

        let state = self.stashes.get(&main);
        // Captured and not read back, and what the search keeps is a list of
        // **indices** into it: a frame costs no copy of a stash.
        let stashes = state.map(|state| state.stashes.clone()).unwrap_or_default();
        let rows: Rc<Vec<usize>> = Rc::new(
            stashes
                .iter()
                .enumerate()
                .filter(|(_, stash)| {
                    crate::ui::find::matches(&query, &stash.subject)
                        || crate::ui::find::matches(&query, &stash.branch)
                        || crate::ui::find::matches(&query, &stash.name)
                })
                .map(|(index, _)| index)
                .collect(),
        );
        if rows.is_empty() {
            let pending = state.is_some_and(|state| state.pending);
            return v_flex()
                .size_full()
                .child(bar)
                .children(find)
                .child(empty_stashes(&query, pending, cx))
                .into_any_element();
        }

        let look = Look::of(cx);
        let entity = cx.entity();
        let scroll = self.stashes_scroll.clone();
        let count = rows.len();
        v_flex()
            .size_full()
            .child(bar)
            .children(find)
            .child(
                div().flex_1().min_h_0().child(
                    self.scrolled(
                        "stashes-bar",
                        &scroll,
                        crate::ui::motion::Axes::Vertical,
                        window,
                        uniform_list("stashes-rows", count, move |visible, _window, cx| {
                            visible
                                .map(|index| match rows.get(index) {
                                    Some(at) => {
                                        render_stash(index, &stashes, *at, &look, &entity, cx)
                                    }
                                    None => div().into_any_element(),
                                })
                                .collect::<Vec<_>>()
                        })
                        .size_full()
                        .track_scroll(&scroll.clone()),
                        cx,
                    ),
                ),
            )
            .into_any_element()
    }

    fn render_stashes_bar(&mut self, cx: &mut Context<Self>) -> impl IntoElement {
        let count = self
            .stashes_of_active()
            .map(|state| state.stashes.len())
            .unwrap_or(0);
        let has_active = self.active.is_some();
        h_flex()
            .h(crate::ui::theme::bar_height(cx))
            .w_full()
            .px_2()
            .gap_1()
            .items_center()
            .border_b_1()
            .border_color(cx.theme().border)
            .child(icon("archive").xsmall())
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .text_xs()
                    .text_color(cx.theme().muted_foreground)
                    .child(tr!("stashes-count", { n: count })),
            )
            .child(
                Button::new("stash-new")
                    .ghost()
                    .xsmall()
                    .icon(icon("plus"))
                    .tooltip(tr!("stash-new"))
                    .disabled(!has_active)
                    .on_click(cx.listener(|this, _, window, cx| this.prompt_stash(window, cx))),
            )
            .child(
                Button::new("stash-clear")
                    .ghost()
                    .xsmall()
                    .icon(icon("trash-2"))
                    .tooltip(tr!("stash-clear"))
                    .disabled(count == 0)
                    .on_click(
                        cx.listener(|this, _, window, cx| this.confirm_clear_stashes(window, cx)),
                    ),
            )
            .child(
                Button::new("stash-refresh")
                    .ghost()
                    .xsmall()
                    .icon(icon("refresh-cw"))
                    .tooltip(tr!("action-refresh"))
                    .on_click(cx.listener(|this, _, _window, cx| this.refresh_stashes(cx))),
            )
    }
}

/// What the theme gives a row, read once per frame and not per row.
#[derive(Clone, Copy)]
struct Look {
    /// Two storeys: what the stash says, then where it came from.
    row: gpui::Pixels,
    muted: gpui::Hsla,
    accent: gpui::Hsla,
    info: gpui::Hsla,
}

impl Look {
    fn of(cx: &App) -> Self {
        Self {
            row: crate::ui::theme::row_height(cx) * 2.,
            muted: cx.theme().muted_foreground,
            accent: cx.theme().accent,
            info: cx.theme().info,
        }
    }
}

/// One stash. `index` names the row of the filtered list, `at` the stash in the
/// list itself — the gestures capture the list and that index rather than a
/// copy of the stash.
fn render_stash(
    index: usize,
    stashes: &Rc<Vec<Stash>>,
    at: usize,
    look: &Look,
    entity: &Entity<ClaudhubApp>,
    cx: &App,
) -> gpui::AnyElement {
    let Some(stash) = stashes.get(at) else {
        return div().into_any_element();
    };
    let mono = cx.theme().mono_font_family.clone();
    let (open, menu) = (entity.clone(), entity.clone());
    let (for_click, for_menu) = (stashes.clone(), stashes.clone());

    v_flex()
        .id(("stash-row", index))
        .h(look.row)
        .w_full()
        .pl_1p5()
        .pr(crate::ui::theme::scroll_gutter())
        .py_0p5()
        .justify_center()
        .cursor_pointer()
        .hover(|s| s.bg(look.accent.opacity(0.4)))
        .on_click(move |_, _window, cx| {
            let Some(hash) = for_click.get(at).map(|stash| stash.hash.clone()) else {
                return;
            };
            open.update(cx, |this, cx| this.open_stash_diff(hash, cx));
        })
        .child(
            h_flex()
                .w_full()
                .gap_1()
                .items_center()
                .child(icon("archive").xsmall().text_color(look.info))
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .truncate()
                        .text_sm()
                        // A message git wrote itself is shown muted: it names
                        // the commit the branch was on, which says when the
                        // stash was made and nothing about what is in it.
                        .when(stash.wip, |el| el.text_color(look.muted))
                        .child(SharedString::from(subject_of(stash))),
                )
                .child(
                    div()
                        .flex_none()
                        .text_xs()
                        .text_color(look.muted)
                        .child(SharedString::from(stash.date.clone())),
                ),
        )
        .child(
            h_flex()
                .w_full()
                .gap_1()
                .items_center()
                .text_xs()
                .text_color(look.muted)
                .child(
                    div()
                        .flex_none()
                        .font_family(mono)
                        .child(SharedString::from(stash.short.clone())),
                )
                .when(!stash.branch.is_empty(), |el| {
                    el.child(icon("git-branch").xsmall()).child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .child(SharedString::from(stash.branch.clone())),
                    )
                })
                .child(
                    div()
                        .flex_none()
                        .font_family(cx.theme().mono_font_family.clone())
                        .child(SharedString::from(stash.name.clone())),
                ),
        )
        .context_menu(move |popup, _window, _cx| match for_menu.get(at) {
            Some(stash) => row_menu(popup, &menu, stash),
            None => popup,
        })
        .into_any_element()
}

/// What a row shows on its first storey.
fn subject_of(stash: &Stash) -> String {
    match stash.subject.trim().is_empty() {
        true => stash.name.clone(),
        false => stash.subject.trim().to_string(),
    }
}

/// The same, with where it came from: what a dialog names the stash by.
fn label_of(stash: &Stash) -> String {
    match stash.branch.is_empty() {
        true => format!("{} · {}", stash.name, subject_of(stash)),
        false => format!("{} · {} · {}", stash.name, stash.branch, subject_of(stash)),
    }
}

/// A branch name suggested from what the stash says.
///
/// Cut down to what `check-ref-format` takes, and to something one can read in
/// a branch list: a message is a sentence, a branch name is not. It is a
/// suggestion — the field stays editable, and an empty one is left to the user
/// to fill in rather than replaced by a name we invented.
fn suggest_branch(subject: &str) -> String {
    let mut out = String::new();
    for c in subject.trim().to_lowercase().chars() {
        if c.is_ascii_alphanumeric() {
            out.push(c);
        } else if !out.ends_with('-') {
            out.push('-');
        }
        if out.len() >= 40 {
            break;
        }
    }
    let name = out.trim_matches('-').to_string();
    // A message of nothing but punctuation, or a `WIP on …` whose sha leads:
    // both give something git would refuse, and an empty field says "type a
    // name" where a refused one says nothing at all.
    match crate::git::tags::is_valid_name(&name) {
        true => name,
        false => String::new(),
    }
}

fn row_menu(popup: PopupMenu, entity: &Entity<ClaudhubApp>, stash: &Stash) -> PopupMenu {
    let (name, hash) = (stash.name.clone(), stash.hash.clone());
    // Apply first and pop second, in that order: the one that keeps the stash
    // is the one that forgives being wrong about what it holds.
    let popup = popup.item({
        let (entity, name, hash) = (entity.clone(), name.clone(), hash.clone());
        PopupMenuItem::new(tr!("stash-apply"))
            .icon(icon("inbox"))
            .on_click(move |_, _window, cx| {
                let (name, hash) = (name.clone(), hash.clone());
                entity.update(cx, |this, cx| {
                    this.restore_stash(name, hash, false, false, cx)
                });
            })
    });
    let popup = popup.item({
        let (entity, name, hash) = (entity.clone(), name.clone(), hash.clone());
        PopupMenuItem::new(tr!("stash-pop"))
            .icon(icon("arrow-down-to-line"))
            .on_click(move |_, _window, cx| {
                let (name, hash) = (name.clone(), hash.clone());
                entity.update(cx, |this, cx| {
                    this.restore_stash(name, hash, true, false, cx)
                });
            })
    });
    let popup = popup.item({
        let (entity, name, hash) = (entity.clone(), name.clone(), hash.clone());
        PopupMenuItem::new(tr!("stash-pop-index"))
            .icon(icon("arrow-down-to-line"))
            .on_click(move |_, _window, cx| {
                let (name, hash) = (name.clone(), hash.clone());
                entity.update(cx, |this, cx| {
                    this.restore_stash(name, hash, true, true, cx)
                });
            })
    });
    let popup = popup.item({
        let (entity, name, hash, subject) = (
            entity.clone(),
            name.clone(),
            hash.clone(),
            subject_of(stash),
        );
        PopupMenuItem::new(tr!("stash-branch"))
            .icon(icon("git-branch"))
            .on_click(move |_, window, cx| {
                let (name, hash, subject) = (name.clone(), hash.clone(), subject.clone());
                entity.update(cx, |this, cx| {
                    this.prompt_stash_branch(name, hash, subject, window, cx)
                });
            })
    });
    let popup = popup.item({
        let (entity, hash) = (entity.clone(), hash.clone());
        PopupMenuItem::new(tr!("stash-show-diff"))
            .icon(icon("file-diff"))
            .on_click(move |_, _window, cx| {
                let hash = hash.clone();
                entity.update(cx, |this, cx| this.open_stash_diff(hash, cx));
            })
    });
    let popup = popup.item({
        let name = name.clone();
        PopupMenuItem::new(tr!("stash-copy-name"))
            .icon(icon("copy"))
            .on_click(move |_, _window, cx| {
                cx.write_to_clipboard(gpui::ClipboardItem::new_string(name.clone()));
            })
    });
    popup.item({
        let (entity, stash) = (entity.clone(), stash.clone());
        PopupMenuItem::new(tr!("stash-drop"))
            .icon(icon("trash-2"))
            .on_click(move |_, window, cx| {
                let stash = stash.clone();
                entity.update(cx, |this, cx| this.confirm_drop_stash(&stash, window, cx));
            })
    })
}

/// Nothing to show: a read under way, a search that found nothing, or a
/// repository with nothing put aside — three different things, and saying the
/// wrong one is how a panel reads as broken.
fn empty_stashes(query: &str, pending: bool, cx: &App) -> gpui::AnyElement {
    let message = if pending {
        tr!("stashes-loading")
    } else if query.trim().is_empty() {
        tr!("stashes-empty")
    } else {
        tr!("find-no-match")
    };
    v_flex()
        .size_full()
        .items_center()
        .justify_center()
        .gap_2()
        .text_color(cx.theme().muted_foreground)
        .child(icon("archive"))
        .child(div().text_sm().px_4().child(message))
        .into_any_element()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn stash(subject: &str, branch: &str, wip: bool) -> Stash {
        Stash {
            name: "stash@{0}".into(),
            index: 0,
            hash: "a".repeat(40),
            short: "aaaaaaa".into(),
            branch: branch.into(),
            subject: subject.into(),
            wip,
            date: "now".into(),
        }
    }

    /// A message is a sentence and a branch name is not: what is suggested has
    /// to be something git will take.
    #[test]
    fn a_branch_is_suggested_from_what_the_stash_says() {
        assert_eq!(
            suggest_branch("Put the migration aside"),
            "put-the-migration-aside"
        );
        assert_eq!(
            suggest_branch("refs: split the loader!"),
            "refs-split-the-loader"
        );
    }

    /// A message with nothing usable in it leaves the field empty: a name we
    /// invented would be one nobody meant, and a refused one says nothing at
    /// all.
    #[test]
    fn a_message_with_no_name_in_it_suggests_nothing() {
        assert!(suggest_branch("!!! ???").is_empty());
        assert!(suggest_branch("").is_empty());
    }

    /// A stash git named itself still has to be nameable in a dialog.
    #[test]
    fn a_row_always_has_something_to_show() {
        assert_eq!(subject_of(&stash("", "", true)), "stash@{0}");
        assert!(label_of(&stash("WIP", "main", true)).contains("main"));
    }
}
