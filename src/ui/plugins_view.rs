//! The Plugins screen: where the focus view's scripts are made — listed,
//! previewed for real on a worktree, and written by an agent of their own.
//!
//! An entry at the foot of the home screen's sidebar opens it in the place
//! of the boards. Three columns: the scripts; the one selected, drawn on a
//! worktree one picks, its last error kept under it rather than said once
//! in a bubble; and the agent that edits them — a chat (Claude, Codex,
//! Gemini…) in the scripts' folder, told what it is there for by the
//! instructions files it reads of itself (`scripts::instructions`). The agent
//! learns whether a save worked from the status file Claudhub rewrites
//! (`scripts::STATUS`): the preview is what loads the selected script, so
//! what it reports is what the user sees.
//!
//! The agent's chat is not brought back at the next launch: the scripts'
//! folder is no worktree, and only worktrees' chats are revived.

use std::path::{Path, PathBuf};

use gpui_kit::component::{
    button::{Button, ButtonVariants as _},
    h_flex,
    menu::{DropdownMenu as _, PopupMenuItem},
    switch::Switch,
    v_flex, ActiveTheme, Disableable as _, Selectable as _, Sizable as _,
};
use gpui_kit::{div, prelude::*, px, AnyElement, Context, SharedString, Window};

use crate::scripts::{Kind, Origin, Script};
use crate::tr;
use crate::ui::app::ClaudhubApp;
use crate::ui::icons::icon;

/// The list of scripts, left of the preview.
const LIST_WIDTH: f32 = 260.;
/// Where the divider between the preview and the agent starts.
const PREVIEW_START: f32 = 620.;

impl ClaudhubApp {
    /// Shows the Plugins screen in the place of the boards.
    pub(super) fn open_plugins(&mut self, cx: &mut Context<Self>) {
        self.scripts.screen.open = true;
        self.watch_scripts(cx);
        self.write_scripts_status(cx);
        cx.notify();
    }

    /// Back to the boards.
    pub(super) fn close_plugins(&mut self, cx: &mut Context<Self>) {
        if std::mem::take(&mut self.scripts.screen.open) {
            cx.notify();
        }
    }

