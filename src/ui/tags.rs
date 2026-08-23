//! The "Tags" panel: what the repository has marked, and what one does with it.
//!
//! It is a tab beside "History", in the review screen's left column, and that
//! is where it belongs: a tag names a commit, the history shows the commits,
//! and the gesture one makes after finding the commit worth marking is right
//! there. Clicking a tag opens its commit's diff, exactly as clicking a row of
//! the history does — two lists onto the same thing, one ordered by date of
//! commit, the other by date of tag.
//!
//! **Local and remote are two different pieces of knowledge**, and the panel
//! keeps them apart because `git::tags` does. The list is a read of `refs/tags`
//! and costs milliseconds; whether `origin` has a tag is a `ls-remote` and
//! costs a round trip. So nothing claims anything about the remote until the
//! globe button has been pressed — and once it has, a tag `origin` does not
//! carry says so, which is the one thing one wants to know before a release.
//!
//! Four gestures, and each is a command of its own: create (annotated when a
//! message is given, which is git's own distinction), push one or push them
//! all, delete locally, delete on the remote. The last two are two menu entries
//! and not a flag on one, because they are two different regrets.

use std::collections::HashSet;
use std::path::PathBuf;
use std::rc::Rc;

use gpui::{div, prelude::*, px, uniform_list, App, Context, Entity, SharedString, Window};
use gpui_component::{
    button::{Button, ButtonVariants},
    checkbox::Checkbox,
    h_flex,
    input::{Input, InputState},
    menu::{ContextMenuExt, PopupMenu, PopupMenuItem},
    v_flex, ActiveTheme, Disableable, Selectable, Sizable, WindowExt,
};

use crate::git::Tag;
use crate::runtime::Cmd;
use crate::tr;
use crate::ui::app::ClaudhubApp;
use crate::ui::find::Pane;
use crate::ui::icons::icon;

/// What is known of a repository's tags.
#[derive(Default)]
pub struct TagsState {
    pub tags: Vec<Tag>,
    /// The names `origin` carries. **`None` until it has been asked for**: a
    /// panel that showed "only local" without having read the remote would be
    /// saying something it does not know.
    pub remote: Option<HashSet<String>>,
    /// A read has gone out and has not come back. Without this guard, every
    /// frame would restart the command for the whole length of the read — it is
    /// the panel that asks, and it asks at render time.
    pub pending: bool,
    pub remote_pending: bool,
    /// The list has come back at least once.
    ///
    /// Without it, a repository with no tag would ask again on every frame: an
    /// empty list and a list never read are the same thing to look at and two
    /// different things to act on — the four-state `Load` of the database tree,
    /// cut down to what this panel needs.
    pub loaded: bool,
}

/// The tag being created, while the dialog is open.
///
/// **An entity of its own and not a field of `ClaudhubApp`**: the closure
/// `open_dialog` keeps is called back on every frame, from the root view's
/// render, that is in the middle of a borrow of the application — touching it
/// there panics. It is `server::WslPrompt`'s pattern.
pub struct TagDraft {
    pub name: Entity<InputState>,
    pub message: Entity<InputState>,
    /// Push it to `origin` right after creating it. A tag one makes for a
    /// release is a tag one pushes, and having to find the menu entry
    /// afterwards is the round trip this panel removes.
    pub push: bool,
    /// The commit to mark, `None` for HEAD. It is the one selected in the
    /// history when there is one: "tag *this* commit" is the gesture one has
    /// while reading it.
    pub at: Option<String>,
}

