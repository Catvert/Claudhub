//! The top bar: the window's title bar, and what drives the views.
//!
//! It carries the application menu and the two pickers that decide what every
//! other panel is talking about — the worktree, then its branch. Zed's title
//! bar does the same, and for the same reason: these two are not information
//! about the place one is in, they are the gestures that *change* it, and the
//! toolbar's rule is that it carries actions.
//!
//! The branch came back up from the status bar with that promotion. It had gone
//! down there as a word one reads; it comes back as a button one clicks, and
//! saying it in both places at once would be one place too many.

use gpui::{div, prelude::*, px, Context, Entity, Window};
use gpui_component::{
    button::{Button, ButtonGroup, ButtonVariants},
    h_flex,
    menu::{DropdownMenu, PopupMenuItem},
    v_flex, ActiveTheme, Disableable, Sizable, StyledExt, TitleBar,
};

use crate::runtime::Cmd;
use crate::tr;
use crate::ui::app::ClaudhubApp;
use crate::ui::icons::icon;

/// One row of the views menu: the tick, the name, and the gesture that toggles it.
///
/// A `PopupMenuItem::element` and not an ordinary entry, for two reasons both of
/// which come down to the same thing — **several of them get toggled in a row**:
///
/// - `PopupMenu::confirm` **closes the menu** after calling an entry's handler,
///   with no way to prevent it. The row therefore consumes the click itself
///   (`stop_propagation`): the entry carrying it never sees it, and nothing
///   closes.
/// - A `checked` is frozen at the menu's construction, which happens only once.
///   The tick is therefore painted by the row, which re-reads the state on every
///   frame.
fn view_toggle(app: Entity<ClaudhubApp>, name: &'static str, title: &'static str) -> PopupMenuItem {
    toggle_row(app, name, move || tr!(title))
}

/// The same, for a plugin: its title comes from its manifest and not from a
/// catalogue — `tr!` reads catalogues compiled into the binary, and a plugin's
/// strings are its own.
fn plugin_toggle(app: Entity<ClaudhubApp>, name: &'static str) -> PopupMenuItem {
    toggle_row(app, name, move || {
        gpui::SharedString::from(
            crate::ui::plugin_view::by_panel(name)
                .map(|(_, panel)| panel.title.clone())
                .unwrap_or_else(|| name.to_string()),
        )
    })
}

fn toggle_row(
    app: Entity<ClaudhubApp>,
    name: &'static str,
    label: impl Fn() -> gpui::SharedString + 'static,
) -> PopupMenuItem {
    PopupMenuItem::element(move |_window, cx| {
        let visible = app.read(cx).panel_visible(name);
        let app = app.clone();
        h_flex()
            .id(name)
            .w_full()
            .gap_2()
            .items_center()
            // The tick's column is reserved permanently: without it, the names
            // would jump one notch on every toggle.
            .child(
                div()
                    .w(px(14.))
                    .when(visible, |this| this.child(icon("check").xsmall())),
            )
            .child(label())
            .on_click(move |_, _window, cx| {
                cx.stop_propagation();
                app.update(cx, |this, cx| this.toggle_panel(name, cx));
            })
    })
}

/// The volume of work in progress: lines added and removed.
///
/// The file count is only there for want of better — a rename or a binary moves
/// no line, and showing nothing would suggest there is nothing.
pub(super) fn volume(summary: crate::git::Summary, cx: &gpui::App) -> impl IntoElement {
    let colors = crate::ui::theme::DiffColors::of(cx);
    h_flex()
        .flex_none()
        .gap_1()
        .text_xs()
        .children(crate::ui::theme::volume(
            summary.added,
            summary.removed,
            &colors,
        ))
        .when(summary.added == 0 && summary.removed == 0, |el| {
            el.child(
                div()
                    .text_color(cx.theme().muted_foreground)
                    .child(summary.files.to_string()),
            )
        })
}

