//! The run widget, as an IDE's: the configuration on show — the
//! environment or a recipe — with whether it runs, the button that starts
//! it or starts it again, and the one that stops it.
//!
//! It merges the two buttons a checkout had: « Run » for the `justfile`,
//! and the power switch of `wt up` / `wt down`. Two ways to set something
//! going, side by side, each saying its state its own way, when an IDE says
//! both in one place: what is selected, whether it runs, and ▶ or ■.
//!
//! **A recipe runs in a terminal tab** tagged with its name
//! (`OpenTerminal::run`), and that tab is what « running » is read off.
//! Stopping interrupts it, as `Ctrl+C` would, and leaves what it printed —
//! an IDE's console stays after a stop; starting again closes the tabs the
//! recipe left, running or not, so that its runs do not pile up.

use std::path::Path;

use gpui_kit::component::{
    button::{Button, ButtonVariants as _},
    h_flex,
    menu::{DropdownMenu as _, PopupMenuItem},
    ActiveTheme, Disableable as _, Sizable as _, Size,
};
use gpui_kit::{div, prelude::*, px, AnyElement, Context, SharedString, Window};

use crate::tr;
use crate::ui::app::ClaudhubApp;
use crate::ui::icons::icon;
use crate::ui::run::{self, RunConfig};

impl ClaudhubApp {
    /// A worktree's configurations — see `run::configs`.
    fn run_configs(&self, worktree: &Path) -> Vec<RunConfig> {
        let recipes: Vec<String> = self
            .just_recipes(worktree)
            .map(|snapshot| {
                snapshot
                    .recipes
                    .iter()
                    .map(|recipe| recipe.name.clone())
                    .collect()
            })
            .unwrap_or_default();
        run::configs(self.env_controls(worktree).is_some(), &recipes)
    }

    /// Whether the project's environment can be started and stopped from
    /// here, as `(up, down)` — `None` where `wt` cannot say whether it is up.
    fn env_controls(&self, worktree: &Path) -> Option<(bool, bool)> {
        self.wt_state(worktree)?.up?;
        let project = self.wt_project(&self.main_of(worktree)?)?;
        (project.has_up || project.has_down).then_some((project.has_up, project.has_down))
    }

    /// Whether a configuration runs now.
    fn runs(&self, worktree: &Path, config: &RunConfig, cx: &Context<Self>) -> bool {
        match config {
            RunConfig::Env => self
                .wt_state(worktree)
                .and_then(|state| state.up)
                .unwrap_or(false),
            RunConfig::Recipe(name) => self
                .recipe_tabs(worktree, name)
                .any(|terminal| !terminal.view.read(cx).has_exited()),
        }
    }