impl Render for TagDraft {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let name = self.name.read(cx).value().to_string();
        let valid = crate::git::tags::is_valid_name(&name);
        let empty = name.trim().is_empty();
        let push = self.push;
        v_flex()
            .w(px(420.))
            .gap_2()
            .child(Input::new(&self.name))
            // Said under the field being typed, not after the dialog has
            // closed: git's own refusal (`fatal: 'v 1.0' is not a valid tag
            // name`) arrives too late to correct anything.
            .when(!empty && !valid, |el| {
                el.child(
                    div()
                        .text_xs()
                        .text_color(cx.theme().danger)
                        .child(tr!("tag-name-invalid")),
                )
            })
            .child(Input::new(&self.message))
            .child(
                div()
                    .text_xs()
                    .text_color(cx.theme().muted_foreground)
                    .child(tr!("tag-message-help")),
            )
            .child(
                Checkbox::new("tag-push")
                    .label(tr!("tag-push-after"))
                    .checked(push)
                    .on_click(cx.listener(|this, _, _window, cx| {
                        this.push = !this.push;
                        cx.notify();
                    })),
            )
            .when_some(self.at.clone(), |el, at| {
                el.child(
                    div()
                        .text_xs()
                        .text_color(cx.theme().muted_foreground)
                        .child(tr!("tag-at-commit", { commit: short(&at) })),
                )
            })
    }
}

fn short(commit: &str) -> String {
    commit.chars().take(8).collect()
}

impl ClaudhubApp {
    /// The tags of the repository the selected worktree belongs to.
    pub(super) fn tags_of_active(&self) -> Option<&TagsState> {
        self.tags.get(&self.active_main()?)
    }

    /// The main repository the panel is about.
    fn active_main(&self) -> Option<PathBuf> {
        self.main_of(self.active.as_deref()?)
    }

    /// Asks for the tag list, once.
    ///
    /// At render time, like the history and with the same guard: opening a
    /// worktree must not pay for a read nobody will look at, and a panel that
    /// asks on every frame asks a hundred times a second.
    fn ensure_tags(&mut self, main: PathBuf, cx: &mut Context<Self>) {
        let state = self.tags.entry(main.clone()).or_default();
        if state.pending || state.loaded {
            return;
        }
        state.pending = true;
        self.git.send(Cmd::LoadTags { main });
        cx.notify();
    }

    pub(super) fn tags_arrived(&mut self, main: PathBuf, tags: Vec<Tag>, cx: &mut Context<Self>) {
        let state = self.tags.entry(main).or_default();
        state.tags = tags;
        state.pending = false;
        state.loaded = true;
        cx.notify();
    }

    pub(super) fn remote_tags_arrived(
        &mut self,
        main: PathBuf,
        names: Vec<String>,
        cx: &mut Context<Self>,
    ) {
        let state = self.tags.entry(main).or_default();
        state.remote = Some(names.into_iter().collect());
        state.remote_pending = false;
        cx.notify();
    }

    /// Re-reads the list, and forgets what was known of the remote.
    ///
    /// Forgetting is the point: the remote list was true at the moment it was
    /// read, and a refresh is precisely the moment one stops vouching for it.
    pub(super) fn refresh_tags(&mut self, cx: &mut Context<Self>) {
        let Some(main) = self.active_main() else {
            return;
        };
        let state = self.tags.entry(main.clone()).or_default();
        state.tags.clear();
        state.remote = None;
        state.loaded = false;
        state.pending = true;
        self.git.send(Cmd::LoadTags { main });
        cx.notify();
    }

    /// Asks `origin` which tags it has.
    fn load_remote_tags(&mut self, cx: &mut Context<Self>) {
        let (Some(main), Some(worktree)) = (self.active_main(), self.active.clone()) else {
            return;
        };
        let state = self.tags.entry(main).or_default();
        if state.remote_pending {
            return;
        }
        state.remote_pending = true;
        self.git.send(Cmd::LoadRemoteTags { worktree });
        cx.notify();
    }