/// An agent's badge: filled when it works, hollow when it waits.
///
/// A badge and not a word: the row already carries a name and a branch, and this
/// is information read out of the corner of the eye while scanning the list.
pub(super) fn agent_badge(agent: &crate::agent::State, cx: &gpui::App) -> impl IntoElement {
    let color = if agent.working {
        cx.theme().warning
    } else {
        cx.theme().muted_foreground
    };
    h_flex()
        .flex_none()
        .gap_1()
        .items_center()
        .child(
            div()
                .size(px(7.))
                .rounded_full()
                .when(agent.working, |el| el.bg(color))
                .when(!agent.working, |el| {
                    el.border_1().border_color(color.opacity(0.8))
                }),
        )
        // Two agents in the same worktree does happen: we say so rather than
        // let it look as if there were only one.
        .when(agent.count > 1, |el| {
            el.child(
                div()
                    .text_xs()
                    .text_color(color)
                    .child(agent.count.to_string()),
            )
        })
        // The agent's name as soon as there is more than one profile to tell
        // apart: the badge says something is going on, it does not say who.
        .child(
            div()
                .text_xs()
                .text_color(color)
                .child(agent.programs.join(", ")),
        )
}

/// The name of a repository we could not open.
///
/// Derived from the path and not asked of git, which is precisely what cannot
/// answer: it is the last segment, the one that is recognised, and the whole
/// path stays on the line below.
fn repo_name(path: &std::path::Path) -> String {
    path.file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.display().to_string())
}

/// What the worktree picker shows of one worktree.
///
/// A snapshot taken when the menu is built, and not read frame by frame: the
/// menu is rebuilt at every opening, and a list whose rows change height under
/// the pointer is a list one misses.
struct WorktreeItem {
    path: std::path::PathBuf,
    label: String,
    branch: Option<String>,
    is_main: bool,
    summary: Option<crate::git::Summary>,
    agent: Option<crate::agent::State>,
}

impl ClaudhubApp {
    /// The two arrows of the trail — `ui::jumps` — and the fourth and fifth
    /// mouse buttons made visible.
    ///
    /// Always both, greyed when there is nowhere to go: an arrow that appears
    /// and disappears moves everything beside it every time one follows a
    /// link, and the pickers are what sits beside it here.
    fn render_trail_buttons(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let (back, forward) = self.can_travel();
        h_flex()
            .flex_shrink_0()
            .child(
                Button::new("trail-back")
                    .ghost()
                    .xsmall()
                    .icon(icon("arrow-left"))
                    .disabled(!back)
                    .tooltip(tr!("editor-jump-back"))
                    .on_click(cx.listener(|this, _, window, cx| this.jump_back(window, cx))),
            )
            .child(
                Button::new("trail-forward")
                    .ghost()
                    .xsmall()
                    .icon(icon("arrow-right"))
                    .disabled(!forward)
                    .tooltip(tr!("editor-jump-forward"))
                    .on_click(cx.listener(|this, _, window, cx| this.jump_forward(window, cx))),
            )
    }

