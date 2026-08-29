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

use std::path::PathBuf;

use gpui::{div, prelude::*, px, Context, Entity, MouseButton, SharedString, Window};
use gpui_component::{
    button::{Button, ButtonGroup, ButtonVariants},
    h_flex,
    menu::{DropdownMenu, PopupMenuItem},
    ActiveTheme, Disableable, Sizable, TitleBar,
};

use crate::tr;
use crate::ui::app::ClaudhubApp;
use crate::ui::icons::icon;

/// A row of the title bar that is **not** the window's drag region.
///
/// Under Windows the bar answers `HTCAPTION` for its whole width — that is what
/// `TitleBar` asks for, and what makes the window draggable by it. A press
/// there arrives as `WM_NCLBUTTONDOWN`: gpui hands it to the view first, but
/// **only consumes it if something stops it propagating**. gpui's own click
/// listener merely records the press, so nothing did, and `DefWindowProc` then
/// entered the window-move loop — which swallows the release. The press was
/// seen, the click never happened: neither "Run", nor the multiplexer, nor the
/// settings could be pressed.
///
/// So everything that has something to click consumes its own press, and the
/// empty middle — which has nothing — stays the drag region. Windows only: on
/// Linux the move is started by us, from a `mouse_move`, and the whole bar can
/// go on being grabbed anywhere.
fn actions() -> gpui::Div {
    h_flex().items_center().gap_1().when(cfg!(windows), |this| {
        this.on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
    })
}

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
fn view_toggle(
    app: Entity<ClaudhubApp>,
    name: &'static str,
    title: crate::ui::rails::Label,
) -> PopupMenuItem {
    toggle_row(app, name, move || title.text())
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
        // **On the rail, and not on screen.** What this menu decides is whether
        // a view can be reached at all; whether it is folded away right now is
        // what its button says, one press from being undone.
        let on_rail = !app.read(cx).panel_off(name);
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
                    .when(on_rail, |this| this.child(icon("check").xsmall())),
            )
            .child(label())
            .on_click(move |_, _window, cx| {
                cx.stop_propagation();
                app.update(cx, |this, cx| this.toggle_panel_off(name, cx));
            })
    })
}

/// The volume of work in progress: lines added and removed.
///
/// The file count is only there for want of better — a rename or a binary moves
/// no line, and showing nothing would suggest there is nothing.
pub(super) fn volume(summary: crate::git::Summary, cx: &gpui::App) -> impl IntoElement {
    volume_on(summary, None, cx)
}

/// The same, painted in one given colour.
///
/// `on` is the foreground the ground underneath asks for: a selected pin is
/// filled with the accent, and the diff's green and its red — chosen against the
/// window's background — are then two of the least readable colours there are.
/// The signs stay, so nothing of what the badge means is lost with the hue.
pub(super) fn volume_on(
    summary: crate::git::Summary,
    on: Option<gpui::Hsla>,
    cx: &gpui::App,
) -> impl IntoElement {
    let mut colors = crate::ui::theme::DiffColors::of(cx);
    if let Some(on) = on {
        colors.added_fg = on;
        colors.removed_fg = on;
    }
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
                    .text_color(on.unwrap_or(cx.theme().muted_foreground))
                    .child(summary.files.to_string()),
            )
        })
}

/// An agent's badge: filled when it works, hollow when it waits.
///
/// A badge and not a word: the row already carries a name and a branch, and this
/// is information read out of the corner of the eye while scanning the list.
pub(super) fn agent_badge(agent: &crate::agent::State, cx: &gpui::App) -> impl IntoElement {
    let color = agent_colour(agent, cx);
    agent_dot(agent, cx)
        // The agent's name as soon as there is more than one profile to tell
        // apart: the badge says something is going on, it does not say who.
        .child(
            div()
                .text_xs()
                .text_color(color)
                .child(agent.programs.join(", ")),
        )
}

fn agent_colour(agent: &crate::agent::State, cx: &gpui::App) -> gpui::Hsla {
    if agent.working {
        cx.theme().warning
    } else {
        cx.theme().muted_foreground
    }
}