    /// Opens the dialog that creates a tag.
    ///
    /// It marks the commit selected in the history when there is one, HEAD
    /// otherwise: "tag this commit" is the gesture one has while reading it,
    /// and "tag where I am" the one from anywhere else.
    pub(super) fn prompt_new_tag(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.active.is_none() {
            return;
        }
        let at = self.active_review().and_then(|state| state.commit.clone());
        let draft = cx.new(|cx| TagDraft {
            name: cx.new(|cx| InputState::new(window, cx).placeholder(tr!("tag-name-placeholder"))),
            message: cx
                .new(|cx| InputState::new(window, cx).placeholder(tr!("tag-message-placeholder"))),
            push: false,
            at,
        });
        let entity = cx.entity();
        // The name, and not the message: it is the field the dialog exists for,
        // and the one Enter can be pressed straight after filling in.
        let name = draft.read(cx).name.clone();
        window.open_dialog(cx, move |dialog, _window, _cx| {
            let (entity, draft) = (entity.clone(), draft.clone());
            dialog
                .title(tr!("tag-new-title"))
                .child(draft.clone())
                .overlay_closable(false)
                .close_button(false)
                .footer(super::dialogs::confirm())
                .on_ok(move |_, _window, cx| {
                    // Read on click, where the borrow has been given back: the
                    // closure above runs inside the application's own render.
                    let draft = draft.read(cx);
                    let name = draft.name.read(cx).value().trim().to_string();
                    let message = draft.message.read(cx).value().trim().to_string();
                    let (push, at) = (draft.push, draft.at.clone());
                    entity.update(cx, |this, cx| {
                        this.create_tag(name, message, at, push, cx);
                    });
                    true
                })
        });
        super::dialogs::focus_field(&name, window, cx);
    }

    fn create_tag(
        &mut self,
        name: String,
        message: String,
        at: Option<String>,
        push: bool,
        cx: &mut Context<Self>,
    ) {
        let Some(worktree) = self.active.clone() else {
            return;
        };
        if !crate::git::tags::is_valid_name(&name) {
            self.toast = Some(crate::ui::app::Toast {
                text: tr!("tag-name-invalid"),
                error: true,
            });
            cx.notify();
            return;
        }
        // One command, creation and push together: two would go into two queues
        // — the local one and the network one — and nothing orders those, so
        // the push could leave before the tag existed. See `Cmd::CreateTag`.
        self.start(
            Some(worktree.clone()),
            if push {
                crate::runtime::Action::PushTag
            } else {
                crate::runtime::Action::Tag
            },
            Cmd::CreateTag {
                worktree,
                name,
                message: (!message.is_empty()).then_some(message),
                at,
                push,
            },
            cx,
        );
    }

    /// Pushes one tag, or every tag `origin` lacks.
    pub(super) fn push_tag(&mut self, name: Option<String>, cx: &mut Context<Self>) {
        let Some(worktree) = self.active.clone() else {
            return;
        };
        self.start(
            Some(worktree.clone()),
            crate::runtime::Action::PushTag,
            Cmd::PushTag { worktree, name },
            cx,
        );
    }

    /// Deletes a tag, after asking.
    ///
    /// `remote` deletes it on `origin` too, and the dialog says so plainly: a
    /// tag other people have already pulled does not come back.
    pub(super) fn confirm_delete_tag(
        &mut self,
        name: String,
        remote: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let entity = cx.entity();
        let label = SharedString::from(name.clone());
        window.open_dialog(cx, move |dialog, _window, _cx| {
            let (entity, name, label) = (entity.clone(), name.clone(), label.clone());
            dialog
                .title(if remote {
                    tr!("tag-delete-remote-title")
                } else {
                    tr!("tag-delete-title")
                })
                .child(
                    v_flex()
                        .gap_1()
                        .child(div().text_sm().child(label.clone()))
                        .child(div().text_xs().child(if remote {
                            tr!("tag-delete-remote-warning")
                        } else {
                            tr!("tag-delete-warning")
                        })),
                )
                .overlay_closable(false)
                .close_button(false)
                .footer(super::dialogs::confirm())
                .on_ok(move |_, _window, cx| {
                    entity.update(cx, |this, cx| this.delete_tag(name.clone(), remote, cx));
                    true
                })
        });
    }