    pub(super) fn render_topbar(
        &mut self,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let active = self.active.clone();
        // One list, read twice — by what the group shows and by what a click
        // means. Two lists would drift at the first addition, and the drift is
        // silent: the button would open the screen next door.
        let aside = crate::ui::workspace::Workspace::ASIDE;
        // The top bar **is** the window's title bar.
        //
        // `TitleBar::title_bar_options()` asks the platform not to draw one: on
        // Windows the window therefore had nothing left to be moved, minimised
        // or closed by. One is needed, and stacking one above this would cost
        // thirty pixels to repeat what it already says — that is the reasoning
        // that moved the screen picker down into the status bar. `TitleBar`
        // brings the drag, the double click that maximises and the window
        // buttons; our actions live inside it. It keeps our height and our
        // colours, not its own.
        //
        // The buttons placed in the drag region stay clickable: the region is
        // returned as `HTCAPTION`, but gpui handles non-client mouse messages
        // and redistributes them. It is what Zed's title bar does, on the same
        // terms.
        TitleBar::new()
            .h(super::theme::toolbar_height(cx))
            .border_color(cx.theme().border)
            .bg(cx.theme().title_bar)
            .child(
                h_flex()
                    .w_full()
                    .h_full()
                    .pr_2()
                    .gap_1()
                    .items_center()
                    .child(self.render_main_menu(cx))
                    // The trail, before the pickers: it is the one thing here
                    // that speaks of *where one has been* rather than of what
                    // one is looking at, and it belongs to the title bar
                    // because it is the only chrome that crosses the screens
                    // the trail crosses. The editor's bar keeps its own two,
                    // on the same trail.
                    .child(self.render_trail_buttons(cx))
                    // The two pickers that drive everything else, in the order
                    // one goes through them: the worktree, then its branch.
                    .child(self.render_worktree_picker(cx))
                    .children(self.render_branch_picker(cx))
                    // What the project says of this worktree — started or not,
                    // and the address it exposes — then everything one can ask
                    // of it. They followed the row they were on.
                    .children(
                        active
                            .clone()
                            .and_then(|worktree| self.render_wt_state(&worktree, cx)),
                    )
                    .children(active.and_then(|worktree| self.render_wt_links(&worktree, cx)))
                    .children(self.render_worktree_actions(cx))
                    // The middle is empty on purpose, and the space is not
                    // lost: it is the window's drag region. Neither `fetch`, nor
                    // `pull`, nor `push` — they have moved down into the
                    // "Changes" panel's bar, where the rest of the gesture
                    // happens: tick, commit, push. The history and the branches
                    // are dock tabs. And the terminals have gone down to the
                    // status bar, at the corner of the window they open on.
                    .child(div().flex_1())
                    // The two screens one does not work in, at the far right
                    // of the title bar and in a group of their own: the
                    // multiplexer is where one goes to see what is running
                    // everywhere at once before leaving for the worktree it
                    // pointed at, and the settings are where one changes how
                    // the rest behaves. Neither is work, which is why they are
                    // out of the screen picker — that row is the row of the
                    // work — and a group is what says they are the same kind of
                    // detour rather than two loose icons. They stay screens:
                    // `Alt+6` and `Alt+7` still open them.
                    .child(
                        ButtonGroup::new("aside-nav")
                            .compact()
                            .xsmall()
                            .children(aside.map(|workspace| {
                                Button::new(("aside", workspace as usize))
                                    .icon(icon(workspace.icon()))
                                    .tooltip(tr!(workspace.label()))
                                    // The name is written, as in the screen
                                    // picker: an icon alone is a rebus one
                                    // learns rather than reads. **Except the
                                    // gear**, the one icon of this window that
                                    // needs no gloss: it is where an
                                    // application's settings are on every one of
                                    // them, and the word beside it only takes
                                    // width from the pickers.
                                    .when(
                                        workspace != crate::ui::workspace::Workspace::Settings,
                                        |button| button.label(tr!(workspace.label())),
                                    )
                                    // Solid against outline, as in the screen
                                    // picker: the "selected" state of an
                                    // outlined group is only a slightly lighter
                                    // background, invisible on half the themes.
                                    .map(|button| {
                                        if self.workspace == workspace {
                                            button.primary()
                                        } else {
                                            button.outline()
                                        }
                                    })
                            }))
                            .on_click(cx.listener(
                                move |this, selected: &Vec<usize>, window, cx| {
                                    let Some(workspace) =
                                        selected.first().and_then(|ix| aside.get(*ix).copied())
                                    else {
                                        return;
                                    };
                                    // The step is written down, as in the
                                    // screen picker: a detour to the settings
                                    // or to the multiplexer is exactly what one
                                    // wants `Ctrl+O` to undo.
                                    this.travel_to(workspace, window, cx);
                                },
                            )),
                    ),
            )
    }