    /// The tabs a recipe was run in, still open.
    fn recipe_tabs<'a>(
        &'a self,
        worktree: &'a Path,
        name: &'a str,
    ) -> impl Iterator<Item = &'a super::terminal_view::OpenTerminal> + 'a {
        self.terminals.iter().filter(move |terminal| {
            terminal.worktree == worktree && terminal.run.as_deref() == Some(name)
        })
    }

    /// Starts a configuration — again, for a recipe that runs: its tabs are
    /// closed first, what they held going with them.
    fn start_config(
        &mut self,
        worktree: &Path,
        config: &RunConfig,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match config {
            RunConfig::Env => {
                if let Some(main) = self.main_of(worktree) {
                    self.wt_up(main, worktree, window, cx);
                }
            }
            RunConfig::Recipe(name) => {
                let old: Vec<_> = self
                    .recipe_tabs(worktree, name)
                    .map(|terminal| terminal.view.entity_id())
                    .collect();
                for view in old {
                    self.close_terminal(view, window, cx);
                }
                self.run_just(worktree.to_path_buf(), name.clone(), window, cx);
            }
        }
    }

    /// Stops a configuration: `wt down`, or the recipe interrupted in the
    /// tab that keeps its output.
    fn stop_config(
        &mut self,
        worktree: &Path,
        config: &RunConfig,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match config {
            RunConfig::Env => {
                if let Some(main) = self.main_of(worktree) {
                    self.wt_down(main, worktree, window, cx);
                }
            }
            RunConfig::Recipe(name) => {
                let views: Vec<_> = self
                    .recipe_tabs(worktree, name)
                    .map(|terminal| terminal.view.clone())
                    .collect();
                for view in views {
                    view.update(cx, |view, _| view.interrupt());
                }
            }
        }
        cx.notify();
    }

    /// What a configuration is called in the widget and its menu.
    fn config_name(config: &RunConfig) -> SharedString {
        match config {
            RunConfig::Env => tr!("run-env"),
            RunConfig::Recipe(name) => SharedString::from(name.clone()),
        }
    }

    /// The widget: the configuration on show and its state, then ▶ — ↻
    /// while it runs a recipe — and ■ while it runs. `None` for a worktree
    /// with nothing to run.
    pub(super) fn render_run(
        &self,
        worktree: &Path,
        size: Size,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let configs = self.run_configs(worktree);
        let default = self
            .just_recipes(worktree)
            .and_then(|snapshot| snapshot.default.clone());
        let shown = run::shown(self.run_choice.get(worktree), &configs, default.as_deref())?;
        let theme = cx.theme().clone();
        let running = self.runs(worktree, &shown, cx);
        let others = configs
            .iter()
            .filter(|config| **config != shown && self.runs(worktree, config, cx))
            .count();
        // `wt up` or `down` under way: neither of the two states, and the
        // only thing to say for the half-minute it takes.
        let switching = shown == RunConfig::Env && self.flight.wt_target() == Some(worktree);
        let (can_up, can_down) = self.env_controls(worktree).unwrap_or((false, false));

        // The configuration, with its state at a glance: a dot, green while
        // it runs, hollow while it does not.
        let dot = div().size(px(7.)).rounded_full().map(|el| {
            if running {
                el.bg(theme.success)
            } else {
                el.border_1()
                    .border_color(theme.muted_foreground.opacity(0.8))
            }
        });
        let entity = cx.entity();
        let menu_worktree = worktree.to_path_buf();
        let menu_items: Vec<(RunConfig, bool)> = configs
            .iter()
            .map(|config| (config.clone(), self.runs(worktree, config, cx)))
            .collect();
        let chosen = shown.clone();
        let selector = Button::new("run-config")
            .ghost()
            .with_size(size)
            .tooltip(tr!("run-config"))
            .child(
                h_flex()
                    .gap_1p5()
                    .items_center()
                    .when(size == Size::XSmall, |el| el.text_xs())
                    .when(size != Size::XSmall, |el| el.text_sm())
                    .child(dot)
                    .child(
                        icon(match shown {
                            RunConfig::Env => "zap",
                            RunConfig::Recipe(_) => "terminal",
                        })
                        .xsmall()
                        .text_color(theme.muted_foreground),
                    )
                    .child(
                        div()
                            .max_w(px(160.))
                            .truncate()
                            .child(Self::config_name(&shown)),
                    )
                    .when(others > 0, |el| {
                        el.child(super::theme::chip(
                            SharedString::from(format!("+{others}")),
                            theme.success,
                        ))
                    })
                    .child(
                        icon("chevron-down")
                            .xsmall()
                            .text_color(theme.muted_foreground),
                    ),
            )
            .dropdown_menu(move |menu, _window, _cx| {
                menu_items.iter().fold(menu, |menu, (config, running)| {
                    let (entity, worktree, pick) =
                        (entity.clone(), menu_worktree.clone(), config.clone());
                    let name = Self::config_name(config);
                    // What runs says so in the menu too, where the
                    // choice is made.
                    let label = if *running {
                        SharedString::from(format!("{name} — {}", tr!("run-running")))
                    } else {
                        name
                    };
                    menu.item(
                        PopupMenuItem::new(label)
                            .icon(icon(match config {
                                RunConfig::Env => "zap",
                                RunConfig::Recipe(_) => "terminal",
                            }))
                            .checked(*config == chosen)
                            .on_click(move |_, _, cx| {
                                let (worktree, pick) = (worktree.clone(), pick.clone());
                                entity.update(cx, |this, cx| {
                                    this.run_choice.insert(worktree, pick);
                                    cx.notify();
                                });
                            }),
                    )
                })
            });

        let (start_worktree, start_config) = (worktree.to_path_buf(), shown.clone());
        let can_start = match shown {
            RunConfig::Env => !running && can_up,
            RunConfig::Recipe(_) => true,
        };
        let start = if switching {
            Button::new("run-start")
                .ghost()
                .with_size(size)
                .disabled(true)
                .icon(icon("loader-circle").text_color(theme.warning))
        } else {
            let again = running && matches!(shown, RunConfig::Recipe(_));
            Button::new("run-start")
                .ghost()
                .with_size(size)
                .disabled(!can_start)
                .icon(
                    icon(if again { "refresh-cw" } else { "play" }).text_color(if can_start {
                        theme.success
                    } else {
                        theme.muted_foreground
                    }),
                )
                .tooltip(if again {
                    tr!("run-again", { name: Self::config_name(&shown) })
                } else {
                    tr!("run-start", { name: Self::config_name(&shown) })
                })
                .on_click(cx.listener(move |this, _, window, cx| {
                    this.start_config(&start_worktree, &start_config, window, cx);
                }))
        };
        let can_stop = running
            && !switching
            && match shown {
                RunConfig::Env => can_down,
                RunConfig::Recipe(_) => true,
            };
        let (stop_worktree, stop_config) = (worktree.to_path_buf(), shown.clone());
        let stop = can_stop.then(|| {
            Button::new("run-stop")
                .ghost()
                .with_size(size)
                .icon(icon("circle-stop").text_color(theme.danger))
                .tooltip(tr!("run-stop", { name: Self::config_name(&shown) }))
                .on_click(cx.listener(move |this, _, window, cx| {
                    this.stop_config(&stop_worktree, &stop_config, window, cx);
                }))
        });
        Some(
            h_flex()
                .flex_none()
                .items_center()
                .child(selector)
                .child(start)
                .children(stop)
                .into_any_element(),
        )
    }
}