    fn delete_tag(&mut self, name: String, remote: bool, cx: &mut Context<Self>) {
        let Some(worktree) = self.active.clone() else {
            return;
        };
        if remote {
            self.start(
                Some(worktree.clone()),
                crate::runtime::Action::PushTag,
                Cmd::DeleteRemoteTag {
                    worktree: worktree.clone(),
                    name: name.clone(),
                },
                cx,
            );
        }
        self.start(
            Some(worktree.clone()),
            crate::runtime::Action::Tag,
            Cmd::DeleteTag { worktree, name },
            cx,
        );
    }

    /// Shows the diff of the commit a tag marks.
    ///
    /// The parent is written `<commit>^` rather than looked up: the history may
    /// not have been loaded, and a tag's commit is almost never the one the
    /// graph has under the cursor. The one commit this gets wrong is a root
    /// commit, which has no parent and no tag either, nine times out of ten.
    fn open_tag_commit(&mut self, id: String, cx: &mut Context<Self>) {
        let Some(worktree) = self.active.clone() else {
            return;
        };
        let range = crate::git::DiffRange::Commit {
            id: id.clone(),
            parent: Some(format!("{id}^")),
        };
        let Some(state) = self.review.get_mut(&worktree) else {
            return;
        };
        state.commit = Some(id);
        state.range = range.clone();
        state.selected = None;
        state.diff = None;
        state.diff_selection = None;
        // Another commit's diffs are of no further use, exactly as when opening
        // one from the history.
        state
            .files
            .retain(|kept, _| !matches!(kept, crate::git::DiffRange::Commit { .. }));
        state
            .pending_files
            .retain(|kept| !matches!(kept, crate::git::DiffRange::Commit { .. }));
        self.ensure_files(range, cx);
        cx.notify();
    }

    // — Rendering ———————————————————————————————————————————————————