    /// The application's menu.
    ///
    /// A single entry point for what is not about the repository being looked at
    /// — settings, layout, quit — rather than buttons scattered through a
    /// toolbar that talks about the current worktree.
    fn render_main_menu(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let entity = cx.entity();
        Button::new("main-menu")
            .ghost()
            .small()
            .icon(icon("menu"))
            .tooltip(tr!("menu-title"))
            .dropdown_menu(move |menu, window, cx| {
                let entity = entity.clone();
                let for_reset = entity.clone();
                let for_shortcuts = entity.clone();
                let for_views = entity.clone();
                menu.item(PopupMenuItem::new(tr!("settings-title")).on_click(
                    move |_, window, cx| {
                        entity.update(cx, |this, cx| this.open_settings(window, cx));
                    },
                ))
                // The shortcuts are what one looks for when one no longer knows:
                // they therefore live where one goes looking, beside the
                // settings, and not in a help one would have to guess at.
                .item(
                    PopupMenuItem::new(tr!("shortcuts-title")).on_click(move |_, window, cx| {
                        for_shortcuts.update(cx, |this, cx| this.open_shortcuts(window, cx));
                    }),
                )
                // Hidden views have no tab left: this is the only place to call
                // them back from, and therefore the only place that says what the
                // window is not showing.
                // The views of **this screen**, and not the eleven of the window:
                // hiding the SQL console from the review would make nothing
                // visibly change, and an entry with no effect reads as a broken
                // entry.
                .submenu(tr!("menu-views"), window, cx, move |menu, _window, cx| {
                    let workspace = for_views.read(cx).workspace;
                    let menu = workspace.views().iter().fold(menu, |menu, &(name, title)| {
                        menu.item(view_toggle(for_views.clone(), name, title))
                    });
                    // The plugins of **this screen**, after the built-in views:
                    // a hidden panel has no tab left, so this submenu is the
                    // only place that says what the window is not showing.
                    crate::ui::plugin_view::on_screen(workspace.key()).fold(
                        menu,
                        |menu, manifest| {
                            // Each **panel**, not each plugin: a master/detail
                            // plugin has two tabs, and hiding one is a gesture
                            // one has to be able to undo.
                            manifest.panels.iter().fold(menu, |menu, panel| {
                                menu.item(plugin_toggle(for_views.clone(), panel.name))
                            })
                        },
                    )
                })
                .item(PopupMenuItem::new(tr!("menu-reset-layout")).on_click(
                    move |_, window, cx| {
                        for_reset.update(cx, |this, cx| this.reset_layout(window, cx));
                    },
                ))
                .separator()
                .item(PopupMenuItem::new(tr!("menu-quit")).on_click(|_, _window, cx| cx.quit()))
            })
    }

    /// The worktree picker: the repository, the worktree, and the list to
    /// change them.
    ///
    /// It says the same thing the sidebar's selection does, and it is not a
    /// duplicate: the sidebar is a panel one can hide, drag or replace with a
    /// terminal, and what drives every view of the window cannot go away with
    /// it.
    fn render_worktree_picker(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let entity = cx.entity();
        let label = self
            .active_worktree()
            .map(|w| w.label())
            .unwrap_or_else(|| tr!("no-worktree").to_string());
        // The repository's name in front, greyed: two worktrees called `main`
        // in two repositories is the normal case, not the exception.
        let repo = self
            .active
            .as_deref()
            .and_then(|path| self.repo_of(path))
            .map(|repo| repo.name.clone());
        let muted = cx.theme().muted_foreground;
        Button::new("worktree-picker")
            .ghost()
            .small()
            .child(
                h_flex()
                    .gap_1()
                    .items_center()
                    .child(icon("folder").xsmall().text_color(muted))
                    .when_some(repo, |el, name| {
                        el.child(div().text_sm().text_color(muted).child(name))
                            .child(div().text_sm().text_color(muted.opacity(0.5)).child("/"))
                    })
                    .child(
                        div()
                            .max_w(px(240.))
                            .truncate()
                            .text_sm()
                            .font_semibold()
                            .child(label),
                    )
                    .child(icon("chevron-down").xsmall().text_color(muted)),
            )
            .dropdown_menu(move |menu, _window, cx| {
                let app = entity.clone();
                let active = app.read(cx).active.clone();
                // A snapshot of the list, taken here: the closures below run
                // while the menu paints, and reading the application from each
                // of them would be a borrow per row.
                let repos: Vec<(std::path::PathBuf, String, Vec<WorktreeItem>)> = app
                    .read(cx)
                    .repos
                    .iter()
                    .map(|repo| {
                        let worktrees = repo
                            .worktrees
                            .iter()
                            .map(|w| WorktreeItem {
                                path: w.path.clone(),
                                label: w.label(),
                                branch: w.branch.clone(),
                                is_main: w.is_main,
                                summary: app.read(cx).summaries.get(&w.path).copied(),
                                agent: app.read(cx).agents.get(&w.path).cloned(),
                            })
                            .collect();
                        (repo.main.clone(), repo.name.clone(), worktrees)
                    })
                    .collect();

                let mut menu = menu.min_w(px(280.)).max_h(px(420.)).scrollable(true);
                for (index, (main, name, worktrees)) in repos.into_iter().enumerate() {
                    menu = menu.item(repo_header(entity.clone(), index, main, name));
                    for worktree in worktrees {
                        let selected = active.as_deref() == Some(worktree.path.as_path());
                        let target = worktree.path.clone();
                        let is_main = worktree.is_main;
                        let app = app.clone();
                        menu = menu.item(
                            PopupMenuItem::element(move |_window, cx| {
                                worktree_row(&worktree, selected, cx)
                            })
                            // The icon column is the menu's own, and the tick
                            // takes its place when the row is the current one:
                            // it is what keeps the names lined up, and it is
                            // what every other menu of the window does.
                            .icon(icon(if is_main { "folder" } else { "git-branch" }))
                            .checked(selected)
                            .on_click(move |_, window, cx| {
                                app.update(cx, |this, cx| {
                                    this.select_worktree(target.clone(), window, cx)
                                });
                            }),
                        );
                    }
                }
                // The repositories that no longer open, last and in error. They
                // appear nowhere else now, and a repository that appears nowhere
                // cannot be removed either — it would only leave two warnings in
                // the log at every start.
                let missing: Vec<(std::path::PathBuf, String, String)> = app
                    .read(cx)
                    .repos
                    .missing()
                    .iter()
                    .map(|repo| {
                        (
                            repo.path.clone(),
                            repo_name(&repo.path),
                            repo.message.clone(),
                        )
                    })
                    .collect();
                if !missing.is_empty() {
                    menu = menu.separator();
                    for (index, (path, name, message)) in missing.into_iter().enumerate() {
                        menu = menu.item(missing_repo(entity.clone(), index, path, name, message));
                    }
                }
                let app = entity.clone();
                menu.separator().item(
                    PopupMenuItem::new(tr!("repo-open"))
                        .icon(icon("folder-plus"))
                        .on_click(move |_, window, cx| {
                            app.update(cx, |this, cx| this.prompt_open_repository(window, cx));
                        }),
                )
            })
    }