    /// The sidebar's entry: a row like a worktree's, lit while the screen
    /// is open — or, `compact`, the rail's icon.
    pub(super) fn plugins_entry(&self, compact: bool, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme().clone();
        let lit = self.scripts.screen.open;
        let toggle = cx.listener(|this, _, _, cx| {
            if this.scripts.screen.open {
                this.close_plugins(cx);
            } else {
                this.open_plugins(cx);
            }
        });
        if compact {
            return Button::new("plugins-entry")
                .ghost()
                .small()
                .icon(icon("square-kanban"))
                .selected(lit)
                .tooltip(tr!("plugins-title"))
                .on_click(toggle)
                .into_any_element();
        }
        h_flex()
            .id("plugins-entry")
            .w_full()
            .pl_3()
            .pr_2()
            .py_1()
            .gap_2()
            .items_center()
            .cursor_pointer()
            .border_l_2()
            .border_color(if lit {
                theme.ring
            } else {
                gpui_kit::transparent_black()
            })
            .when(lit, |el| el.bg(theme.list_active))
            .when(!lit, |el| el.hover(|style| style.bg(theme.list_hover)))
            .on_click(toggle)
            .child(icon("square-kanban").xsmall().text_color(if lit {
                theme.ring
            } else {
                theme.muted_foreground
            }))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .text_sm()
                    .text_color(theme.foreground)
                    .child(tr!("plugins-title")),
            )
            .child(
                div()
                    .text_xs()
                    .text_color(theme.muted_foreground)
                    .child(SharedString::from(self.scripts.count().to_string())),
            )
            .into_any_element()
    }

    /// The screen: the scripts, the preview, the agent.
    pub(super) fn render_plugins(
        &mut self,
        live: &[PathBuf],
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        // The preview's worktree: the one chosen while it lives, else the
        // one on show, else the first.
        let preview_on = self
            .scripts
            .screen
            .preview_on
            .clone()
            .filter(|path| live.contains(path))
            .or_else(|| self.active.clone().filter(|path| live.contains(path)))
            .or_else(|| live.first().cloned());
        if self.scripts.screen.preview_on != preview_on {
            self.scripts.screen.preview_on = preview_on.clone();
            self.write_scripts_status(cx);
        }
        let list = self.plugins_list(cx);
        let catalog = self
            .scripts
            .screen
            .catalog
            .clone()
            .filter(|_| self.scripts.screen.selected.is_none());
        let preview = match catalog {
            Some((slug, folder)) => self.market_card(&slug, &folder, window, cx),
            None => self.plugins_preview(live, preview_on.as_deref(), window, cx),
        };
        let agent = self.plugins_agent(window, cx);
        let sides = match crate::ui::scripts::root() {
            Some(root) => self.two_sides(&root, "plugins", PREVIEW_START, preview, agent, cx),
            None => preview,
        };
        h_flex()
            .size_full()
            .gap_3()
            .child(list)
            .child(div().flex_1().min_w_0().h_full().child(sides))
            .into_any_element()
    }

    /// The scripts, by kind, the broken ones with why; what creates one, and
    /// the folder they are in.
    fn plugins_list(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme().clone();
        let selected = self.scripts.screen.selected.clone();
        let rows: Vec<AnyElement> = [Kind::Tab, Kind::Home]
            .into_iter()
            .flat_map(|kind| self.scripts.of_kind(kind))
            .map(|script| {
                let lit = selected.as_deref() == Some(script.id.as_str());
                let enabled = super::scripts::script_enabled(&script.id, cx);
                let id = script.id.clone();
                let kind = match script.kind {
                    Kind::Tab => tr!("settings-script-tab"),
                    Kind::Home => tr!("settings-script-home"),
                };
                let origin = match script.origin {
                    Origin::User => None,
                    Origin::Builtin => Some(tr!("plugins-builtin")),
                    Origin::Fork => Some(tr!("plugins-fork")),
                    Origin::Market => Some(tr!("plugins-market")),
                };
                let switch = Switch::new(SharedString::from(format!("plugins-on-{id}")))
                    .xsmall()
                    .checked(enabled)
                    .tooltip(if enabled {
                        tr!("plugins-disable")
                    } else {
                        tr!("plugins-enable")
                    })
                    .on_click({
                        let (app, id) = (cx.entity().downgrade(), id.clone());
                        move |on, _, cx| {
                            let _ =
                                app.update(cx, |this, cx| this.set_script_enabled(&id, *on, cx));
                        }
                    });
                v_flex()
                    .id(SharedString::from(format!("plugins-row-{id}")))
                    .w_full()
                    .px_3()
                    .py_1p5()
                    .cursor_pointer()
                    .border_l_2()
                    .border_color(if lit {
                        theme.ring
                    } else {
                        gpui_kit::transparent_black()
                    })
                    .when(lit, |el| el.bg(theme.list_active))
                    .when(!lit, |el| el.hover(|style| style.bg(theme.list_hover)))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.scripts.screen.selected = Some(id.clone());
                        this.scripts.screen.catalog = None;
                        this.write_scripts_status(cx);
                        cx.notify();
                    }))
                    .child(
                        h_flex()
                            .w_full()
                            .gap_2()
                            .items_center()
                            .child(
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .truncate()
                                    .text_sm()
                                    .when(!enabled, |el| el.text_color(theme.muted_foreground))
                                    .child(SharedString::from(script.title.clone())),
                            )
                            .child(switch),
                    )
                    .child(
                        h_flex()
                            .gap_2()
                            .text_xs()
                            .text_color(theme.muted_foreground)
                            .child(
                                div()
                                    .font_family(theme.mono_font_family.clone())
                                    .child(SharedString::from(script.id.clone())),
                            )
                            .child(kind)
                            .children(script.version.as_ref().map(|version| {
                                div().child(tr!("plugins-version", { version: version.clone() }))
                            }))
                            .children(origin.map(|origin| {
                                div()
                                    .text_color(if script.origin == Origin::Fork {
                                        theme.warning
                                    } else {
                                        theme.ring
                                    })
                                    .child(origin)
                            })),
                    )
                    .into_any_element()
            })
            .collect();
        let broken: Vec<AnyElement> = self
            .scripts
            .broken()
            .iter()
            .map(|(id, why)| {
                v_flex()
                    .w_full()
                    .px_3()
                    .py_1p5()
                    .text_xs()
                    .child(
                        div()
                            .font_family(theme.mono_font_family.clone())
                            .child(SharedString::from(id.clone())),
                    )
                    .child(
                        div()
                            .text_color(theme.danger)
                            .child(SharedString::from(why.clone())),
                    )
                    .into_any_element()
            })
            .collect();
        let empty = (rows.is_empty() && broken.is_empty()).then(|| {
            div()
                .px_3()
                .py_2()
                .text_sm()
                .text_color(theme.muted_foreground)
                .child(tr!("plugins-empty"))
        });
        let root = crate::ui::scripts::root();
        let new = Button::new("plugins-new")
            .ghost()
            .xsmall()
            .icon(icon("plus"))
            .tooltip(tr!("plugins-new"))
            .dropdown_menu({
                let app = cx.entity().downgrade();
                move |menu, _, _| {
                    let (tab, home) = (app.clone(), app.clone());
                    menu.item(
                        PopupMenuItem::new(tr!("plugins-new-tab"))
                            .icon(icon("layout-dashboard"))
                            .on_click(move |_, _, cx| {
                                let _ =
                                    tab.update(cx, |this, cx| this.create_script(Kind::Tab, cx));
                            }),
                    )
                    .item(
                        PopupMenuItem::new(tr!("plugins-new-home"))
                            .icon(icon("house"))
                            .on_click(move |_, _, cx| {
                                let _ =
                                    home.update(cx, |this, cx| this.create_script(Kind::Home, cx));
                            }),
                    )
                }
            });
        let folder = Button::new("plugins-folder")
            .ghost()
            .xsmall()
            .icon(icon("folder-open"))
            .tooltip(tr!("settings-scripts-open"))
            .disabled(root.is_none())
            .on_click(move |_, _, cx| {
                let Some(root) = root.clone() else {
                    return;
                };
                cx.spawn(async move |cx| {
                    let made = root.clone();
                    let made = cx
                        .background_executor()
                        .spawn(async move { std::fs::create_dir_all(made) })
                        .await;
                    if let Err(error) = made {
                        log::warn!("scripts folder: {error}");
                    }
                    cx.update(|cx| cx.open_with_system(&root));
                })
                .detach();
            });
        v_flex()
            .flex_none()
            .w(px(LIST_WIDTH))
            .h_full()
            .overflow_hidden()
            .rounded(theme.radius_lg)
            .border_1()
            .border_color(theme.border)
            .bg(theme.background)
            .child(
                h_flex()
                    .flex_none()
                    .w_full()
                    .h(super::theme::toolbar_height(cx))
                    .px_3()
                    .gap_1()
                    .items_center()
                    .border_b_1()
                    .border_color(theme.border)
                    .child(
                        div()
                            .flex_1()
                            .text_sm()
                            .font_weight(gpui_kit::FontWeight::SEMIBOLD)
                            .child(tr!("plugins-title")),
                    )
                    .child(folder)
                    .child(new),
            )
            .child(
                v_flex()
                    .id("plugins-list")
                    .flex_1()
                    .min_h_0()
                    .py_1()
                    .overflow_y_scroll()
                    .children(rows)
                    .children(broken)
                    .children(empty)
                    .child(self.market_list(cx)),
            )
            .into_any_element()
    }

    /// The selected script, drawn on `preview_on` as a board would draw it,
    /// with the worktree to draw it on and its last error.
    fn plugins_preview(
        &mut self,
        live: &[PathBuf],
        preview_on: Option<&Path>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let theme = cx.theme().clone();
        let selected = self
            .scripts
            .screen
            .selected
            .clone()
            .filter(|id| self.scripts.script(id).is_some());
        let label = |this: &Self, path: &Path| -> SharedString {
            let (project, name) = this.project_label(path);
            match project {
                Some(project) if project != name => format!("{project} · {name}").into(),
                _ => name,
            }
        };
        let choices: Vec<(PathBuf, SharedString)> = live
            .iter()
            .map(|path| (path.clone(), label(self, path)))
            .collect();
        let picker = Button::new("plugins-preview-on")
            .ghost()
            .xsmall()
            .icon(icon("git-branch"))
            .label(
                preview_on
                    .map(|path| label(self, path))
                    .unwrap_or_else(|| tr!("plugins-no-worktree")),
            )
            .dropdown_menu({
                let app = cx.entity().downgrade();
                move |menu, _, _| {
                    choices.iter().fold(menu, |menu, (path, name)| {
                        let (app, path) = (app.clone(), path.clone());
                        menu.item(PopupMenuItem::new(name.clone()).on_click(move |_, _, cx| {
                            let _ = app.update(cx, |this, cx| {
                                this.scripts.screen.preview_on = Some(path.clone());
                                this.write_scripts_status(cx);
                                cx.notify();
                            });
                        }))
                    })
                }
            });
        // A builtin is not opened — an update would write over what was
        // changed in it —: it is forked. A fork goes back to it.
        let gestures: Vec<AnyElement> = selected
            .as_ref()
            .and_then(|id| self.scripts.script(id))
            .map(|script| {
                let entry = script.dir.join(&script.entry);
                let open = Button::new("plugins-open-entry")
                    .ghost()
                    .xsmall()
                    .icon(icon("external-link"))
                    .tooltip(tr!("plugins-open-entry"))
                    .on_click(move |_, _, cx| cx.open_with_system(&entry));
                let id = script.id.clone();
                match script.origin {
                    Origin::User => vec![
                        open.into_any_element(),
                        self.plugins_rename_button(script, cx),
                        self.plugins_remove_button(script, cx),
                    ],
                    Origin::Builtin => vec![Button::new("plugins-fork")
                        .ghost()
                        .xsmall()
                        .icon(icon("git-fork"))
                        .label(tr!("plugins-fork-action"))
                        .tooltip(tr!("plugins-fork-help"))
                        .on_click(cx.listener(move |this, _, _, cx| this.fork_script(&id, cx)))
                        .into_any_element()],
                    Origin::Fork => vec![
                        open.into_any_element(),
                        Button::new("plugins-unfork")
                            .ghost()
                            .xsmall()
                            .icon(icon("undo-2"))
                            .label(tr!("plugins-unfork-action"))
                            .tooltip(tr!("plugins-unfork-help"))
                            .on_click(
                                cx.listener(move |this, _, _, cx| this.unfork_script(&id, cx)),
                            )
                            .into_any_element(),
                    ],
                    Origin::Market => self.plugins_market_gestures(script, cx),
                }
            })
            .unwrap_or_default();
        let title = selected
            .as_ref()
            .and_then(|id| self.scripts.script(id))
            .map(|script| SharedString::from(script.title.clone()))
            .unwrap_or_else(|| tr!("plugins-preview"));
        let body = match (&selected, preview_on) {
            (Some(id), Some(path)) => {
                let failure = self
                    .script_failure(path, id)
                    .map(|why| SharedString::from(why.to_string()));
                let view = self.script_element(path, id, window, cx);
                v_flex()
                    .size_full()
                    .gap_2()
                    .child(div().flex_1().min_h_0().child(view))
                    .children(failure.map(|why| {
                        div()
                            .flex_none()
                            .max_h(px(160.))
                            .p_2()
                            .rounded(theme.radius)
                            .border_1()
                            .border_color(theme.danger)
                            .text_xs()
                            .font_family(theme.mono_font_family.clone())
                            .text_color(theme.danger)
                            .child(why)
                    }))
                    .into_any_element()
            }
            (None, _) => super::theme::centered_note(tr!("plugins-select"), cx),
            (Some(_), None) => super::theme::centered_note(tr!("plugins-no-worktree"), cx),
        };
        v_flex()
            .size_full()
            .gap_2()
            .child(
                h_flex()
                    .flex_none()
                    .w_full()
                    .h(super::theme::toolbar_height(cx))
                    .gap_2()
                    .items_center()
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .text_sm()
                            .font_weight(gpui_kit::FontWeight::SEMIBOLD)
                            .child(title),
                    )
                    .children(gestures)
                    .child(picker),
            )
            .children(
                selected
                    .as_deref()
                    .and_then(|id| self.plugins_permissions(id, cx)),
            )
            .child(div().flex_1().min_h_0().w_full().child(body))
            .into_any_element()
    }

    /// What is done to a plugin installed from a marketplace: brought up to
    /// the catalog's version, forked, uninstalled — asked first, its data
    /// and secrets going with it.
    fn plugins_market_gestures(&self, script: &Script, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let id = script.id.clone();
        let mut gestures = Vec::new();
        if let Some(from) = self.plugin_provenance(&id) {
            gestures.push(
                div()
                    .text_xs()
                    .font_family(cx.theme().mono_font_family.clone())
                    .text_color(cx.theme().muted_foreground)
                    .child(from)
                    .into_any_element(),
            );
        }
        if let Some((slug, folder)) = self.plugin_update(&id) {
            gestures.push(
                Button::new("plugins-market-update")
                    .ghost()
                    .xsmall()
                    .icon(icon("download"))
                    .label(tr!("market-update"))
                    .on_click(
                        cx.listener(move |this, _, _, cx| this.install_plugin(&slug, &folder, cx)),
                    )
                    .into_any_element(),
            );
        }
        let fork = id.clone();
        gestures.push(
            Button::new("plugins-fork")
                .ghost()
                .xsmall()
                .icon(icon("git-fork"))
                .label(tr!("plugins-fork-action"))
                .tooltip(tr!("market-fork-help"))
                .on_click(cx.listener(move |this, _, _, cx| this.fork_script(&fork, cx)))
                .into_any_element(),
        );
        let title = script.title.clone();
        gestures.push(
            Button::new("plugins-uninstall")
                .ghost()
                .xsmall()
                .icon(icon("trash-2"))
                .tooltip(tr!("market-uninstall"))
                .on_click(cx.listener(move |_, _, window, cx| {
                    let id = id.clone();
                    super::dialogs::ask(
                        cx.entity(),
                        tr!("market-uninstall-title", { title: title.clone() }),
                        || {
                            div()
                                .text_sm()
                                .child(tr!("market-uninstall-body"))
                                .into_any_element()
                        },
                        super::dialogs::confirm,
                        move |this, _, cx| this.uninstall_plugin(&id, cx),
                        window,
                        cx,
                    );
                }))
                .into_any_element(),
        );
        gestures
    }

    /// Renaming a script of the user's: its folder, which is its id.
    fn plugins_rename_button(&self, script: &Script, cx: &mut Context<Self>) -> AnyElement {
        let (id, title) = (script.id.clone(), script.title.clone());
        Button::new("plugins-rename")
            .ghost()
            .xsmall()
            .icon(icon("pencil"))
            .tooltip(tr!("plugins-rename"))
            .on_click(cx.listener(move |this, _, window, cx| {
                let from = id.clone();
                this.open_text_dialog_with(
                    tr!("plugins-rename-title", { title: title.clone() }),
                    SharedString::from(id.clone()),
                    id.clone(),
                    window,
                    cx,
                    move |this, to, _, cx| this.rename_script(&from, &to, cx),
                );
            }))
            .into_any_element()
    }

    /// Removing a script of the user's, asked first.
    fn plugins_remove_button(&self, script: &Script, cx: &mut Context<Self>) -> AnyElement {
        let (id, title) = (script.id.clone(), script.title.clone());
        let aside = crate::ui::scripts::root()
            .map(|root| root.join(crate::scripts::REMOVED).display().to_string())
            .unwrap_or_default();
        Button::new("plugins-remove")
            .ghost()
            .xsmall()
            .icon(icon("trash-2"))
            .tooltip(tr!("plugins-remove"))
            .on_click(cx.listener(move |_, _, window, cx| {
                let (id, aside) = (id.clone(), aside.clone());
                super::dialogs::ask(
                    cx.entity(),
                    tr!("plugins-remove-title", { title: title.clone() }),
                    move || {
                        div()
                            .text_sm()
                            .child(tr!("plugins-remove-body", { path: aside.clone() }))
                            .into_any_element()
                    },
                    super::dialogs::confirm,
                    move |this, _, cx| this.remove_script(&id, cx),
                    window,
                    cx,
                );
            }))
            .into_any_element()
    }

    /// What the selected script keeps, and the gestures on it: its secrets,
    /// to forget; its data, to clear. Nothing when it keeps nothing. The
    /// network is every script's, and asks nothing.
    fn plugins_permissions(&mut self, id: &str, cx: &mut Context<Self>) -> Option<AnyElement> {
        let theme = cx.theme().clone();
        let script = self.scripts.script(id)?.clone();
        let has_data = self.has_store(id);
        if script.permissions.secrets.is_empty() && !has_data {
            return None;
        }
        let line = |label: SharedString| {
            h_flex().w_full().gap_2().items_center().text_xs().child(
                div()
                    .flex_none()
                    .w(px(90.))
                    .text_color(theme.muted_foreground)
                    .child(label),
            )
        };
        let secrets = (!script.permissions.secrets.is_empty()).then(|| {
            let names: Vec<(String, String)> = script
                .permissions
                .secrets
                .iter()
                .map(|secret| (secret.name.clone(), secret.label.clone()))
                .collect();
            let (id, forgotten) = (id.to_string(), names.clone());
            let app = cx.entity().downgrade();
            line(tr!("plugins-secrets"))
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .truncate()
                        .child(SharedString::from(
                            names
                                .iter()
                                .map(|(_, label)| label.as_str())
                                .collect::<Vec<_>>()
                                .join(", "),
                        )),
                )
                .child(
                    Button::new("plugins-forget-secrets")
                        .ghost()
                        .xsmall()
                        .label(tr!("plugins-forget-secrets"))
                        .on_click(move |_, _, cx| {
                            // The keyring may answer slowly: never in a click.
                            let (id, forgotten, app) = (id.clone(), forgotten.clone(), app.clone());
                            cx.spawn(async move |cx| {
                                cx.background_executor()
                                    .spawn(async move {
                                        for (name, _) in &forgotten {
                                            super::scripts::forget_secret(&id, name);
                                        }
                                    })
                                    .await;
                                let _ = app.update(cx, |this, cx| {
                                    this.announce(tr!("plugins-secrets-forgotten"), cx)
                                });
                            })
                            .detach();
                        }),
                )
        });
        let data = has_data.then(|| {
            let (app, id) = (cx.entity().downgrade(), id.to_string());
            let path = Self::store_path(&id)
                .map(|path| SharedString::from(path.display().to_string()))
                .unwrap_or_default();
            line(tr!("plugins-data"))
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .truncate()
                        .font_family(theme.mono_font_family.clone())
                        .text_color(theme.muted_foreground)
                        .child(path),
                )
                .child(
                    Button::new("plugins-clear-data")
                        .ghost()
                        .xsmall()
                        .label(tr!("plugins-clear-data"))
                        .on_click(move |_, _, cx| {
                            let _ = app.update(cx, |this, cx| {
                                this.clear_store(&id, cx);
                                this.refresh_scripts(cx);
                            });
                        }),
                )
        });
        Some(
            v_flex()
                .flex_none()
                .w_full()
                .gap_1p5()
                .p_2()
                .rounded(theme.radius)
                .border_1()
                .border_color(theme.border)
                .bg(theme.background)
                .children(secrets)
                .children(data)
                .into_any_element(),
        )
    }

    /// The agent that edits the scripts: its chat, or — none open — the
    /// agents one can talk to, to pick one.
    fn plugins_agent(&mut self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme().clone();
        let chat = self.scripts.screen.agent.and_then(|id| self.chat_by_id(id));
        if let Some(chat) = chat {
            return super::summary_view::chat_frame(chat, window, &theme, cx);
        }
        let agents = super::settings::Settings::global(cx).terminal.chat_agents();
        let offers: Vec<AnyElement> = if agents.is_empty() {
            let app = cx.entity().downgrade();
            vec![Button::new("plugins-chat-configure")
                .small()
                .ghost()
                .icon(icon("message-square-plus"))
                .label(tr!("chat-configure"))
                .on_click(move |_, window, cx| super::panels::open_chat_settings(&app, window, cx))
                .into_any_element()]
        } else {
            agents
                .into_iter()
                .enumerate()
                .map(|(n, agent)| {
                    Button::new(("plugins-chat-open", n))
                        .small()
                        .when(n == 0, |button| button.primary())
                        .icon(icon("message-square-plus"))
                        .label(tr!("chat-with", { agent: agent.label() }))
                        .on_click(cx.listener(move |this, _, window, cx| {
                            this.start_plugins_agent(agent.clone(), window, cx);
                        }))
                        .into_any_element()
                })
                .collect()
        };
        v_flex()
            .size_full()
            .gap_3()
            .items_center()
            .justify_center()
            .rounded(theme.radius_lg)
            .border_1()
            .border_color(theme.border)
            .bg(theme.background)
            .child(icon("bot").large().text_color(theme.muted_foreground))
            .child(
                div()
                    .max_w(px(380.))
                    .text_sm()
                    .text_center()
                    .text_color(theme.muted_foreground)
                    .child(tr!("plugins-agent-help")),
            )
            .child(v_flex().gap_2().items_center().children(offers))
            .into_any_element()
    }

    /// Starts a chat with `agent` in the scripts' folder. What it is there
    /// for is in the instructions files written beside the scripts — the
    /// ones Claude Code, Codex and Gemini each read of themselves.
    fn start_plugins_agent(
        &mut self,
        agent: crate::acp::Agent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(root) = crate::ui::scripts::root() else {
            return;
        };
        // The folder is made with the examples the first time it is read;
        // an agent started before that would start nowhere. Off the thread,
        // and the chat after.
        let builtins = crate::ui::scripts::builtin_root().unwrap_or_else(|| root.clone());
        cx.spawn_in(window, async move |this, cx| {
            let folder = root.clone();
            cx.background_executor()
                .spawn(async move {
                    super::scripts::prepare(&folder, &builtins);
                    if let Err(error) = crate::scripts::write_instructions(&folder, &builtins) {
                        log::warn!("preparing the scripts' folder: {error}");
                    }
                })
                .await;
            let _ = this.update_in(cx, |this, window, cx| {
                this.open_plugins_chat(&root, agent, window, cx)
            });
        })
        .detach();
    }

    /// The chat of the Plugins screen, opened in the prepared folder `root`.
    fn open_plugins_chat(
        &mut self,
        root: &Path,
        agent: crate::acp::Agent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.write_scripts_status(cx);
        let placement = super::settings::Settings::global(cx).terminal.placement;
        let before = self.chats.len();
        self.open_chat(root, agent, placement, None, window, cx);
        if self.chats.len() == before {
            return;
        }
        let Some(open) = self.chats.last() else {
            return;
        };
        let (id, view) = (open.id(), open.view.clone());
        self.scripts.screen.agent = Some(id);
        super::dialogs::focus_field(&view, window, cx);
        cx.notify();
    }
}
