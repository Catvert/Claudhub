//! The Plugins screen: where the focus view's scripts are made — listed,
//! previewed for real on a worktree, and written by an agent of their own.
//!
//! An entry at the foot of the home screen's sidebar opens it in the place
//! of the boards. Three columns: the scripts; the one selected, drawn on a
//! worktree one picks, its last error kept under it rather than said once
//! in a bubble; and the agent that edits them — a terminal in the scripts'
//! folder, told what it is there for (`scripts::editing_prompt`). The agent
//! learns whether a save worked from the status file Claudhub rewrites
//! (`scripts::STATUS`): the preview is what loads the selected script, so
//! what it reports is what the user sees.
//!
//! The agent's terminal is not brought back at the next launch: its prompt
//! names a selection that will have changed.

use std::path::{Path, PathBuf};

use gpui_kit::component::{
    button::{Button, ButtonVariants as _},
    h_flex,
    menu::{DropdownMenu as _, PopupMenuItem},
    v_flex, ActiveTheme, Disableable as _, Selectable as _, Sizable as _,
};
use gpui_kit::{div, prelude::*, px, AnyElement, Context, Focusable as _, SharedString, Window};

use crate::scripts::Kind;
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
        let preview = self.plugins_preview(live, preview_on.as_deref(), window, cx);
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
                let id = script.id.clone();
                let kind = match script.kind {
                    Kind::Tab => tr!("settings-script-tab"),
                    Kind::Home => tr!("settings-script-home"),
                };
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
                        this.write_scripts_status(cx);
                        cx.notify();
                    }))
                    .child(
                        div()
                            .truncate()
                            .text_sm()
                            .child(SharedString::from(script.title.clone())),
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
                            .child(kind),
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
                let Some(root) = &root else {
                    return;
                };
                if let Err(error) = std::fs::create_dir_all(root) {
                    log::warn!("scripts folder: {error}");
                }
                cx.open_with_system(root);
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
                    .children(empty),
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
        let open_entry = selected.as_ref().and_then(|id| {
            let script = self.scripts.script(id)?;
            let entry = script.dir.join(&script.entry);
            Some(
                Button::new("plugins-open-entry")
                    .ghost()
                    .xsmall()
                    .icon(icon("external-link"))
                    .tooltip(tr!("plugins-open-entry"))
                    .on_click(move |_, _, cx| cx.open_with_system(&entry)),
            )
        });
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
                    .children(open_entry)
                    .child(picker),
            )
            .child(div().flex_1().min_h_0().w_full().child(body))
            .into_any_element()
    }

    /// The agent that edits the scripts: its terminal, or what starts it.
    fn plugins_agent(&mut self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme().clone();
        let terminal = self
            .scripts
            .screen
            .agent
            .and_then(|id| self.terminal(id))
            .filter(|terminal| !terminal.exited)
            .map(|terminal| terminal.view.clone());
        let Some(view) = terminal else {
            return v_flex()
                .size_full()
                .gap_3()
                .items_center()
                .justify_center()
                .rounded(theme.radius_lg)
                .border_1()
                .border_color(theme.border)
                .bg(theme.background)
                .child(
                    div()
                        .max_w(px(360.))
                        .text_sm()
                        .text_center()
                        .text_color(theme.muted_foreground)
                        .child(tr!("plugins-agent-help")),
                )
                .child(
                    Button::new("plugins-agent-start")
                        .primary()
                        .small()
                        .icon(icon("bot"))
                        .label(tr!("plugins-agent-start"))
                        .on_click(cx.listener(|this, _, window, cx| {
                            this.start_plugins_agent(window, cx);
                        })),
                )
                .into_any_element();
        };
        let focused = view.focus_handle(cx).contains_focused(window, cx);
        v_flex()
            .size_full()
            .rounded(theme.radius_lg)
            .overflow_hidden()
            .bg(theme.background)
            .border_1()
            .border_color(if focused { theme.ring } else { theme.border })
            // Cached, as on the boards: see `render_tile`.
            .child(
                v_flex()
                    .flex_1()
                    .min_h_0()
                    .child(view.cached(gpui_kit::StyleRefinement::default().size_full())),
            )
            .into_any_element()
    }

    /// Starts the editing agent in the scripts' folder, told what it is
    /// there for. The configured agent profile runs it; a Claude is let write
    /// in the folder without asking at each file.
    fn start_plugins_agent(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(root) = crate::ui::scripts::root() else {
            return;
        };
        let Some(profile) = super::settings::Settings::global(cx)
            .terminal
            .default_profile()
            .cloned()
        else {
            self.announce_error(tr!("plugins-agent-none"), cx);
            return;
        };
        // The folder is made with the examples the first time it is read;
        // an agent started before that would start nowhere.
        if let Err(error) = crate::scripts::seed(&root, super::scripts::EXAMPLES) {
            log::warn!("writing the example scripts: {error}");
        }
        let prompt = crate::scripts::editing_prompt(&root, self.scripts.screen.selected.as_deref());
        let mut launch = super::terminal_view::Launch::agent(&profile);
        launch.label = tr!("plugins-agent");
        if let Some((program, args)) = launch.command.as_mut() {
            if super::revive::is_claude(program) {
                args.extend([
                    "--permission-mode".to_string(),
                    "acceptEdits".to_string(),
                    "--append-system-prompt".to_string(),
                    prompt,
                ]);
            }
        }
        let before = self.terminals.len();
        self.open_terminal(&root, launch, window, cx);
        if self.terminals.len() == before {
            return;
        }
        let Some(opened) = self.terminals.last_mut() else {
            return;
        };
        opened.relaunch = None;
        let view = opened.view.clone();
        self.scripts.screen.agent = Some(view.entity_id().as_u64());
        super::dialogs::focus_field(&view, window, cx);
        cx.notify();
    }
}