    /// The branch picker: what HEAD is on, and the branches to move it to.
    ///
    /// The list is `branches::rows_for`, the very one the panel used: locals
    /// under their heading, then the remotes with no local twin. `None` without
    /// a worktree — an empty picker offers a gesture that cannot be made.
    fn render_branch_picker(&self, cx: &mut Context<Self>) -> Option<impl IntoElement> {
        let worktree = self.active.clone()?;
        let repo = self.repo_of(&worktree)?;
        let main = repo.main.clone();
        let label = self
            .active_worktree()
            .and_then(|w| w.branch.clone())
            .unwrap_or_else(|| tr!("branch-detached").to_string());
        // The lead and the lag come with it: they are read as part of the
        // branch, and they were the other half of what the status bar said.
        let (ahead, behind) = self
            .active_review()
            .map(|r| (r.status.ahead, r.status.behind))
            .unwrap_or((0, 0));
        let rows = std::rc::Rc::new(crate::ui::branches::rows_for(&repo.branches, ""));
        let entity = cx.entity();
        let muted = cx.theme().muted_foreground;
        Some(
            Button::new("branch-picker")
                .ghost()
                .small()
                .child(
                    h_flex()
                        .gap_1()
                        .items_center()
                        .text_color(muted)
                        .child(icon("git-branch").xsmall())
                        .child(div().max_w(px(220.)).truncate().text_sm().child(label))
                        // Behind before ahead: that is what has to be integrated
                        // before one can push.
                        .when(behind > 0, |el| el.child(format!("↓{behind}")))
                        .when(ahead > 0, |el| el.child(format!("↑{ahead}")))
                        .child(icon("chevron-down").xsmall()),
                )
                .dropdown_menu(move |menu, _window, _cx| {
                    let mut menu = menu.min_w(px(320.)).max_h(px(420.)).scrollable(true);
                    for (index, row) in rows.iter().enumerate() {
                        match row {
                            crate::ui::branches::Row::Group(kind) => {
                                menu = menu.item(PopupMenuItem::label(match kind {
                                    crate::git::BranchKind::Local => tr!("branches-local"),
                                    crate::git::BranchKind::Remote => tr!("branches-remote"),
                                }));
                            }
                            crate::ui::branches::Row::Branch(row) => {
                                menu = menu.item(branch_item(
                                    entity.clone(),
                                    index,
                                    row.clone(),
                                    worktree.clone(),
                                    main.clone(),
                                ));
                            }
                        }
                    }
                    // The panel's other gesture, the one that does not need its
                    // search: a branch one creates has no name to look for yet.
                    let app = entity.clone();
                    menu.separator().item(
                        PopupMenuItem::new(tr!("branch-new"))
                            .icon(icon("plus"))
                            .on_click(move |_, window, cx| {
                                app.update(cx, |this, cx| this.prompt_new_branch(window, cx));
                            }),
                    )
                }),
        )
    }