    pub(super) fn render_tags(
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
        self.ensure_tags(main.clone(), cx);
        let query = self.query(Pane::Tags, cx);
        let find = self.render_find(Pane::Tags, cx);
        let bar = self.render_tags_bar(cx);

        let state = self.tags.get(&main);
        let remote = state.and_then(|state| state.remote.clone());
        // Cloned rather than borrowed: the row closure runs for every visible
        // row on every frame, with the application already borrowed — reading
        // the entity from inside it is the panic gpui refuses.
        let rows: Rc<Vec<Tag>> = Rc::new(
            state
                .map(|state| {
                    state
                        .tags
                        .iter()
                        .filter(|tag| {
                            crate::ui::find::matches(&query, &tag.name)
                                || crate::ui::find::matches(&query, &tag.subject)
                        })
                        .cloned()
                        .collect()
                })
                .unwrap_or_default(),
        );
        if rows.is_empty() {
            let pending = state.is_some_and(|state| state.pending);
            return v_flex()
                .size_full()
                .child(bar)
                .children(find)
                .child(empty_tags(&query, pending, cx))
                .into_any_element();
        }

        let look = Look::of(cx);
        let entity = cx.entity();
        let scroll = self.tags_scroll.clone();
        let count = rows.len();
        v_flex()
            .size_full()
            .child(bar)
            .children(find)
            .child(
                div().flex_1().min_h_0().child(
                    self.scrolled(
                        "tags-bar",
                        &scroll,
                        crate::ui::motion::Axes::Vertical,
                        window,
                        uniform_list("tags-rows", count, move |visible, _window, cx| {
                            visible
                                .map(|index| match rows.get(index) {
                                    Some(tag) => {
                                        render_tag(index, tag, remote.as_ref(), &look, &entity, cx)
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

    fn render_tags_bar(&mut self, cx: &mut Context<Self>) -> impl IntoElement {
        let state = self.tags_of_active();
        let count = state.map(|state| state.tags.len()).unwrap_or(0);
        let known = state.and_then(|state| state.remote.as_ref());
        let unpushed = known
            .map(|remote| {
                state
                    .map(|state| {
                        state
                            .tags
                            .iter()
                            .filter(|tag| !remote.contains(&tag.name))
                            .count()
                    })
                    .unwrap_or(0)
            })
            .unwrap_or(0);
        let checking = state.is_some_and(|state| state.remote_pending);
        h_flex()
            .h(crate::ui::theme::bar_height(cx))
            .w_full()
            .px_2()
            .gap_1()
            .items_center()
            .border_b_1()
            .border_color(cx.theme().border)
            .child(icon("tag").xsmall())
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .text_xs()
                    .text_color(cx.theme().muted_foreground)
                    // What the remote is worth is said, never assumed: as long
                    // as nobody has asked, the count of what is missing there is
                    // not a thing we know.
                    .child(if known.is_some() {
                        tr!("tags-count-unpushed", { n: count, unpushed: unpushed })
                    } else {
                        tr!("tags-count", { n: count })
                    }),
            )
            .child(
                Button::new("tag-new")
                    .ghost()
                    .xsmall()
                    .icon(icon("plus"))
                    .tooltip(tr!("tag-new"))
                    .on_click(cx.listener(|this, _, window, cx| this.prompt_new_tag(window, cx))),
            )
            .child(
                Button::new("tag-push-all")
                    .ghost()
                    .xsmall()
                    .icon(icon("arrow-up-from-line"))
                    .tooltip(tr!("tag-push-all"))
                    .disabled(count == 0)
                    .on_click(cx.listener(|this, _, _window, cx| this.push_tag(None, cx))),
            )
            .child(
                Button::new("tag-remote")
                    .ghost()
                    .xsmall()
                    .icon(icon("globe"))
                    .tooltip(tr!("tag-check-remote"))
                    .disabled(checking)
                    .selected(known.is_some())
                    .on_click(cx.listener(|this, _, _window, cx| this.load_remote_tags(cx))),
            )
            .child(
                Button::new("tag-refresh")
                    .ghost()
                    .xsmall()
                    .icon(icon("refresh-cw"))
                    .tooltip(tr!("action-refresh"))
                    .on_click(cx.listener(|this, _, _window, cx| this.refresh_tags(cx))),
            )
    }
}

/// What the theme gives a row, read once per frame and not per row.
#[derive(Clone, Copy)]
struct Look {
    row: gpui::Pixels,
    muted: gpui::Hsla,
    accent: gpui::Hsla,
    warning: gpui::Hsla,
    info: gpui::Hsla,
}

impl Look {
    fn of(cx: &App) -> Self {
        Self {
            // Two storeys: the tag, then the commit it marks.
            row: crate::ui::theme::row_height(cx) * 2.,
            muted: cx.theme().muted_foreground,
            accent: cx.theme().accent,
            warning: cx.theme().warning,
            info: cx.theme().info,
        }
    }
}

/// One tag.
fn render_tag(
    index: usize,
    tag: &Tag,
    remote: Option<&HashSet<String>>,
    look: &Look,
    entity: &Entity<ClaudhubApp>,
    cx: &App,
) -> gpui::AnyElement {
    let mono = cx.theme().mono_font_family.clone();
    // Only said when the remote list has been read: "only local" about a remote
    // nobody asked would be a claim, not a fact.
    let only_local = remote.map(|remote| !remote.contains(&tag.name));
    let (open, menu) = (entity.clone(), entity.clone());
    let (target, for_menu) = (tag.target.clone(), tag.clone());

    v_flex()
        .id(("tag-row", index))
        .h(look.row)
        .w_full()
        .pl_1p5()
        .pr(crate::ui::theme::scroll_gutter())
        .py_0p5()
        .justify_center()
        .cursor_pointer()
        .hover(|s| s.bg(look.accent.opacity(0.4)))
        .on_click(move |_, _window, cx| {
            let target = target.clone();
            open.update(cx, |this, cx| this.open_tag_commit(target, cx));
        })
        .child(
            h_flex()
                .w_full()
                .gap_1()
                .items_center()
                .child(
                    icon(if tag.annotated { "tag" } else { "tags" })
                        .xsmall()
                        .text_color(if tag.annotated { look.info } else { look.muted }),
                )
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .truncate()
                        .text_sm()
                        .child(SharedString::from(tag.name.clone())),
                )
                .when(only_local == Some(true), |el| {
                    el.child(
                        div()
                            .flex_none()
                            .text_xs()
                            .text_color(look.warning)
                            .child(tr!("tag-only-local")),
                    )
                })
                .child(
                    div()
                        .flex_none()
                        .text_xs()
                        .text_color(look.muted)
                        .child(SharedString::from(tag.date.clone())),
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
                        .child(SharedString::from(tag.target.clone())),
                )
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .truncate()
                        .child(SharedString::from(summary(tag))),
                ),
        )
        .context_menu(move |popup, _window, _cx| row_menu(popup, &menu, &for_menu))
        .into_any_element()
}

/// The second storey: what the tag says, and who tagged.
fn summary(tag: &Tag) -> String {
    let mut parts = Vec::new();
    if !tag.subject.trim().is_empty() {
        parts.push(tag.subject.trim().to_string());
    }
    if !tag.author.trim().is_empty() {
        parts.push(tag.author.trim().to_string());
    }
    parts.join(" · ")
}

fn row_menu(popup: PopupMenu, entity: &Entity<ClaudhubApp>, tag: &Tag) -> PopupMenu {
    let popup = popup.item({
        let (entity, name) = (entity.clone(), tag.name.clone());
        PopupMenuItem::new(tr!("tag-push"))
            .icon(icon("arrow-up-from-line"))
            .on_click(move |_, _window, cx| {
                let name = name.clone();
                entity.update(cx, |this, cx| this.push_tag(Some(name), cx));
            })
    });
    let popup = popup.item({
        let (entity, target) = (entity.clone(), tag.target.clone());
        PopupMenuItem::new(tr!("tag-show-commit"))
            .icon(icon("git-commit-horizontal"))
            .on_click(move |_, _window, cx| {
                let target = target.clone();
                entity.update(cx, |this, cx| this.open_tag_commit(target, cx));
            })
    });
    let popup = popup.item({
        let name = tag.name.clone();
        PopupMenuItem::new(tr!("tag-copy-name"))
            .icon(icon("copy"))
            .on_click(move |_, _window, cx| {
                cx.write_to_clipboard(gpui::ClipboardItem::new_string(name.clone()));
            })
    });
    let popup = popup.item({
        let (entity, name) = (entity.clone(), tag.name.clone());
        PopupMenuItem::new(tr!("tag-delete"))
            .icon(icon("trash-2"))
            .on_click(move |_, window, cx| {
                let name = name.clone();
                entity.update(cx, |this, cx| {
                    this.confirm_delete_tag(name, false, window, cx)
                });
            })
    });
    popup.item({
        let (entity, name) = (entity.clone(), tag.name.clone());
        // The same icon as the local deletion: two entries doing the same thing
        // to two objects belong to the same family of gestures, and it is the
        // label that tells them apart.
        PopupMenuItem::new(tr!("tag-delete-remote"))
            .icon(icon("trash-2"))
            .on_click(move |_, window, cx| {
                let name = name.clone();
                entity.update(cx, |this, cx| {
                    this.confirm_delete_tag(name, true, window, cx)
                });
            })
    })
}

/// Nothing to show: a read under way, a search that found nothing, or a
/// repository with no tag at all — three different things, and saying the wrong
/// one is how a panel reads as broken.
fn empty_tags(query: &str, pending: bool, cx: &App) -> gpui::AnyElement {
    let message = if pending {
        tr!("tags-loading")
    } else if query.trim().is_empty() {
        tr!("tags-empty")
    } else {
        tr!("find-no-match")
    };
    v_flex()
        .size_full()
        .items_center()
        .justify_center()
        .gap_2()
        .text_color(cx.theme().muted_foreground)
        .child(icon("tag"))
        .child(div().text_sm().px_4().child(message))
        .into_any_element()
}