/// The badge without the name: the dot, and the count when there is more than
/// one.
///
/// It is what a pin's button carries — the button already says which checkout it
/// is, and the programs' names would take from the row the width it has to
/// spare. The picker's row has the space and says them.
pub(super) fn agent_dot(agent: &crate::agent::State, cx: &gpui::App) -> gpui::Div {
    agent_dot_on(agent, None, cx)
}

/// The same, painted in one given colour — `volume_on`'s reason, and the same
/// grounds.
pub(super) fn agent_dot_on(
    agent: &crate::agent::State,
    on: Option<gpui::Hsla>,
    cx: &gpui::App,
) -> gpui::Div {
    let color = on.unwrap_or_else(|| agent_colour(agent, cx));
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
        let for_close = cx.entity();
        TitleBar::new()
            .h(super::theme::toolbar_height(cx))
            // Our cross, under Linux, would `remove_window` without a word —
            // the same question as the window manager's cross, asked here.
            .on_close_window(move |_, window, cx| {
                for_close.update(cx, |this, cx| {
                    if this.quit_or_ask(window, cx) {
                        window.remove_window();
                    }
                });
            })
            .border_color(cx.theme().border)
            .bg(cx.theme().title_bar)
            .child(
                h_flex()
                    .w_full()
                    .h_full()
                    .pr_2()
                    .gap_1()
                    .items_center()
                    .child(
                        actions()
                            .child(self.render_main_menu(cx))
                            // The trail, before the pickers: it is the one thing here
                            // that speaks of *where one has been* rather than of what
                            // one is looking at, and it belongs to the title bar
                            // because it is the only chrome that crosses the screens
                            // the trail crosses. The editor's bar keeps its own two,
                            // on the same trail.
                            .child(self.render_trail_buttons(cx))
                            // The pinned checkouts come **before** the picker rather
                            // than after it: they are the shortest way to the same
                            // gesture, and what is put in front is what one reaches
                            // for first. The picker stays right behind them, for the
                            // checkouts one has not pinned.
                            .children(self.render_pins(cx))
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
                            ),
                    )
                    // The middle is empty on purpose, and the space is not
                    // lost: it is the window's drag region. Neither `fetch`, nor
                    // `pull`, nor `push` — they have moved down into the
                    // "Changes" panel's bar, where the rest of the gesture
                    // happens: tick, commit, push. The history and the branches
                    // are dock tabs. And the terminals have gone down to the
                    // status bar, at the corner of the window they open on.
                    .child(div().flex_1())
                    .child(
                        actions()
                            // Opening the worktree in the browser, then its actions,
                            // then the run button: the right corner is the "act on
                            // what I am looking at" corner, and the address a project
                            // exposes is the gesture one makes most once it runs.
                            .children(
                                active.and_then(|worktree| self.render_wt_links(&worktree, cx)),
                            )
                            .children(self.render_worktree_actions(cx))
                            // The run button, at the far right and just before the two
                            // screens one does not work in. A `justfile` is the
                            // project's commands, and running one is a gesture of its
                            // own: not one of the pickers that say what the window is
                            // talking about, and not one of the worktree's operations
                            // either — it is the corner one reaches for, next to the
                            // multiplexer, which is where what it starts ends up. It is
                            // painted only where there is a justfile with a recipe in
                            // it.
                            .children(self.render_just(cx))
                            // The gear, at the far right of the title bar. It
                            // needs no gloss: it is where an application's
                            // settings are on every one of them, and a word
                            // beside it would only take width from the pickers.
                            .child(
                                Button::new("settings")
                                    .icon(icon("settings"))
                                    .tooltip(tr!("workspace-settings"))
                                    .ghost()
                                    .xsmall()
                                    .on_click(cx.listener(|this, _, window, cx| {
                                        this.open_settings(window, cx);
                                    })),
                            ),
                    ),
            )
    }

    /// The pinned checkouts, as a group of buttons.
    ///
    /// A popover is the right shape for twelve checkouts one browses; it is the
    /// wrong one for the two or three one goes back and forth between all day.
    /// A pin is what says which those are, and the row is read left to right in
    /// the order they were pinned — nothing reorders itself under the hand.
    ///
    /// Nothing is painted when nothing is pinned: an empty group would take the
    /// pickers' width to say that a feature exists.
    fn render_pins(&self, cx: &mut Context<Self>) -> Option<impl IntoElement> {
        // Shared, not copied twice: the click handler outlives the frame and
        // used to take a second list of its own.
        let pins: std::rc::Rc<[PathBuf]> = self.pinned_worktrees(cx).into();
        if pins.is_empty() {
            return None;
        }
        let active = self.active_path();
        let muted = cx.theme().muted_foreground;
        // What a selected pin is painted with. A solid button's ground is the
        // accent, and everything the row carries beside its name — the
        // repository, the agent, the volume — is coloured against the *window's*
        // background: greys and pastels, which the accent swallows. On One Dark
        // the pin read as three illegible marks. They therefore take the ground's
        // own foreground, the one the theme picked to be read on it.
        let on_accent = cx.theme().primary_foreground;
        let for_click = pins.clone();
        Some(
            ButtonGroup::new("worktree-pins")
                .compact()
                .xsmall()
                .children(pins.iter().enumerate().map(|(index, path)| {
                    let (repo, label) = self.project_label(path);
                    let selected = active.as_deref() == Some(path.as_path());
                    let on_selected = selected.then_some(on_accent);
                    Button::new(("pin", index))
                        .tooltip(SharedString::from(path.display().to_string()))
                        .child(
                            h_flex()
                                .gap_1()
                                .items_center()
                                // The repository's name in front and greyed,
                                // exactly as the picker's trigger says it, and
                                // dropped when it would repeat the checkout's
                                // own name — the multiplexer's rule, and the
                                // same helper.
                                .children(repo.map(|name| {
                                    div()
                                        .text_xs()
                                        .text_color(if selected {
                                            on_accent.opacity(0.7)
                                        } else {
                                            muted.opacity(0.8)
                                        })
                                        .child(name)
                                }))
                                .child(div().max_w(px(140.)).truncate().child(label))
                                // Who is working in it and how much is in
                                // progress — the two things one glances at
                                // before switching, and the two the picker's
                                // row already carries. They come from the
                                // background sweep, so a pin says what it says
                                // of a checkout nobody has opened: the agent
                                // every two seconds, the volume every fifth
                                // reading.
                                .children(
                                    self.agents
                                        .get(path)
                                        .map(|agent| agent_dot_on(agent, on_selected, cx)),
                                )
                                .children(
                                    self.summaries
                                        .get(path)
                                        .copied()
                                        .filter(|summary| !summary.is_empty())
                                        .map(|summary| volume_on(summary, on_selected, cx)),
                                ),
                        )
                        // Solid against outline, the window's polarity: the
                        // "selected" state of an outlined group is a background
                        // a few percent off its own, invisible on half the
                        // themes.
                        .map(|button| {
                            if selected {
                                button.primary()
                            } else {
                                button.outline()
                            }
                        })
                }))
                .on_click(cx.listener(move |this, selected: &Vec<usize>, window, cx| {
                    let Some(path) = selected.first().and_then(|ix| for_click.get(*ix).cloned())
                    else {
                        return;
                    };
                    this.select_worktree(path, window, cx);
                })),
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
                let for_quit = entity.clone();
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
                // **What a rail can carry, and the tick says whether it does.**
                // A view taken off has no button at all — that is what taking
                // it off means — so this menu is the only way back, and it has
                // to list everything that can be off. What it leaves out is the
                // situational views, whose button comes and goes with their
                // content: offering to take one off is offering to remove what
                // is not there.
                //
                // One list now, and not one per screen: there is one window.
                .submenu(tr!("menu-views"), window, cx, move |menu, _window, cx| {
                    let off = for_views.read(cx).rail_states().1;
                    let menu = crate::ui::rails::TOOLS
                        .iter()
                        .filter(|tool| crate::ui::rails::in_view_menu(tool, &off))
                        .fold(menu, |menu, tool| {
                            menu.item(view_toggle(for_views.clone(), tool.panel, tool.title))
                        });
                    // The plugins after the built-in views. Each **panel**, not
                    // each plugin: a master/detail plugin has two tabs, and
                    // hiding one is a gesture one has to be able to undo.
                    crate::ui::plugin_view::manifests()
                        .iter()
                        .fold(menu, |menu, manifest| {
                            manifest.panels.iter().fold(menu, |menu, panel| {
                                menu.item(plugin_toggle(for_views.clone(), panel.name))
                            })
                        })
                })
                .item(PopupMenuItem::new(tr!("menu-reset-layout")).on_click(
                    move |_, window, cx| {
                        for_reset.update(cx, |this, cx| this.reset_layout(window, cx));
                    },
                ))
                .separator()
                .item(
                    PopupMenuItem::new(tr!("menu-quit")).on_click(move |_, window, cx| {
                        for_quit.update(cx, |this, cx| {
                            if this.quit_or_ask(window, cx) {
                                cx.quit();
                            }
                        });
                    }),
                )
            })
    }

    /// What can be done to the worktree being looked at: git on one side, the
    /// project's `wt.toml` on the other.
    ///
    /// `ClaudhubApp::worktree_menu`, unchanged — it was the sidebar row's right
    /// click. A right click needs a row to land on, and there is no list up
    /// here: it becomes a button, which is also what makes it findable.
    /// The run button: the default recipe on the left, all of them under the
    /// chevron.
    ///
    /// Two buttons and not a menu alone. What a bare `just` runs is the recipe
    /// the project put first, which is the one one runs twenty times a day, and
    /// making it cost a menu would be making the common gesture pay for the
    /// rare one. The chevron is where the rest lives.
    ///
    /// The recipes are read from what the worker brought back — never from the
    /// disk here, and never a subprocess in a render.
    fn render_just(&mut self, cx: &mut Context<Self>) -> Option<impl IntoElement> {
        let worktree = self.active.clone()?;
        // The snapshot is shared rather than copied: the recipes go into the
        // menu's `'static` closure, and this runs on every frame of the bar.
        let snapshot = self.just_recipes(&worktree)?;
        let default = snapshot.default.clone()?;
        let entity = cx.entity();
        let run = {
            let (worktree, default) = (worktree.clone(), default.clone());
            let entity = entity.clone();
            Button::new("just-run")
                .ghost()
                .small()
                .icon(icon("play"))
                .label(tr!("just-run"))
                // What the click actually runs, written out: "Run" alone leaves
                // one to guess which recipe, and a project's first recipe is not
                // always the one its name suggests.
                .tooltip(SharedString::from(format!("just {default}")))
                .on_click(move |_, window, cx| {
                    let (worktree, default) = (worktree.clone(), default.clone());
                    entity.update(cx, |this, cx| this.run_just(worktree, default, window, cx));
                })
        };
        // Nothing to unfold when the default recipe is the only one: a chevron
        // opening a menu of one is a click that says nothing.
        let more = (snapshot.recipes.len() > 1).then(|| {
            Button::new("just-recipes")
                .ghost()
                .small()
                .icon(icon("chevron-down"))
                .tooltip(tr!("just-recipes"))
                .dropdown_menu(move |menu, _window, _cx| {
                    snapshot.recipes.iter().fold(menu, |menu, recipe| {
                        let (worktree, name) = (worktree.clone(), recipe.name.clone());
                        let entity = entity.clone();
                        // The recipe as `just --list` writes it, its doc
                        // comment after: the menu says what the tool says, and
                        // what a recipe takes is part of what one reads before
                        // clicking.
                        let label = if recipe.doc.is_empty() {
                            recipe.signature()
                        } else {
                            format!("{} — {}", recipe.signature(), recipe.doc)
                        };
                        menu.item(
                            PopupMenuItem::new(SharedString::from(label))
                                .icon(icon("terminal"))
                                .on_click(move |_, window, cx| {
                                    let (worktree, name) = (worktree.clone(), name.clone());
                                    entity.update(cx, |this, cx| {
                                        this.run_just(worktree, name, window, cx)
                                    });
                                }),
                        )
                    })
                })
        });
        Some(h_flex().items_center().child(run).children(more))
    }

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