    /// What can be done to the worktree being looked at: git on one side, the
    /// project's `wt.toml` on the other.
    ///
    /// `ClaudhubApp::worktree_menu`, unchanged — it was the sidebar row's right
    /// click. A right click needs a row to land on, and there is no list up
    /// here: it becomes a button, which is also what makes it findable.
    fn render_worktree_actions(&self, cx: &mut Context<Self>) -> Option<impl IntoElement> {
        let worktree = self.active.clone()?;
        let main = self.main_of(&worktree)?;
        let entity = cx.entity();
        Some(
            Button::new("worktree-actions")
                .ghost()
                .small()
                .icon(icon("ellipsis"))
                .dropdown_menu(move |menu, _window, cx| {
                    let (main, worktree) = (main.clone(), worktree.clone());
                    entity.update(cx, |this, cx| this.worktree_menu(menu, main, worktree, cx))
                }),
        )
    }
}

/// One worktree in the picker: what the sidebar row says, minus its buttons.
///
/// The volume and the agent come along because they are what one chooses on:
/// "the one where something is happening" is the commonest way of naming a
/// worktree.
fn worktree_row(
    worktree: &WorktreeItem,
    selected: bool,
    cx: &mut gpui::App,
) -> impl IntoElement + use<> {
    h_flex()
        .w_full()
        .gap_1()
        .items_center()
        .child(
            v_flex()
                .flex_1()
                .min_w_0()
                .child(
                    div()
                        .truncate()
                        .text_sm()
                        .when(selected, |el| el.font_semibold())
                        .child(worktree.label.clone()),
                )
                .when_some(worktree.branch.clone(), |el, branch| {
                    el.child(
                        div()
                            .truncate()
                            .text_xs()
                            .text_color(cx.theme().muted_foreground)
                            .child(branch),
                    )
                }),
        )
        .when_some(worktree.agent.as_ref(), |el, agent| {
            el.child(agent_badge(agent, cx))
        })
        .when_some(
            worktree.summary.filter(|summary| !summary.is_empty()),
            |el, summary| el.child(volume(summary, cx)),
        )
}

/// A repository's heading, and the one action that belongs to it.
///
/// The same two-line heading the sidebar draws, and the same `+`: the picker
/// took the sidebar's place at the top of the window, and a gesture that only
/// exists in a panel one has hidden is a gesture one no longer has.
/// Not clickable itself — hence `disabled` — but its button is: the click is
/// consumed, so the entry never sees it and the menu does not close on the row.
fn repo_header(
    app: Entity<ClaudhubApp>,
    index: usize,
    main: std::path::PathBuf,
    name: String,
) -> PopupMenuItem {
    PopupMenuItem::element(move |_window, cx| {
        let (app, main) = (app.clone(), main.clone());
        h_flex()
            .id(("repo-heading", index))
            .w_full()
            .gap_1()
            .items_center()
            .justify_between()
            .child(
                div()
                    .text_xs()
                    .font_semibold()
                    .text_color(cx.theme().muted_foreground)
                    .child(name.clone()),
            )
            .child(
                Button::new(("new-worktree", index))
                    .ghost()
                    .xsmall()
                    .icon(icon("plus"))
                    .tooltip(tr!("worktree-new"))
                    .on_click(move |_, window, cx| {
                        cx.stop_propagation();
                        app.update(cx, |this, cx| {
                            this.prompt_new_worktree(main.clone(), window, cx)
                        });
                    }),
            )
    })
    .disabled(true)
}

/// One branch's row: its name, then what it carries or who holds it.
fn branch_row(
    row: &crate::ui::branches::BranchRow,
    index: usize,
    taken: bool,
    main: std::path::PathBuf,
    app: Entity<ClaudhubApp>,
    cx: &mut gpui::App,
) -> impl IntoElement + use<> {
    let muted = cx.theme().muted_foreground;
    // Where it is checked out matters more than what it carries: that is what
    // explains a greyed row.
    let detail = match row.taken_by.as_ref().filter(|_| !row.is_head) {
        Some(path) => format!(
            "{} {}",
            tr!("branch-checked-out"),
            path.file_name().unwrap_or_default().to_string_lossy()
        ),
        None => row.detail.clone(),
    };
    let name = row.name.clone();
    h_flex()
        .w_full()
        .gap_2()
        .items_center()
        .child(
            v_flex()
                .flex_1()
                .min_w_0()
                .child(
                    div()
                        .truncate()
                        .text_sm()
                        .when(row.is_head, |el| el.font_semibold())
                        .child(row.name.clone()),
                )
                .when(!detail.is_empty(), |el| {
                    el.child(div().truncate().text_xs().text_color(muted).child(detail))
                }),
        )
        .when(row.behind > 0, |el| {
            el.child(
                div()
                    .text_xs()
                    .text_color(muted)
                    .child(format!("↓{}", row.behind)),
            )
        })
        .when(row.ahead > 0, |el| {
            el.child(
                div()
                    .text_xs()
                    .text_color(muted)
                    .child(format!("↑{}", row.ahead)),
            )
        })
        .when(!row.is_head && !taken, |el| {
            el.child(
                Button::new(("worktree-from", index))
                    .ghost()
                    .xsmall()
                    .icon(icon("folder-open"))
                    .tooltip(tr!("branch-new-worktree"))
                    .on_click(move |_, _window, cx| {
                        cx.stop_propagation();
                        app.update(cx, |this, cx| {
                            this.worktree_from_branch(main.clone(), name.clone(), cx)
                        });
                    }),
            )
        })
}

/// One branch in the picker: the row, and the two gestures on it.
///
/// The click checks the branch out; the button beside it opens a fresh worktree
/// on it, which is the opening gesture of a review when an agent's work has
/// landed on a branch nobody has checked out. Both are refused on a branch
/// another worktree already holds — git refuses two checkouts of the same
/// branch, and an entry that can only answer with an error is worse than a
/// greyed one that says who has it.
fn branch_item(
    app: Entity<ClaudhubApp>,
    index: usize,
    row: crate::ui::branches::BranchRow,
    worktree: std::path::PathBuf,
    main: std::path::PathBuf,
) -> PopupMenuItem {
    let taken = row.taken();
    let (is_head, checkout) = (row.is_head, row.name.clone());
    let (for_checkout, for_worktree) = (worktree.clone(), app.clone());
    PopupMenuItem::element(move |_window, cx| {
        branch_row(&row, index, taken, main.clone(), for_worktree.clone(), cx)
    })
    .icon(icon("git-branch"))
    .checked(is_head)
    .disabled(taken || is_head)
    .on_click(move |_, _window, cx| {
        app.update(cx, |this, cx| {
            this.start(
                Some(for_checkout.clone()),
                crate::runtime::Action::Checkout,
                Cmd::Checkout {
                    worktree: for_checkout.clone(),
                    branch: checkout.clone(),
                },
                cx,
            );
        });
    })
}

/// A repository that no longer opens, and the button that forgets it.
///
/// It stays on the list because a repository that appears nowhere cannot be
/// removed either: a moved folder, an erased clone, an unmounted partition left
/// two warnings in the log at every start and no way out but editing the
/// settings file by hand.
fn missing_repo(
    app: Entity<ClaudhubApp>,
    index: usize,
    path: std::path::PathBuf,
    name: String,
    message: String,
) -> PopupMenuItem {
    PopupMenuItem::element(move |_window, cx| {
        let (app, path) = (app.clone(), path.clone());
        h_flex()
            .id(("missing-repo", index))
            .w_full()
            .gap_2()
            .items_center()
            .child(
                icon("triangle-alert")
                    .xsmall()
                    .text_color(cx.theme().warning),
            )
            .child(
                v_flex()
                    .flex_1()
                    .min_w_0()
                    .child(
                        div()
                            .truncate()
                            .text_sm()
                            .text_color(cx.theme().muted_foreground)
                            .child(name.clone()),
                    )
                    // What git answered, and not just "not found": it is what
                    // says whether the folder moved or the disk is missing.
                    .child(
                        div()
                            .truncate()
                            .text_xs()
                            .text_color(cx.theme().warning)
                            .child(message.clone()),
                    ),
            )
            .child(
                Button::new(("forget-repo", index))
                    .ghost()
                    .xsmall()
                    .icon(icon("x"))
                    .tooltip(tr!("repo-forget"))
                    .on_click(move |_, window, cx| {
                        cx.stop_propagation();
                        app.update(cx, |this, cx| {
                            this.forget_repository(path.clone(), window, cx)
                        });
                    }),
            )
    })
    .disabled(true)
}
