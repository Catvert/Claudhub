//! The settings screen.
//!
//! The form comes from gpui-component: pages, groups, search and a reset button
//! are provided, and each field is declared by a read/write pair. Those closures
//! only receive an `App` — which is what forces the settings to live in a global
//! rather than in `ClaudhubApp`.
//!
//! There is no "Apply" button: every change takes effect as it is typed and the
//! file write follows, deferred. A form asking you to confirm before seeing the
//! result makes choosing a font or a size impossible except blind.
//!
//! **A screen and not a dialog.** It was a modal window — what one reaches for
//! when there is nowhere to put a form. It covered what was being adjusted, it
//! could not be left open beside the effect it produced, and the two things one
//! comes here for, trying a theme and reading why something failed, are exactly
//! the two that want the rest of the window still in sight. The bar was already
//! there and the dock already knew how to carry a panel; what the move costs is
//! that the render closure now runs **on every frame** instead of once at
//! opening — hence `Environment`, and the cached log records.

use gpui::{div, prelude::*, px, Anchor, App, Context, Entity, SharedString, Subscription, Window};
use gpui_component::button::{Button, ButtonGroup, ButtonVariants};
use gpui_component::input::{Input, InputEvent, InputState};
use gpui_component::menu::{DropdownMenu, PopupMenuItem};
use gpui_component::setting::{
    NumberFieldOptions, SelectIndex, SettingField, SettingGroup, SettingItem, SettingPage,
};
use gpui_component::{h_flex, v_flex, ActiveTheme, Disableable, Selectable, Sizable, StyledExt};

use crate::tr;
use crate::ui::app::ClaudhubApp;
use crate::ui::icons::icon;
use crate::ui::settings::{
    self, LanguageChoice, Settings, ThemeMode, DEFAULT_MONO_FONT, DEFAULT_UI_FONT,
};

/// Which page the settings screen shows.
///
/// An enum and not an index: the page order is decided in `settings_pages`, and
/// that is the only place that has to know it.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(super) enum Page {
    /// Wherever you were — what is wanted when the settings are simply opened.
    #[default]
    First,
    Databases,
}

/// What the screen asks the system for, and asks it **once**.
///
/// The dialog this screen replaces paid for it at opening; a screen has no
/// opening, and its declaration is rebuilt on every frame. Enumerating the
/// installed fonts and stat-ing every line of `/etc/shells` at that rate is
/// filesystem work in the middle of a frame.
pub(super) struct Environment {
    ui_fonts: Vec<(SharedString, SharedString)>,
    mono_fonts: Vec<(SharedString, SharedString)>,
    shells: Vec<(SharedString, SharedString)>,
}

impl Environment {
    fn read(cx: &App) -> Self {
        let installed = cx.text_system().all_font_names();
        Self {
            ui_fonts: choices(settings::font_choices(&installed, false, DEFAULT_UI_FONT)),
            mono_fonts: choices(settings::font_choices(&installed, true, DEFAULT_MONO_FONT)),
            shells: shell_choices(),
        }
    }
}

impl ClaudhubApp {
    pub(super) fn open_settings(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.open_settings_at(Page::First, window, cx);
    }

    /// The settings screen, shown on a given page.
    ///
    /// "Add a connection" comes from the "Databases" panel; answering it with a
    /// form opened on appearance leaves you hunting through a seven-entry
    /// sidebar for what you had just asked for.
    ///
    /// The page cannot simply be written into the form on every frame:
    /// `default_selected_index` is read when the form's state is **created**,
    /// and that state lives as long as its id. The id therefore carries a
    /// counter that this gesture bumps — a named page is a request, honoured
    /// every time, even twice in a row having wandered off in between, where
    /// `First` means "wherever you were" and leaves the form alone.
    pub(super) fn open_settings_at(
        &mut self,
        page: Page,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !matches!(page, Page::First) {
            self.settings_page = page;
            self.settings_epoch += 1;
        }
        self.enter_workspace(crate::ui::workspace::Workspace::Settings, window, cx);
        cx.notify();
    }

    pub(super) fn render_settings_panel(
        &mut self,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let environment = self.settings_environment(cx);
        // The registry is populated asynchronously at startup and re-read here
        // rather than cached: it is watched, and a theme file dropped in the
        // folder while Claudhub runs has to show up in the list.
        let light_themes = theme_choices(gpui_component::ThemeMode::Light, cx);
        let dark_themes = theme_choices(gpui_component::ThemeMode::Dark, cx);
        let logs = LogView {
            records: self.log_records(),
            level: self.logs_level,
            app: cx.entity(),
        };

        // The pages are assembled as a list rather than chained: that is what
        // makes it possible to record a page's place **at the moment it is
        // added**. A hard-coded index would have named the neighbour as soon as
        // one page was inserted before it, and nothing would say so.
        let mut pages = vec![
            appearance_page(
                environment.ui_fonts.clone(),
                environment.mono_fonts.clone(),
                light_themes,
                dark_themes,
            ),
            terminal_page(environment.shells.clone(), environment.mono_fonts.clone()),
            review_page(),
            keyboard_page(),
            files_page(),
            lsp_page(),
        ];
        let databases_ix = pages.len();
        pages.push(databases_page());
        pages.push(plugins_page());
        pages.push(logs_page(logs));
        let selected = match self.settings_page {
            Page::First => None,
            Page::Databases => Some(databases_ix),
        };

        div().size_full().child(
            gpui_component::setting::Settings::new(SharedString::from(format!(
                "claudhub-settings-{}",
                self.settings_epoch
            )))
            .sidebar_width(px(190.))
            .pages(pages)
            .map(|form| match selected {
                Some(page_ix) => form.default_selected_index(SelectIndex {
                    page_ix,
                    group_ix: None,
                }),
                None => form,
            }),
        )
    }

    /// What the system told us, read once and kept.
    ///
    /// Emptied when the remote server answers: its `/etc/shells` is the one the
    /// form must offer, and ours names nothing over there.
    fn settings_environment(&mut self, cx: &App) -> std::rc::Rc<Environment> {
        self.settings_env
            .get_or_insert_with(|| std::rc::Rc::new(Environment::read(cx)))
            .clone()
    }

    pub(super) fn forget_settings_environment(&mut self) {
        self.settings_env = None;
    }

    /// The log records, copied only when there are new ones.
    ///
    /// `logging::records` copies the ring — two thousand entries — and this page
    /// renders on every frame. The counter is what says the copy is out of date;
    /// the buffer's own length would stop moving as soon as the ring is full.
    ///
    /// Nothing notifies the view when a record is written — a worker thread
    /// logs, it does not touch gpui — so the page follows the frames the rest of
    /// the application causes. The background sweep alone brings one every two
    /// seconds, which is what makes a log written elsewhere appear on its own.
    fn log_records(&mut self) -> std::rc::Rc<Vec<crate::logging::Entry>> {
        let written = crate::logging::written();
        if self.logs_seen != written {
            self.logs_seen = written;
            self.logs = std::rc::Rc::new(crate::logging::records());
        }
        self.logs.clone()
    }
}

fn choices(names: Vec<String>) -> Vec<(SharedString, SharedString)> {
    names
        .into_iter()
        .map(|name| (SharedString::from(name.clone()), SharedString::from(name)))
        .collect()
}

/// Shells the system declares. The menu only offers them: the field stays free,
/// and empty always means "the login shell".
fn shell_choices() -> Vec<(SharedString, SharedString)> {
    choices(settings::available_shells())
}

/// The state of the "shell" field, kept from one render to the next.
///
/// The subscription lives inside it: a dropped `Subscription` is cut, and the
/// field would stop writing into the settings from the next frame on.
struct ShellField {
    input: Entity<InputState>,
    _subscription: Subscription,
}

/// The shell is typed freely, and the menu only offers.
///
/// A closed list would do if `/etc/shells` told the truth; it ignores everything
/// not installed by the system — a shell compiled by hand, a `nix run`, a `tmux
/// new-session` — and we do not want a setting you leave by editing a JSON file.
fn shell_item(shells: Vec<(SharedString, SharedString)>) -> SettingItem {
    SettingItem::new(
        tr!("settings-shell"),
        SettingField::render(move |_, window, cx| {
            let shells = shells.clone();
            let state = window.use_keyed_state("claudhub-shell", cx, |window, cx| {
                let input = cx.new(|cx| {
                    InputState::new(window, cx)
                        .placeholder(tr!("settings-shell-default"))
                        .default_value(Settings::global(cx).terminal.shell.clone())
                });
                let subscription = cx.subscribe(
                    &input,
                    |_: &mut ShellField, input, event: &InputEvent, cx| {
                        if !matches!(event, InputEvent::Change) {
                            return;
                        }
                        let value = input.read(cx).value().to_string();
                        Settings::update_global(cx, |s| s.terminal.shell = value);
                    },
                );
                ShellField {
                    input,
                    _subscription: subscription,
                }
            });
            let input = state.read(cx).input.clone();
            let for_menu = input.clone();
            h_flex()
                .w(px(300.))
                .gap_1()
                .child(div().flex_1().child(Input::new(&input).small()))
                .when(!shells.is_empty(), |el| {
                    el.child(
                        Button::new("detected-shells")
                            .outline()
                            .small()
                            .icon(icon("chevron-down"))
                            .tooltip(tr!("settings-shell-detected"))
                            .dropdown_menu_with_anchor(Anchor::TopRight, move |menu, _, _| {
                                shells.iter().fold(menu, |menu, (value, label)| {
                                    let (input, value) = (for_menu.clone(), value.clone());
                                    menu.item(PopupMenuItem::new(label.clone()).on_click(
                                        move |_, window, cx| {
                                            input.update(cx, |state, cx| {
                                                state.set_value(value.clone(), window, cx)
                                            });
                                        },
                                    ))
                                })
                            }),
                    )
                })
        }),
    )
    .description(tr!("settings-shell-help"))
}

/// The state of one row of the profiles table, kept from one render to the next.
///
/// The three subscriptions live inside it: dropped, they would be cut and the
/// fields would stop writing into the settings on the next frame.
struct AgentField {
    name: Entity<InputState>,
    command: Entity<InputState>,
    env: Entity<InputState>,
    _subscriptions: Vec<Subscription>,
}

/// The agent profiles table.
///
/// A bespoke field because there is nothing like it in gpui-component's form:
/// these are rows added and removed, with three inputs each.
///
/// **The state key carries the number of profiles** (`claudhub-agent-{n}-{i}`).
/// `use_keyed_state` keeps one state per key: without the count, deleting the
/// first profile would leave row 0's fields filled with the old one, and we
/// would write into the settings what we thought we had deleted. Renaming a
/// profile, on the other hand, does not change the count — so the fields keep
/// their cursor while typing.
fn agents_item() -> SettingItem {
    SettingItem::new(
        tr!("settings-agents"),
        SettingField::render(move |_, window, cx| {
            let profiles = Settings::global(cx).terminal.agents.clone();
            let count = profiles.len();
            let rows: Vec<_> = profiles
                .iter()
                .enumerate()
                .map(|(index, profile)| agent_row(index, count, profile, window, cx))
                .collect();
            v_flex().w(px(460.)).gap_1().children(rows).child(
                h_flex().child(
                    Button::new("add-agent")
                        .outline()
                        .small()
                        .icon(icon("plus"))
                        .label(tr!("settings-agent-add"))
                        .on_click(|_, _window, cx| {
                            Settings::update_global(cx, |s| {
                                s.terminal.agents.push(settings::AgentProfile::default())
                            });
                        }),
                ),
            )
        }),
    )
    .description(tr!("settings-agents-help"))
}

fn agent_row(
    index: usize,
    count: usize,
    profile: &settings::AgentProfile,
    window: &mut Window,
    cx: &mut App,
) -> impl IntoElement {
    let key = format!("claudhub-agent-{count}-{index}");
    let (name, command, env) = (
        profile.name.clone(),
        profile.command_line(),
        profile.env_line(),
    );
    let state = window.use_keyed_state(SharedString::from(key), cx, move |window, cx| {
        let name_input = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder(tr!("settings-agent-name"))
                .default_value(name)
        });
        let command_input = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder(tr!("settings-agent-command"))
                .default_value(command)
        });
        let env_input = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder(tr!("settings-agent-env"))
                .default_value(env)
        });
        let subscriptions = vec![
            cx.subscribe(
                &name_input,
                move |_: &mut AgentField, input, event: &InputEvent, cx| {
                    if !matches!(event, InputEvent::Change) {
                        return;
                    }
                    let value = input.read(cx).value().to_string();
                    edit_agent(index, cx, |profile| profile.name = value);
                },
            ),
            cx.subscribe(
                &command_input,
                move |_: &mut AgentField, input, event: &InputEvent, cx| {
                    if !matches!(event, InputEvent::Change) {
                        return;
                    }
                    let value = input.read(cx).value().to_string();
                    edit_agent(index, cx, |profile| {
                        // The line is split honouring quotes: a path containing
                        // a space must not become two arguments.
                        let mut parts = settings::split_command(&value).into_iter();
                        profile.command = parts.next().unwrap_or_default();
                        profile.args = parts.collect();
                    });
                },
            ),
            cx.subscribe(
                &env_input,
                move |_: &mut AgentField, input, event: &InputEvent, cx| {
                    if !matches!(event, InputEvent::Change) {
                        return;
                    }
                    let value = input.read(cx).value().to_string();
                    edit_agent(index, cx, |profile| profile.set_env_line(&value));
                },
            ),
        ];
        AgentField {
            name: name_input,
            command: command_input,
            env: env_input,
            _subscriptions: subscriptions,
        }
    });
    let field = state.read(cx);
    let (name, command, env) = (field.name.clone(), field.command.clone(), field.env.clone());
    h_flex()
        .gap_1()
        .items_center()
        .child(div().w(px(90.)).child(Input::new(&name).small()))
        .child(div().flex_1().child(Input::new(&command).small()))
        .child(div().w(px(130.)).child(Input::new(&env).small()))
        .child(
            Button::new(("remove-agent", index))
                .ghost()
                .small()
                .icon(icon("trash-2"))
                .tooltip(tr!("settings-agent-remove"))
                .on_click(move |_, _window, cx| {
                    Settings::update_global(cx, |s| {
                        if index < s.terminal.agents.len() {
                            s.terminal.agents.remove(index);
                        }
                    });
                }),
        )
}

/// The profile launched when nobody says which.
///
/// The list of choices is read again on every render of the form: it changes
/// while the table just above is being edited.
fn default_agent_item() -> SettingItem {
    SettingItem::new(
        tr!("settings-default-agent"),
        SettingField::render(|_, _window, cx| {
            let profiles = Settings::global(cx).terminal.agents.clone();
            let current = Settings::global(cx)
                .terminal
                .default_profile()
                .map(|profile| profile.label().to_string())
                .unwrap_or_default();
            Button::new("default-agent")
                .outline()
                .small()
                .label(SharedString::from(current))
                .dropdown_menu_with_anchor(Anchor::TopRight, move |menu, _, _| {
                    profiles.iter().fold(menu, |menu, profile| {
                        let label = SharedString::from(profile.label().to_string());
                        let chosen = label.clone();
                        menu.item(PopupMenuItem::new(label).on_click(move |_, _window, cx| {
                            let chosen = chosen.to_string();
                            Settings::update_global(cx, |s| s.terminal.default_agent = chosen);
                        }))
                    })
                })
        }),
    )
    .description(tr!("settings-default-agent-help"))
}

/// Changes a profile in place, if the index still exists.
///
/// The index may be one frame stale: a subscription set up for row 2 outlives
/// row 2's disappearance, and writing out of bounds would panic in the middle of
/// a render.
fn edit_agent(index: usize, cx: &mut App, edit: impl FnOnce(&mut settings::AgentProfile)) {
    Settings::update_global(cx, |s| {
        if let Some(profile) = s.terminal.agents.get_mut(index) {
            edit(profile);
        }
    });
}

/// The registry's palettes for a given appearance.
fn theme_choices(mode: gpui_component::ThemeMode, cx: &App) -> Vec<(SharedString, SharedString)> {
    gpui_component::ThemeRegistry::global(cx)
        .sorted_themes()
        .into_iter()
        .filter(|theme| theme.mode == mode)
        .map(|theme| (theme.name.clone(), theme.name.clone()))
        .collect()
}

fn appearance_page(
    ui_fonts: Vec<(SharedString, SharedString)>,
    mono_fonts: Vec<(SharedString, SharedString)>,
    light_themes: Vec<(SharedString, SharedString)>,
    dark_themes: Vec<(SharedString, SharedString)>,
) -> SettingPage {
    let themes = vec![
        (SharedString::from("dark"), tr!("settings-theme-dark")),
        (SharedString::from("light"), tr!("settings-theme-light")),
        (SharedString::from("system"), tr!("settings-theme-system")),
    ];
    let languages = vec![
        (
            SharedString::from("system"),
            tr!("settings-language-system"),
        ),
        (SharedString::from("fr"), SharedString::from("Français")),
        (SharedString::from("en"), SharedString::from("English")),
    ];

    SettingPage::new(tr!("settings-page-appearance"))
        .default_open(true)
        .group(
            SettingGroup::new()
                .title(tr!("settings-group-theme"))
                .item(
                    SettingItem::new(
                        tr!("settings-theme"),
                        SettingField::dropdown(
                            themes,
                            |cx: &App| Settings::global(cx).theme.as_key().into(),
                            |value: SharedString, cx: &mut App| {
                                Settings::update_global(cx, |s| {
                                    s.theme = ThemeMode::from_key(&value)
                                });
                            },
                        )
                        .default_value(SharedString::from("dark")),
                    )
                    .description(tr!("settings-theme-help")),
                )
                .item(
                    SettingItem::new(
                        tr!("settings-dark-theme"),
                        SettingField::dropdown(
                            dark_themes,
                            |cx: &App| Settings::global(cx).dark_theme.clone().into(),
                            |value: SharedString, cx: &mut App| {
                                Settings::update_global(cx, |s| s.dark_theme = value.to_string())
                            },
                        )
                        .default_value(SharedString::from(settings::DEFAULT_DARK_THEME)),
                    )
                    .description(tr!("settings-palette-help")),
                )
                .item(SettingItem::new(
                    tr!("settings-light-theme"),
                    SettingField::dropdown(
                        light_themes,
                        |cx: &App| Settings::global(cx).light_theme.clone().into(),
                        |value: SharedString, cx: &mut App| {
                            Settings::update_global(cx, |s| s.light_theme = value.to_string())
                        },
                    )
                    .default_value(SharedString::from(settings::DEFAULT_LIGHT_THEME)),
                ))
                .item(
                    SettingItem::new(
                        tr!("settings-language"),
                        SettingField::dropdown(
                            languages,
                            |cx: &App| Settings::global(cx).language.as_key().into(),
                            |value: SharedString, cx: &mut App| {
                                Settings::update_global(cx, |s| {
                                    s.language = LanguageChoice::from_key(&value)
                                });
                            },
                        )
                        .default_value(SharedString::from("system")),
                    )
                    .description(tr!("settings-language-help")),
                ),
        )
        .group(
            SettingGroup::new()
                .title(tr!("settings-group-fonts"))
                .item(
                    SettingItem::new(
                        tr!("settings-ui-font"),
                        SettingField::dropdown(
                            ui_fonts,
                            |cx: &App| Settings::global(cx).ui_font().to_string().into(),
                            |value: SharedString, cx: &mut App| {
                                Settings::update_global(cx, |s| {
                                    s.ui_font_family = value.to_string()
                                })
                            },
                        )
                        .default_value(SharedString::from(DEFAULT_UI_FONT)),
                    )
                    .description(tr!("settings-ui-font-help")),
                )
                .item(SettingItem::new(
                    tr!("settings-ui-font-size"),
                    SettingField::number_input(
                        size_range(),
                        |cx: &App| Settings::global(cx).font_size as f64,
                        |value: f64, cx: &mut App| {
                            Settings::update_global(cx, |s| s.font_size = clamp_size(value))
                        },
                    )
                    .default_value(14.0),
                ))
                .item(
                    SettingItem::new(
                        tr!("settings-mono-font"),
                        SettingField::dropdown(
                            mono_fonts,
                            |cx: &App| Settings::global(cx).mono_font().to_string().into(),
                            |value: SharedString, cx: &mut App| {
                                Settings::update_global(cx, |s| {
                                    s.mono_font_family = value.to_string()
                                })
                            },
                        )
                        .default_value(SharedString::from(DEFAULT_MONO_FONT)),
                    )
                    .description(tr!("settings-mono-font-help")),
                )
                .item(SettingItem::new(
                    tr!("settings-diff-font-size"),
                    SettingField::number_input(
                        size_range(),
                        |cx: &App| Settings::global(cx).diff_font_size as f64,
                        |value: f64, cx: &mut App| {
                            Settings::update_global(cx, |s| s.diff_font_size = clamp_size(value))
                        },
                    )
                    .default_value(13.0),
                )),
        )
        .group(
            SettingGroup::new()
                .title(tr!("settings-group-window"))
                .item(
                    SettingItem::new(
                        tr!("settings-maximized"),
                        SettingField::switch(
                            |cx: &App| Settings::global(cx).start_maximized,
                            |value: bool, cx: &mut App| {
                                Settings::update_global(cx, |s| s.start_maximized = value)
                            },
                        )
                        .default_value(false),
                    )
                    .description(tr!("settings-maximized-help")),
                ),
        )
}

fn terminal_page(
    shells: Vec<(SharedString, SharedString)>,
    mono_fonts: Vec<(SharedString, SharedString)>,
) -> SettingPage {
    // The terminal's font reuses the fixed-pitch list, preceded by the "same as
    // the diffs" entry: the terminal is allowed not to choose, so that setting
    // the fixed pitch once is enough for the common case.
    let mut fonts = vec![(SharedString::default(), tr!("settings-font-inherit"))];
    fonts.extend(mono_fonts);

    SettingPage::new(tr!("settings-page-terminal"))
        .group(
            SettingGroup::new()
                .title(tr!("settings-group-shell"))
                .item(shell_item(shells))
                .item(agents_item())
                .item(default_agent_item()),
        )
        .group(
            SettingGroup::new()
                .title(tr!("settings-group-terminal-display"))
                .item(SettingItem::new(
                    tr!("settings-terminal-font"),
                    SettingField::dropdown(
                        fonts,
                        |cx: &App| Settings::global(cx).terminal.font_family.clone().into(),
                        |value: SharedString, cx: &mut App| {
                            Settings::update_global(cx, |s| {
                                s.terminal.font_family = value.to_string()
                            })
                        },
                    )
                    .default_value(SharedString::default()),
                ))
                .item(SettingItem::new(
                    tr!("settings-terminal-font-size"),
                    SettingField::number_input(
                        size_range(),
                        |cx: &App| Settings::global(cx).terminal.font_size as f64,
                        |value: f64, cx: &mut App| {
                            Settings::update_global(cx, |s| {
                                s.terminal.font_size = clamp_size(value)
                            })
                        },
                    )
                    .default_value(13.0),
                ))
                .item(
                    SettingItem::new(
                        tr!("settings-scrollback"),
                        SettingField::number_input(
                            NumberFieldOptions {
                                min: 0.,
                                max: 200_000.,
                                step: 1_000.,
                            },
                            |cx: &App| Settings::global(cx).terminal.scrollback as f64,
                            |value: f64, cx: &mut App| {
                                Settings::update_global(cx, |s| {
                                    s.terminal.scrollback = value.clamp(0., 200_000.) as usize
                                })
                            },
                        )
                        .default_value(10_000.0),
                    )
                    .description(tr!("settings-scrollback-help")),
                ),
        )
}

/// The keyboard.
///
/// A short page, and that is accepted: vim mode changes the meaning of half the
/// keys, and it is the first place one goes looking for it. The reminder about
/// `F1` is there because help you cannot find is not help.
fn keyboard_page() -> SettingPage {
    SettingPage::new(tr!("settings-page-keyboard"))
        .group(
            SettingGroup::new()
                .title(tr!("settings-group-vim"))
                .item(
                    SettingItem::new(
                        tr!("settings-vim-mode"),
                        SettingField::switch(
                            |cx: &App| Settings::global(cx).vim_mode,
                            |value: bool, cx: &mut App| {
                                Settings::update_global(cx, |s| s.vim_mode = value)
                            },
                        )
                        .default_value(false),
                    )
                    .description(tr!("settings-vim-mode-help")),
                )
                .item(
                    SettingItem::new(
                        tr!("settings-vim-clipboard"),
                        SettingField::switch(
                            |cx: &App| Settings::global(cx).vim_clipboard,
                            |value: bool, cx: &mut App| {
                                Settings::update_global(cx, |s| s.vim_clipboard = value)
                            },
                        )
                        .default_value(false),
                    )
                    .description(tr!("settings-vim-clipboard-help")),
                ),
        )
        .group(
            SettingGroup::new()
                .title(tr!("settings-group-shortcuts"))
                .item(shortcuts_item()),
        )
}

/// Every binding, editable.
///
/// **A `SettingItem::render`**, like the databases and for the same reason: an
/// ordinary item cuts its field to four hundred pixels, and a row here is a
/// label, a field and two buttons across the page.
///
/// The list is `shortcuts::all()` — the table both the keymap and the help come
/// out of. A second list would have diverged on the first addition, which is
/// the whole point of that module.
fn shortcuts_item() -> SettingItem {
    SettingItem::render(move |_, window, cx| {
        let overrides = Settings::global(cx).shortcuts.clone();
        let vim = Settings::global(cx).vim_mode;
        // Which keys are claimed twice under the same predicate. Counted once
        // here and not per row: a duplicate is settled by declaration order,
        // which is never what was meant, and nothing else would say so.
        let mut claimed: std::collections::HashMap<(&str, String), usize> =
            std::collections::HashMap::new();
        for entry in crate::ui::shortcuts::all() {
            let keys = entry.effective(&overrides).trim().to_string();
            if !keys.is_empty() {
                *claimed.entry((entry.predicate, keys)).or_default() += 1;
            }
        }

        let mut rows: Vec<gpui::AnyElement> = Vec::new();
        for group in crate::ui::shortcuts::Group::ORDER {
            let family: Vec<_> = crate::ui::shortcuts::all()
                .filter(|entry| entry.group == group)
                .collect();
            if family.is_empty() {
                continue;
            }
            rows.push(
                div()
                    .pt_2()
                    .text_sm()
                    .font_semibold()
                    .text_color(cx.theme().muted_foreground)
                    .child(tr!(group.key()))
                    .into_any_element(),
            );
            for entry in family {
                let keys = entry.effective(&overrides).trim().to_string();
                let conflict = !keys.is_empty()
                    && claimed
                        .get(&(entry.predicate, keys))
                        .is_some_and(|count| *count > 1);
                rows.push(shortcut_row(entry, vim, conflict, window, cx).into_any_element());
            }
        }
        v_flex()
            .w_full()
            .gap_1()
            .child(
                div()
                    .text_sm()
                    .text_color(cx.theme().muted_foreground)
                    .child(tr!("settings-shortcuts-help")),
            )
            .children(rows)
    })
}

/// Waits for the next keystroke and records it for that binding.
///
/// **A keystroke interceptor and not a focused element.** An interceptor runs
/// *before* the keymap, and `stop_propagation` there stops the dispatch dead:
/// pressing `Ctrl+T` to record it must not also hide the terminals. Doing it
/// with a focusable capture zone would have meant a context excluded from every
/// predicate — eight strings to keep in step, one forgotten being a shortcut
/// firing while one records it.
///
/// Escape gives up, a modifier held on its own is waited out, and everything
/// else is written to the field, to the settings and to the keymap at once.
fn start_capture(
    entry: &'static crate::ui::shortcuts::Entry,
    state: &Entity<ShortcutField>,
    cx: &mut App,
) {
    if state.read(cx).capturing {
        state.update(cx, |field, _| field.capturing = false);
        return;
    }
    let already = state.read(cx)._capture.is_some();
    state.update(cx, |field, _| field.capturing = true);
    if already {
        return;
    }
    let handle = state.clone();
    let subscription = cx.intercept_keystrokes(move |event, window, cx| {
        if !handle.read(cx).capturing {
            return;
        }
        // The key belongs to the capture, whatever it is bound to elsewhere.
        cx.stop_propagation();
        let Some(keys) = crate::ui::shortcuts::stroke_syntax(&event.keystroke) else {
            return; // a modifier on its own: wait for the key it qualifies
        };
        handle.update(cx, |field, _| field.capturing = false);
        if keys == "escape" {
            return;
        }
        let input = handle.read(cx).keys.clone();
        input.update(cx, |input, cx| input.set_value(keys.clone(), window, cx));
        Settings::update_global(cx, |settings| {
            if keys == entry.keys {
                settings.shortcuts.remove(&entry.id());
            } else {
                settings.shortcuts.insert(entry.id(), keys.clone());
            }
        });
        crate::ui::shortcuts::rebind(cx);
    });
    state.update(cx, |field, _| field._capture = Some(subscription));
}

/// A binding's field, kept from one render to the next.
struct ShortcutField {
    keys: Entity<InputState>,
    /// Waiting for the key to record.
    capturing: bool,
    /// The keystroke interceptor, installed on the first capture and kept
    /// afterwards: `capturing` is what turns it on and off. Dropping a
    /// subscription from inside its own callback is not a thing to try.
    _capture: Option<Subscription>,
    _subscription: Subscription,
}

/// One binding: what it does, the keys it answers to, and the way back.
///
/// **The state's key is the binding's id**, which never moves: the list is
/// neither added to nor reordered while the window is open, so there is no
/// count to carry as the agent profiles do. Resetting writes into the field
/// itself — the state would otherwise keep the text one has just abandoned.
fn shortcut_row(
    entry: &'static crate::ui::shortcuts::Entry,
    vim: bool,
    conflict: bool,
    window: &mut Window,
    cx: &mut App,
) -> impl IntoElement {
    let id = entry.id();
    let overrides = Settings::global(cx).shortcuts.clone();
    let current = entry.effective(&overrides).to_string();
    let customised = overrides.contains_key(&id);
    let invalid = !crate::ui::shortcuts::valid_keys(&current);
    // A vim binding while the mode is off is not an error, but it is not a key
    // either: the row says so rather than offering a gesture that does nothing.
    let idle = entry.vim && !vim;

    let state = window.use_keyed_state(
        SharedString::from(format!("claudhub-shortcut-{id}")),
        cx,
        move |window, cx| {
            let keys = cx.new(|cx| {
                InputState::new(window, cx)
                    .placeholder(SharedString::from(entry.keys))
                    .default_value(current.clone())
            });
            let subscription =
                cx.subscribe(&keys, move |_: &mut ShortcutField, input, event, cx| {
                    if !matches!(event, InputEvent::Change) {
                        return;
                    }
                    let value = input.read(cx).value().trim().to_string();
                    // What does not read is left in the field and not written:
                    // the row says so, and the keymap keeps what it had.
                    if !crate::ui::shortcuts::valid_keys(&value) {
                        return;
                    }
                    Settings::update_global(cx, |settings| {
                        if value == entry.keys {
                            settings.shortcuts.remove(&entry.id());
                        } else {
                            settings.shortcuts.insert(entry.id(), value.clone());
                        }
                    });
                    // The keymap is rebuilt at once: a shortcut one has to
                    // restart to try is a shortcut one sets blind.
                    crate::ui::shortcuts::rebind(cx);
                });
            ShortcutField {
                keys,
                capturing: false,
                _capture: None,
                _subscription: subscription,
            }
        },
    );
    let input = state.read(cx).keys.clone();
    let for_reset = input.clone();
    let capturing = state.read(cx).capturing;
    let for_capture = state.clone();

    h_flex()
        .w_full()
        .gap_2()
        .items_center()
        .child(
            div()
                .flex_1()
                .min_w_0()
                .text_sm()
                .when(idle, |el| el.text_color(cx.theme().muted_foreground))
                .child(tr!(entry.label)),
        )
        .when(idle, |el| {
            el.child(
                div()
                    .text_xs()
                    .text_color(cx.theme().muted_foreground)
                    .child(tr!("settings-shortcut-vim-only")),
            )
        })
        // The warnings are read, not guessed: a key gpui cannot make sense of,
        // and a key two bindings claim under the same predicate — which
        // declaration order settles, silently.
        .when(invalid, |el| {
            el.child(
                icon("triangle-alert")
                    .xsmall()
                    .text_color(cx.theme().danger),
            )
            .child(
                div()
                    .text_xs()
                    .text_color(cx.theme().danger)
                    .child(tr!("settings-shortcut-invalid")),
            )
        })
        .when(conflict && !invalid, |el| {
            el.child(
                icon("triangle-alert")
                    .xsmall()
                    .text_color(cx.theme().warning),
            )
            .child(
                div()
                    .text_xs()
                    .text_color(cx.theme().warning)
                    .child(tr!("settings-shortcut-conflict")),
            )
        })
        .child(div().w(px(180.)).child(Input::new(&input).small()))
        // Pressing the keys rather than spelling them, which is what one wants
        // nine times out of ten; the field stays for the tenth — a sequence
        // (`g g`) has no single keystroke to capture, and emptying it is how a
        // binding is switched off.
        .child(
            Button::new(SharedString::from(format!("capture-{id}")))
                .small()
                .when(capturing, |el| el.primary())
                .when(!capturing, |el| el.outline())
                .label(if capturing {
                    tr!("settings-shortcut-capturing")
                } else {
                    tr!("settings-shortcut-capture")
                })
                .on_click(move |_, _window, cx| {
                    start_capture(entry, &for_capture, cx);
                }),
        )
        .child(
            Button::new(SharedString::from(format!("reset-{id}")))
                .ghost()
                .xsmall()
                .icon(icon("undo-2"))
                .tooltip(tr!("settings-shortcut-reset"))
                .disabled(!customised)
                .on_click(move |_, window, cx| {
                    Settings::update_global(cx, |settings| {
                        settings.shortcuts.remove(&entry.id());
                    });
                    for_reset.update(cx, |input, cx| {
                        input.set_value(entry.keys, window, cx);
                    });
                    crate::ui::shortcuts::rebind(cx);
                }),
        )
}

fn review_page() -> SettingPage {
    SettingPage::new(tr!("settings-page-review"))
        .group(
            SettingGroup::new()
                .title(tr!("settings-group-integration"))
                .item(
                    SettingItem::new(
                        tr!("settings-update-rebase"),
                        SettingField::switch(
                            |cx: &App| Settings::global(cx).update_with_rebase,
                            |value: bool, cx: &mut App| {
                                Settings::update_global(cx, |s| s.update_with_rebase = value)
                            },
                        )
                        .default_value(false),
                    )
                    .description(tr!("settings-update-rebase-help")),
                )
                .item(
                    SettingItem::new(
                        tr!("settings-integrate-no-ff"),
                        SettingField::switch(
                            |cx: &App| Settings::global(cx).integrate_no_ff,
                            |value: bool, cx: &mut App| {
                                Settings::update_global(cx, |s| s.integrate_no_ff = value)
                            },
                        )
                        .default_value(true),
                    )
                    .description(tr!("settings-integrate-no-ff-help")),
                )
                .item(
                    SettingItem::new(
                        tr!("settings-commit-message"),
                        SettingField::input(
                            |cx: &App| Settings::global(cx).commit_message_command.clone().into(),
                            |value: SharedString, cx: &mut App| {
                                Settings::update_global(cx, |s| {
                                    s.commit_message_command = value.to_string()
                                })
                            },
                        )
                        .default_value(SharedString::from(
                            crate::ui::settings::DEFAULT_COMMIT_MESSAGE_COMMAND,
                        )),
                    )
                    .description(tr!("settings-commit-message-help")),
                )
                .item(
                    SettingItem::new(
                        tr!("settings-auto-fetch"),
                        SettingField::number_input(
                            NumberFieldOptions {
                                min: 0.,
                                max: 240.,
                                step: 5.,
                            },
                            |cx: &App| Settings::global(cx).auto_fetch_minutes as f64,
                            |value: f64, cx: &mut App| {
                                Settings::update_global(cx, |s| {
                                    s.auto_fetch_minutes = value.clamp(0., 240.) as u32
                                })
                            },
                        )
                        .default_value(10.0),
                    )
                    .description(tr!("settings-auto-fetch-help")),
                ),
        )
        .group(
            SettingGroup::new()
                .title(tr!("settings-group-diff"))
                .item(
                    SettingItem::new(
                        tr!("settings-diff-context"),
                        SettingField::number_input(
                            NumberFieldOptions {
                                min: 0.,
                                max: 50.,
                                step: 1.,
                            },
                            |cx: &App| Settings::global(cx).diff_context as f64,
                            |value: f64, cx: &mut App| {
                                Settings::update_global(cx, |s| {
                                    s.diff_context = value.clamp(0., 50.) as usize
                                })
                            },
                        )
                        .default_value(3.0),
                    )
                    .description(tr!("settings-diff-context-help")),
                )
                .item(SettingItem::render(|_, _, cx| {
                    div()
                        .text_xs()
                        .text_color(gpui_component::ActiveTheme::theme(cx).muted_foreground)
                        .child(tr!("settings-diff-context-note"))
                }))
                .item(
                    SettingItem::new(
                        tr!("settings-diff-whole-file"),
                        SettingField::switch(
                            |cx: &App| Settings::global(cx).diff_whole_file,
                            |value: bool, cx: &mut App| {
                                Settings::update_global(cx, |s| s.diff_whole_file = value)
                            },
                        )
                        .default_value(false),
                    )
                    .description(tr!("settings-diff-whole-file-help")),
                )
                .item(
                    SettingItem::new(
                        tr!("settings-diff-split"),
                        SettingField::switch(
                            |cx: &App| Settings::global(cx).diff_split,
                            |value: bool, cx: &mut App| {
                                Settings::update_global(cx, |s| s.diff_split = value)
                            },
                        )
                        .default_value(false),
                    )
                    .description(tr!("settings-diff-split-help")),
                ),
        )
}

fn files_page() -> SettingPage {
    SettingPage::new(tr!("settings-page-files")).group(
        SettingGroup::new()
            .item(
                SettingItem::new(
                    tr!("settings-external-editor"),
                    SettingField::input(
                        |cx: &App| Settings::global(cx).external_editor.clone().into(),
                        |value: SharedString, cx: &mut App| {
                            Settings::update_global(cx, |s| s.external_editor = value.to_string())
                        },
                    )
                    .default_value(SharedString::default()),
                )
                .description(tr!("settings-external-editor-help")),
            )
            .item(
                SettingItem::new(
                    tr!("settings-notes-dir"),
                    SettingField::input(
                        |cx: &App| Settings::global(cx).notes_dir.clone().into(),
                        |value: SharedString, cx: &mut App| {
                            Settings::update_global(cx, |s| s.notes_dir = value.to_string())
                        },
                    )
                    .default_value(SharedString::default()),
                )
                .description(tr!("settings-notes-dir-help")),
            )
            // The distribution is only chosen here after the fact: the question
            // is asked on first startup, where no setting is known yet. The
            // field exists to change it — a machine often has several — and the
            // change only takes effect on the next launch, the running server
            // already having its repositories open.
            .item(
                SettingItem::new(
                    tr!("settings-wsl-distro"),
                    SettingField::input(
                        |cx: &App| Settings::global(cx).wsl_distro.clone().into(),
                        |value: SharedString, cx: &mut App| {
                            Settings::update_global(cx, |s| s.wsl_distro = value.to_string())
                        },
                    )
                    .default_value(SharedString::default()),
                )
                .description(tr!("settings-wsl-distro-help")),
            )
            .item(
                SettingItem::new(
                    tr!("settings-show-ignored"),
                    SettingField::switch(
                        |cx: &App| Settings::global(cx).show_ignored_files,
                        |value: bool, cx: &mut App| {
                            Settings::update_global(cx, |s| s.show_ignored_files = value)
                        },
                    )
                    .default_value(false),
                )
                .description(tr!("settings-show-ignored-help")),
            ),
    )
}

/// Sentry: the organisation and the token. The **project** belongs to the
/// repository and lives in the state store, not here — two repositories of the
/// same organisation do not have the same errors.
/// The databases page.
///
/// Connections are declared here and nowhere else: it is the second level of the
/// extension system — a declaration, not code — the same as the agent profiles',
/// and the "Databases" panel is only the view of that list.
fn databases_page() -> SettingPage {
    SettingPage::new(tr!("settings-page-databases")).group(
        SettingGroup::new().item(databases_item()).item(
            SettingItem::new(
                tr!("settings-db-page-size"),
                SettingField::number_input(
                    NumberFieldOptions {
                        min: 1.,
                        max: 100_000.,
                        step: 100.,
                    },
                    |cx: &App| Settings::global(cx).db_page_size as f64,
                    |value: f64, cx: &mut App| {
                        Settings::update_global(cx, |s| {
                            s.db_page_size = value.clamp(1., 100_000.) as usize
                        })
                    },
                )
                .default_value(500.),
            )
            .description(tr!("settings-db-page-size-help")),
        ),
    )
}

/// The connections table.
///
/// **A `SettingItem::render` and not a `SettingItem::new`**, the only one in
/// this whole window. An ordinary item puts its label in one column and its
/// field in what is left — four hundred pixels, cut for a checkbox or a menu —
/// and a connection needs five: name, host, port, user, password. The first
/// attempt set a hard-coded width, which overflowed the column and pushed the
/// engine picker off screen. A free element takes the whole page, and the title
/// is written by hand.
///
/// **The state key carries the number of connections**
/// (`claudhub-database-{n}-{i}`), like the agent profiles' and for the same
/// reason: `use_keyed_state` keeps one state per key, and without the count,
/// deleting the first connection would leave row 0's fields filled with the old
/// one — we would write into the settings what we thought we had deleted.
/// Changing engine, on the other hand, does not change the count: the fields
/// keep what had been typed into them.
fn databases_item() -> SettingItem {
    SettingItem::render(move |_, window, cx| {
        let connections = Settings::global(cx).databases.clone();
        let count = connections.len();
        let rows: Vec<_> = connections
            .iter()
            .enumerate()
            .map(|(index, connection)| database_row(index, count, connection, window, cx))
            .collect();
        v_flex()
            .w_full()
            .gap_2()
            .child(
                v_flex()
                    .gap_1()
                    .child(div().text_sm().child(tr!("settings-databases")))
                    .child(
                        div()
                            .text_sm()
                            .text_color(cx.theme().muted_foreground)
                            .child(tr!("settings-databases-help")),
                    ),
            )
            .children(rows)
            .child(
                h_flex().child(
                    Button::new("add-database")
                        .outline()
                        .small()
                        .icon(icon("plus"))
                        .label(tr!("settings-database-add"))
                        .on_click(|_, _window, cx| {
                            Settings::update_global(cx, |s| {
                                s.databases.push(crate::db::Connection::default())
                            });
                        }),
                ),
            )
    })
}

/// A connection's fields, kept from one render to the next.
struct DatabaseField {
    name: Entity<InputState>,
    path: Entity<InputState>,
    host: Entity<InputState>,
    port: Entity<InputState>,
    user: Entity<InputState>,
    password: Entity<InputState>,
    databases: Entity<InputState>,
    scope: Entity<InputState>,
    _subscriptions: Vec<Subscription>,
}

/// A connection: its name, its engine, and the address that engine asks for.
///
/// **The engine is chosen with two buttons and not with a dropdown.** It is the
/// first gesture — an empty connection opens on SQLite, and one has to be able
/// to leave it without guessing that a button hides a list. Both labels read at
/// a glance, which is exactly what one is after when coming to declare a MariaDB
/// database.
///
/// **`min_w_0` on every elastic field**: a flex item's minimum size defaults to
/// its content's, and an input does not go below its own — without it, a narrow
/// row pushes its neighbours out instead of shrinking them. It is the same trap
/// as the scrollbars'.
fn database_row(
    index: usize,
    count: usize,
    connection: &crate::db::Connection,
    window: &mut Window,
    cx: &mut App,
) -> impl IntoElement {
    let key = format!("claudhub-database-{count}-{index}");
    let values = connection.clone();
    let state = window.use_keyed_state(SharedString::from(key), cx, move |window, cx| {
        let mut field = |placeholder: SharedString,
                         value: String,
                         masked: bool,
                         cx: &mut Context<DatabaseField>| {
            cx.new(|cx| {
                let state = InputState::new(window, cx)
                    .placeholder(placeholder)
                    .default_value(value);
                if masked {
                    state.masked(true)
                } else {
                    state
                }
            })
        };
        let name = field(
            tr!("settings-database-name"),
            values.name.clone(),
            false,
            cx,
        );
        let path = field(
            tr!("settings-database-path"),
            values.path.clone(),
            false,
            cx,
        );
        let host = field(
            tr!("settings-database-host"),
            values.host.clone(),
            false,
            cx,
        );
        let port = field(
            tr!("settings-database-port"),
            // Zero means "the engine's port": showing it would suggest that port
            // 0 had been chosen.
            if values.port == 0 {
                String::new()
            } else {
                values.port.to_string()
            },
            false,
            cx,
        );
        let user = field(
            tr!("settings-database-user"),
            values.user.clone(),
            false,
            cx,
        );
        let password = field(
            tr!("settings-database-password"),
            values.password.clone(),
            true,
            cx,
        );
        let databases = field(
            tr!("settings-database-databases"),
            values.databases.join(", "),
            false,
            cx,
        );
        let scope = field(
            SharedString::from(format!(
                "{} — {}",
                tr!("settings-database-scope"),
                crate::db::scope::EXAMPLE
            )),
            values.scope.clone(),
            false,
            cx,
        );
        let watch = |input: &Entity<InputState>,
                     edit: fn(&mut crate::db::Connection, String),
                     cx: &mut Context<DatabaseField>| {
            cx.subscribe(
                input,
                move |_: &mut DatabaseField, input, event: &InputEvent, cx| {
                    if !matches!(event, InputEvent::Change) {
                        return;
                    }
                    let value = input.read(cx).value().to_string();
                    edit_database(index, cx, |connection| edit(connection, value));
                },
            )
        };
        let subscriptions = vec![
            watch(&name, |c, v| c.name = v, cx),
            watch(&path, |c, v| c.path = v, cx),
            watch(&host, |c, v| c.host = v, cx),
            // An unreadable port counts as zero, that is, "the engine's": a
            // half-typed field must not prevent writing the rest.
            watch(&port, |c, v| c.port = v.trim().parse().unwrap_or(0), cx),
            watch(&user, |c, v| c.user = v, cx),
            watch(&password, |c, v| c.password = v, cx),
            watch(
                &databases,
                |c, v| {
                    c.databases = v
                        .split(',')
                        .map(|name| name.trim().to_string())
                        .filter(|name| !name.is_empty())
                        .collect()
                },
                cx,
            ),
            watch(&scope, |c, v| c.scope = v, cx),
        ];
        DatabaseField {
            name,
            path,
            host,
            port,
            user,
            password,
            databases,
            scope,
            _subscriptions: subscriptions,
        }
    });
    let field = state.read(cx);
    let (name, path, host, port, user, password, databases, scope) = (
        field.name.clone(),
        field.path.clone(),
        field.host.clone(),
        field.port.clone(),
        field.user.clone(),
        field.password.clone(),
        field.databases.clone(),
        field.scope.clone(),
    );
    let engine = connection.engine;
    let sqlite = engine == crate::db::Engine::Sqlite;

    v_flex()
        .w_full()
        .min_w_0()
        .gap_1p5()
        .p_2()
        .rounded(cx.theme().radius)
        .border_1()
        .border_color(cx.theme().border)
        .child(
            h_flex()
                .w_full()
                .min_w_0()
                .gap_1()
                .items_center()
                .child(div().flex_1().min_w_0().child(Input::new(&name).small()))
                .children(crate::db::Engine::ALL.map(|choice| {
                    let chosen = engine == choice;
                    Button::new(("database-engine", index * 10 + choice as usize))
                        .small()
                        // Solid against outline, and not a button's "selected"
                        // state alone: on two neighbouring buttons of the same
                        // variant, the nuance it brings does not read, and that
                        // is precisely the question one asks on arriving — which
                        // of the two is active.
                        .map(|button| {
                            if chosen {
                                button.primary()
                            } else {
                                button.outline()
                            }
                        })
                        .selected(chosen)
                        .label(SharedString::new_static(choice.label()))
                        .on_click(move |_, _window, cx| {
                            edit_database(index, cx, |connection| connection.engine = choice);
                        })
                }))
                .child(
                    Button::new(("remove-database", index))
                        .ghost()
                        .small()
                        .icon(icon("trash-2"))
                        .tooltip(tr!("settings-database-remove"))
                        .on_click(move |_, _window, cx| {
                            Settings::update_global(cx, |s| {
                                if index < s.databases.len() {
                                    s.databases.remove(index);
                                }
                            });
                        }),
                ),
        )
        .when(sqlite, |this| {
            this.child(div().w_full().min_w_0().child(Input::new(&path).small()))
        })
        .when(!sqlite, |this| {
            this.child(
                h_flex()
                    .w_full()
                    .min_w_0()
                    .gap_1()
                    .child(div().flex_1().min_w_0().child(Input::new(&host).small()))
                    .child(
                        div()
                            .w(px(72.))
                            .flex_none()
                            .child(Input::new(&port).small()),
                    ),
            )
            .child(
                h_flex()
                    .w_full()
                    .min_w_0()
                    .gap_1()
                    .child(div().flex_1().min_w_0().child(Input::new(&user).small()))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .child(Input::new(&password).small()),
                    ),
            )
            .child(
                div()
                    .w_full()
                    .min_w_0()
                    .child(Input::new(&databases).small()),
            )
        })
        // The worktree scope, for both engines: it says which of the databases
        // belong to the checkout being reviewed, and a SQLite file attached
        // beside others is the same question.
        .child(div().w_full().min_w_0().child(Input::new(&scope).small()))
        .child(
            div()
                .text_xs()
                .text_color(cx.theme().muted_foreground)
                .child(tr!("settings-database-scope-help")),
        )
}

/// The language servers page.
///
/// The third list declared in the settings, after the agent profiles and the
/// database connections, and the same reasoning: a declaration, not code. What
/// it does **not** carry is the server's own configuration — PHPantom reads a
/// `.phpantom.toml` it watches itself, and a second place to say the same thing
/// would be a second truth. Nor the environment, which stays writable by hand
/// in `settings.json`: a language server takes its settings from its own file,
/// where an agent takes its model from a variable.
fn lsp_page() -> SettingPage {
    SettingPage::new(tr!("settings-page-lsp")).group(SettingGroup::new().item(lsp_item()))
}

/// The servers table, a `SettingItem::render` for the databases' reason: four
/// fields do not fit in the column an ordinary item leaves.
fn lsp_item() -> SettingItem {
    SettingItem::render(move |_, window, cx| {
        let servers = Settings::global(cx).lsp.clone();
        let count = servers.len();
        let rows: Vec<_> = servers
            .iter()
            .enumerate()
            .map(|(index, server)| lsp_row(index, count, server, window, cx))
            .collect();
        v_flex()
            .w_full()
            .gap_2()
            .child(
                v_flex()
                    .gap_1()
                    .child(div().text_sm().child(tr!("settings-lsp")))
                    .child(
                        div()
                            .text_sm()
                            .text_color(cx.theme().muted_foreground)
                            .child(tr!("settings-lsp-help")),
                    ),
            )
            .children(rows)
            .child(
                h_flex().child(
                    Button::new("add-lsp")
                        .outline()
                        .small()
                        .icon(icon("plus"))
                        .label(tr!("settings-lsp-add"))
                        .on_click(|_, _window, cx| {
                            Settings::update_global(cx, |s| s.lsp.push(Default::default()));
                        }),
                ),
            )
    })
}

/// A server's fields, kept from one render to the next.
struct LspField {
    name: Entity<InputState>,
    command: Entity<InputState>,
    extensions: Entity<InputState>,
    language: Entity<InputState>,
    _subscriptions: Vec<Subscription>,
}

/// One declared server.
///
/// **The command and its arguments are one field**, split by
/// `cmdline::split_command`, which honours quotes: `split_whitespace` breaks on
/// every path containing a space, and that is a failure one only understands
/// after reading the code. The state key carries the count, like the databases'
/// and the agent profiles': without it, deleting the first row would leave row
/// zero's fields filled with the old one.
fn lsp_row(
    index: usize,
    count: usize,
    server: &crate::lsp::Server,
    window: &mut Window,
    cx: &mut App,
) -> impl IntoElement {
    let key = format!("claudhub-lsp-{count}-{index}");
    let values = server.clone();
    let state = window.use_keyed_state(SharedString::from(key), cx, move |window, cx| {
        let mut field = |placeholder: SharedString, value: String, cx: &mut Context<LspField>| {
            cx.new(|cx| {
                InputState::new(window, cx)
                    .placeholder(placeholder)
                    .default_value(value)
            })
        };
        let name = field(tr!("settings-lsp-name"), values.name.clone(), cx);
        let command = field(
            tr!("settings-lsp-command"),
            crate::cmdline::join_command(
                std::iter::once(values.command.clone()).chain(values.args.clone()),
            ),
            cx,
        );
        let extensions = field(
            tr!("settings-lsp-extensions"),
            values.extensions.join(", "),
            cx,
        );
        let language = field(tr!("settings-lsp-language"), values.language_id.clone(), cx);
        let watch = |input: &Entity<InputState>,
                     edit: fn(&mut crate::lsp::Server, String),
                     cx: &mut Context<LspField>| {
            cx.subscribe(
                input,
                move |_: &mut LspField, input, event: &InputEvent, cx| {
                    if !matches!(event, InputEvent::Change) {
                        return;
                    }
                    let value = input.read(cx).value().to_string();
                    edit_lsp(index, cx, |server| edit(server, value));
                },
            )
        };
        let subscriptions = vec![
            watch(&name, |s, v| s.name = v, cx),
            watch(
                &command,
                |s, v| {
                    let mut parts = crate::cmdline::split_command(&v).into_iter();
                    s.command = parts.next().unwrap_or_default();
                    s.args = parts.collect();
                },
                cx,
            ),
            watch(
                &extensions,
                |s, v| {
                    s.extensions = v
                        .split(',')
                        .map(|ext| ext.trim().trim_start_matches('.').to_string())
                        .filter(|ext| !ext.is_empty())
                        .collect()
                },
                cx,
            ),
            watch(&language, |s, v| s.language_id = v.trim().to_string(), cx),
        ];
        LspField {
            name,
            command,
            extensions,
            language,
            _subscriptions: subscriptions,
        }
    });
    let field = state.read(cx);
    let (name, command, extensions, language) = (
        field.name.clone(),
        field.command.clone(),
        field.extensions.clone(),
        field.language.clone(),
    );

    v_flex()
        .w_full()
        .min_w_0()
        .gap_1p5()
        .p_2()
        .rounded(cx.theme().radius)
        .border_1()
        .border_color(cx.theme().border)
        .child(
            h_flex()
                .w_full()
                .min_w_0()
                .gap_1()
                .items_center()
                .child(div().flex_1().min_w_0().child(Input::new(&name).small()))
                .child(
                    Button::new(("remove-lsp", index))
                        .ghost()
                        .small()
                        .icon(icon("trash-2"))
                        .tooltip(tr!("settings-lsp-remove"))
                        .on_click(move |_, _window, cx| {
                            Settings::update_global(cx, |s| {
                                if index < s.lsp.len() {
                                    s.lsp.remove(index);
                                }
                            });
                        }),
                ),
        )
        .child(div().w_full().min_w_0().child(Input::new(&command).small()))
        .child(
            h_flex()
                .w_full()
                .min_w_0()
                .gap_1()
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .child(Input::new(&extensions).small()),
                )
                .child(
                    div()
                        .w(px(120.))
                        .flex_none()
                        .child(Input::new(&language).small()),
                ),
        )
}

/// Changes a server in place, if the index still exists — a subscription set up
/// for row 2 outlives row 2 by a frame.
fn edit_lsp(index: usize, cx: &mut App, edit: impl FnOnce(&mut crate::lsp::Server)) {
    Settings::update_global(cx, |s| {
        if let Some(server) = s.lsp.get_mut(index) {
            edit(server);
        }
    });
}

/// Changes a connection in place, if the index still exists.
///
/// A subscription set up for row 2 outlives row 2's disappearance by a frame,
/// and writing out of bounds would panic in the middle of a render.
fn edit_database(index: usize, cx: &mut App, edit: impl FnOnce(&mut crate::db::Connection)) {
    Settings::update_global(cx, |s| {
        if let Some(connection) = s.databases.get_mut(index) {
            edit(connection);
        }
    });
}

/// What the logs page needs: the records, what is being shown of them, and a
/// way back to the application — the level is a posture of reading, not a
/// preference, and it lives in `ClaudhubApp` rather than in the settings file.
struct LogView {
    records: std::rc::Rc<Vec<crate::logging::Entry>>,
    level: log::LevelFilter,
    app: Entity<ClaudhubApp>,
}

/// How many rows are painted.
///
/// The ring holds two thousand, and this list is **not** virtualised: it lives
/// inside the form's page, which scrolls as one block. Painting two thousand
/// styled lines per frame is what a virtualised list exists to avoid, and the
/// tail is what a log is read from — hence a cap, and a line that says so
/// rather than a list that silently stops.
const LOG_ROWS: usize = 200;

/// The levels the filter offers, from the widest to the narrowest.
const LOG_LEVELS: [log::LevelFilter; 5] = [
    log::LevelFilter::Trace,
    log::LevelFilter::Debug,
    log::LevelFilter::Info,
    log::LevelFilter::Warn,
    log::LevelFilter::Error,
];

/// The colour of a level. Warnings and errors are the two one is looking for;
/// the rest is context, and painting it would make the page unreadable.
fn level_color(level: log::Level, cx: &App) -> gpui::Hsla {
    match level {
        log::Level::Error => cx.theme().danger,
        log::Level::Warn => cx.theme().warning,
        _ => cx.theme().muted_foreground,
    }
}

/// One record, as a line of text — what the copy button puts in the clipboard.
///
/// The same shape as what `env_logger` prints on stderr: a log pasted into a
/// report has to look like the one whoever reads it would have got from a
/// terminal.
fn log_line(entry: &crate::logging::Entry) -> String {
    format!(
        "[{} {:<5} {}] {}",
        entry.at.format("%Y-%m-%dT%H:%M:%S%.3f"),
        entry.level,
        entry.target,
        entry.message
    )
}

/// What Claudhub has written since it started.
///
/// **A page and not a file.** A graphical application has no console under its
/// window: without this, finding out why a fetch failed or why the remote server
/// died means relaunching from a terminal, which is asking the user to reproduce
/// the problem before being allowed to look at it.
fn logs_page(view: LogView) -> SettingPage {
    SettingPage::new(tr!("settings-page-logs"))
        // Nothing here is a setting, so nothing here resets.
        .resettable(false)
        .group(
            SettingGroup::new().item(SettingItem::render(move |_, _window, cx| {
                let LogView {
                    records,
                    level,
                    app,
                } = &view;
                // Filtered before being cut: the last two hundred **warnings** are
                // not the warnings among the last two hundred records, and the
                // second reading is the one that makes a page look empty.
                let shown: Vec<&crate::logging::Entry> = records
                    .iter()
                    .filter(|entry| entry.level <= *level)
                    .collect();
                let total = shown.len();
                let mono = cx.theme().mono_font_family.clone();
                let muted = cx.theme().muted_foreground;
                let rows = shown
                    .iter()
                    .rev()
                    .take(LOG_ROWS)
                    .rev()
                    .map(|entry| {
                        h_flex()
                            .w_full()
                            .gap_2()
                            .items_start()
                            .text_xs()
                            .font_family(mono.clone())
                            .child(
                                div()
                                    .flex_none()
                                    .text_color(muted)
                                    .child(entry.at.format("%H:%M:%S%.3f").to_string()),
                            )
                            .child(
                                div()
                                    .flex_none()
                                    .w(px(44.))
                                    .text_color(level_color(entry.level, cx))
                                    .child(entry.level.to_string()),
                            )
                            .child(
                                div()
                                    .flex_none()
                                    .max_w(px(160.))
                                    .truncate()
                                    .text_color(muted)
                                    .child(entry.target.clone()),
                            )
                            // No `truncate` on the message: a log line one cannot
                            // read to the end is a log line for nothing. It wraps.
                            .child(div().flex_1().min_w_0().child(entry.message.clone()))
                    })
                    .collect::<Vec<_>>();

                v_flex()
                    .w_full()
                    .gap_2()
                    .child(
                        div()
                            .text_sm()
                            .text_color(muted)
                            .child(tr!("settings-logs-help")),
                    )
                    .child(
                        h_flex()
                            .w_full()
                            .gap_2()
                            .items_center()
                            .justify_between()
                            .child(
                                ButtonGroup::new("log-level")
                                    .outline()
                                    .compact()
                                    .small()
                                    .children(LOG_LEVELS.map(|choice| {
                                        Button::new(("log-level", choice as usize))
                                            .label(choice.to_string())
                                            .selected(choice == *level)
                                    }))
                                    .on_click({
                                        let app = app.clone();
                                        move |selected: &Vec<usize>, _window, cx| {
                                            let Some(choice) =
                                                selected.first().and_then(|ix| LOG_LEVELS.get(*ix))
                                            else {
                                                return;
                                            };
                                            app.update(cx, |this, cx| {
                                                this.logs_level = *choice;
                                                cx.notify();
                                            });
                                        }
                                    }),
                            )
                            .child(
                                h_flex()
                                    .gap_1()
                                    .child(
                                        Button::new("log-copy")
                                            .outline()
                                            .small()
                                            .icon(icon("copy"))
                                            .label(tr!("settings-logs-copy"))
                                            .disabled(total == 0)
                                            .on_click({
                                                // Everything the filter kept, not
                                                // the two hundred painted: what goes
                                                // into a report is the log, not the
                                                // end of it.
                                                let text = shown
                                                    .iter()
                                                    .map(|entry| log_line(entry))
                                                    .collect::<Vec<_>>()
                                                    .join("\n");
                                                move |_, _window, cx| {
                                                    cx.write_to_clipboard(
                                                        gpui::ClipboardItem::new_string(
                                                            text.clone(),
                                                        ),
                                                    );
                                                }
                                            }),
                                    )
                                    .child(
                                        Button::new("log-clear")
                                            .outline()
                                            .small()
                                            .icon(icon("trash-2"))
                                            .label(tr!("settings-logs-clear"))
                                            .disabled(records.is_empty())
                                            .on_click({
                                                let app = app.clone();
                                                move |_, _window, cx| {
                                                    crate::logging::clear();
                                                    app.update(cx, |_, cx| cx.notify());
                                                }
                                            }),
                                    ),
                            ),
                    )
                    .when(total == 0, |el| {
                        el.child(
                            div()
                                .py_2()
                                .text_sm()
                                .text_color(muted)
                                .child(tr!("settings-logs-empty")),
                        )
                    })
                    // The cap is said rather than hidden: a list that stops without
                    // a word reads as a list that has nothing more in it.
                    .when(total > LOG_ROWS, |el| {
                        el.child(div().text_xs().text_color(muted).child(tr!(
                            "settings-logs-truncated",
                            { shown: LOG_ROWS.to_string(), total: total.to_string() }
                        )))
                    })
                    .children(rows)
            })),
        )
}

/// Bounds common to the text sizes, the same as the wheel's.
fn size_range() -> NumberFieldOptions {
    NumberFieldOptions {
        min: settings::MIN_FONT_SIZE as f64,
        max: settings::MAX_FONT_SIZE as f64,
        step: 1.0,
    }
}

fn clamp_size(value: f64) -> f32 {
    settings::clamp_font_size(value as f32)
}

// — Les plugins ————————————————————————————————————————————————————————

/// Acts on the application from a form closure.
///
/// The form's closures only receive an `App` — the very reason the settings
/// live in a global — so a gesture that has to reach `ClaudhubApp` needs a way
/// back. A **weak** handle, like the dock's panels hold: strong, it would keep
/// the application alive past the window.
///
/// Only from a **click**, never from a render. This closure's ancestors run
/// inside `ClaudhubApp`'s own render — the settings panel delegates to it — and
/// updating the entity there is the panic `open_dialog`'s closure already
/// taught this repository. A click handler runs once that borrow is given back.
fn with_app(
    window: &mut Window,
    cx: &mut App,
    f: impl FnOnce(&mut ClaudhubApp, &mut Window, &mut Context<ClaudhubApp>),
) {
    let Some(app) = cx
        .try_global::<crate::ui::app::AppHandle>()
        .and_then(|handle| handle.0.upgrade())
    else {
        return;
    };
    app.update(cx, |app, cx| f(app, window, cx));
}

/// The plugins page: what is installed, what it is told, and git.
///
/// **What it does not carry is as decided as what it does.** The compilation
/// status and its error stay in the plugin's own panel, where the reload button
/// already is and where the error is read beside the tree it replaces. The
/// reason is mechanical as much as editorial: this closure runs **inside**
/// `ClaudhubApp`'s own render — the settings panel delegates to it — so reading
/// the root entity here is the panic that `open_dialog`'s closure already
/// taught this repository. A click handler, on the other hand, runs later, once
/// that borrow is given back: that is why the buttons can act and the page
/// cannot report.
fn plugins_page() -> SettingPage {
    SettingPage::new(tr!("settings-page-plugins")).group(SettingGroup::new().item(plugins_item()))
}

/// The state the install row keeps between two frames.
struct PluginInstallField {
    url: Entity<InputState>,
    name: Entity<InputState>,
    _subscriptions: Vec<Subscription>,
}

/// One plugin's settings and secrets, kept from one render to the next.
struct PluginFields {
    values: Vec<(String, Entity<InputState>)>,
    secrets: Vec<(String, Entity<InputState>)>,
    _subscriptions: Vec<Subscription>,
}

fn plugins_item() -> SettingItem {
    SettingItem::render(move |_, window, cx| {
        let manifests = crate::ui::plugin_view::manifests();
        let count = manifests.len();
        let rows: Vec<_> = manifests
            .iter()
            .map(|manifest| plugin_row(count, manifest, window, cx))
            .collect();
        v_flex()
            .w_full()
            .gap_2()
            .child(
                v_flex()
                    .gap_1()
                    .child(div().text_sm().child(tr!("settings-plugins")))
                    .child(
                        div()
                            .text_sm()
                            .text_color(cx.theme().muted_foreground)
                            .child(tr!("settings-plugins-help")),
                    ),
            )
            .when(count == 0, |el| {
                el.child(
                    div()
                        .text_sm()
                        .text_color(cx.theme().muted_foreground)
                        .child(tr!("settings-plugins-none")),
                )
            })
            .children(rows)
            .child(plugin_install_row(count, window, cx))
    })
}

/// Cloning a plugin from a repository.
///
/// The name is **suggested** from the address and stays editable: two plugins
/// may well be published from repositories called `claudhub-plugin` by two
/// different people, and the directory's name is what everything else keys on.
fn plugin_install_row(count: usize, window: &mut Window, cx: &mut App) -> impl IntoElement {
    let key = format!("claudhub-plugin-install-{count}");
    let state = window.use_keyed_state(SharedString::from(key), cx, move |window, cx| {
        let url = cx.new(|cx| InputState::new(window, cx).placeholder(tr!("settings-plugin-url")));
        let name =
            cx.new(|cx| InputState::new(window, cx).placeholder(tr!("settings-plugin-name")));
        // Typing an address fills the name in, and only while nobody has
        // touched it: correcting a suggestion that keeps coming back is worse
        // than having none.
        // `subscribe_in` and not `subscribe`: writing into an input needs a
        // window, and an ordinary subscription has none — it is the only place
        // in this form that writes a field rather than reading it.
        let suggest = cx.subscribe_in(&url, window, {
            let name = name.clone();
            move |_: &mut PluginInstallField, url, event: &InputEvent, window: &mut Window, cx| {
                if !matches!(event, InputEvent::Change) {
                    return;
                }
                if !name.read(cx).value().trim().is_empty() {
                    return;
                }
                let Some(id) = crate::plugin::install::id_from_url(&url.read(cx).value()) else {
                    return;
                };
                name.update(cx, |state, cx| state.set_value(id, window, cx));
            }
        });
        PluginInstallField {
            url,
            name,
            _subscriptions: vec![suggest],
        }
    });
    let field = state.read(cx);
    let (url, name) = (field.url.clone(), field.name.clone());
    h_flex()
        .w_full()
        .min_w_0()
        .gap_1()
        .items_center()
        .child(div().flex_1().min_w_0().child(Input::new(&url).small()))
        .child(div().w(px(150.)).min_w_0().child(Input::new(&name).small()))
        .child(
            Button::new("install-plugin")
                .outline()
                .small()
                .icon(icon("download"))
                .label(tr!("settings-plugin-install"))
                .on_click({
                    let (url, name) = (url.clone(), name.clone());
                    move |_, window, cx| {
                        let address = url.read(cx).value().trim().to_string();
                        let id = name.read(cx).value().trim().to_string();
                        let id = if id.is_empty() {
                            crate::plugin::install::id_from_url(&address).unwrap_or_default()
                        } else {
                            id
                        };
                        with_app(window, cx, move |app, window, cx| {
                            app.manage_plugin(
                                &id,
                                crate::plugin::install::Manage::Install { url: address },
                                window,
                                cx,
                            );
                        });
                    }
                }),
        )
}

fn plugin_row(
    count: usize,
    manifest: &crate::plugin::manifest::Manifest,
    window: &mut Window,
    cx: &mut App,
) -> impl IntoElement {
    let id = manifest.id.clone();
    let key = format!("claudhub-plugin-{count}-{id}");
    let declared = manifest.declaration.clone();
    let configured = Settings::global(cx).plugins.get(&id).cloned();
    // Where each secret lives, read **now**: the keyed state below is built
    // once and would hold the picture from the frame a plugin was first shown,
    // so a token just put into the keyring would still read "in clear".
    let places: std::collections::BTreeMap<String, crate::plugin::host::SecretPlace> = declared
        .secrets
        .iter()
        .map(|name| {
            let value = configured
                .as_ref()
                .and_then(|p| p.secrets.get(name))
                .map(String::as_str)
                .unwrap_or_default();
            (name.clone(), crate::plugin::host::SecretPlace::of(value))
        })
        .collect();
    let enabled = configured.as_ref().map(|p| p.enabled).unwrap_or(true);
    let state = window.use_keyed_state(SharedString::from(key), cx, {
        let id = id.clone();
        move |window, cx| {
            let mut subscriptions = Vec::new();
            let mut values = Vec::new();
            for (name, default) in &declared.settings {
                let current = configured
                    .as_ref()
                    .and_then(|p| p.settings.get(name).cloned())
                    .unwrap_or_else(|| default.clone());
                let input = cx.new(|cx| {
                    InputState::new(window, cx)
                        .placeholder(SharedString::from(default.clone()))
                        .default_value(current)
                });
                subscriptions.push(watch_plugin_field(&input, id.clone(), name.clone(), cx));
                values.push((name.clone(), input));
            }
            let mut secrets = Vec::new();
            for name in &declared.secrets {
                let current = configured
                    .as_ref()
                    .and_then(|p| p.secrets.get(name).cloned())
                    .unwrap_or_default();
                let input = cx.new(|cx| {
                    InputState::new(window, cx)
                        // The placeholder says the three forms: a secret is the
                        // one field here whose *shape* carries a decision about
                        // where it lives, and nothing else on this page would
                        // say so.
                        .placeholder(SharedString::from(format!(
                            "{name} — {}",
                            tr!("settings-plugin-secret-help")
                        )))
                        .default_value(current)
                        .masked(true)
                });
                subscriptions.push(keep_on_blur(&input, id.clone(), name.clone(), window, cx));
                secrets.push((name.clone(), input));
            }
            PluginFields {
                values,
                secrets,
                _subscriptions: subscriptions,
            }
        }
    });
    let fields = state.read(cx);
    let values = fields.values.clone();
    let secrets = fields.secrets.clone();
    let revision = crate::plugin::install::revision(&manifest.dir);
    let muted = cx.theme().muted_foreground;
    let title = SharedString::from(manifest.title().to_string());
    let dir = manifest.dir.clone();

    v_flex()
        .w_full()
        .min_w_0()
        .gap_1p5()
        .p_2()
        .rounded(cx.theme().radius)
        .border_1()
        .border_color(cx.theme().border)
        .child(
            h_flex()
                .w_full()
                .min_w_0()
                .gap_2()
                .items_center()
                .child(icon(manifest.icon()).small())
                .child(
                    v_flex()
                        .flex_1()
                        .min_w_0()
                        .child(div().truncate().text_sm().child(title))
                        .child(
                            div()
                                .truncate()
                                .text_xs()
                                .text_color(muted)
                                .font_family(cx.theme().mono_font_family.clone())
                                .child(SharedString::from(match &revision {
                                    Some(revision) => format!("{id} — {revision}"),
                                    // A directory dropped in by hand is
                                    // perfectly legitimate: it simply has
                                    // nothing to pull from.
                                    None => format!("{id} — {}", tr!("settings-plugin-handmade")),
                                })),
                        ),
                )
                .child(
                    Button::new(SharedString::from(format!("plugin-enabled-{id}")))
                        .small()
                        .map(|button| {
                            if enabled {
                                button.primary()
                            } else {
                                button.outline()
                            }
                        })
                        .selected(enabled)
                        .label(tr!("settings-plugin-enabled"))
                        .on_click({
                            let id = id.clone();
                            move |_, _window, cx| {
                                let id = id.clone();
                                Settings::update_global(cx, move |s| {
                                    let entry = s.plugins.entry(id).or_default();
                                    entry.enabled = !entry.enabled;
                                });
                            }
                        }),
                )
                .child(
                    Button::new(SharedString::from(format!("plugin-edit-{id}")))
                        .ghost()
                        .small()
                        .icon(icon("pencil"))
                        .tooltip(tr!("settings-plugin-edit"))
                        .on_click({
                            let id = id.clone();
                            move |_, window, cx| {
                                let id = id.clone();
                                with_app(window, cx, move |app, window, cx| {
                                    let Some(manifest) = crate::ui::plugin_view::manifests()
                                        .iter()
                                        .find(|m| m.id == id)
                                    else {
                                        return;
                                    };
                                    app.edit_plugin(manifest, window, cx);
                                });
                            }
                        }),
                )
                .child(
                    Button::new(SharedString::from(format!("plugin-update-{id}")))
                        .ghost()
                        .small()
                        .icon(icon("refresh-cw"))
                        .tooltip(tr!("settings-plugin-update"))
                        .disabled(revision.is_none())
                        .on_click({
                            let id = id.clone();
                            move |_, window, cx| {
                                let id = id.clone();
                                with_app(window, cx, move |app, window, cx| {
                                    app.manage_plugin(
                                        &id,
                                        crate::plugin::install::Manage::Update,
                                        window,
                                        cx,
                                    );
                                });
                            }
                        }),
                )
                .child(
                    Button::new(SharedString::from(format!("plugin-remove-{id}")))
                        .ghost()
                        .small()
                        .icon(icon("trash-2"))
                        .tooltip(tr!("settings-plugin-remove"))
                        .on_click({
                            let id = id.clone();
                            move |_, window, cx| {
                                let id = id.clone();
                                with_app(window, cx, move |app, window, cx| {
                                    app.confirm_plugin_removal(id.clone(), window, cx);
                                });
                            }
                        }),
                ),
        )
        .when(!values.is_empty() || !secrets.is_empty(), |el| {
            el.child(
                v_flex()
                    .w_full()
                    .min_w_0()
                    .gap_1()
                    .children(
                        values
                            .into_iter()
                            .map(|(name, input)| plugin_field_row(name, input, cx)),
                    )
                    .children(secrets.into_iter().map(|(name, input)| {
                        let place = places
                            .get(&name)
                            .copied()
                            .unwrap_or(crate::plugin::host::SecretPlace::Plain);
                        plugin_secret_row(name, input, place, cx)
                    })),
            )
        })
        .child(
            div()
                .text_xs()
                .text_color(muted)
                .child(SharedString::from(dir.display().to_string())),
        )
}

/// A secret's field, and one word saying where its value actually is.
///
/// It is the question one comes to this page with — "is my token in a file?" —
/// and nothing else on the row would answer it: the field is masked, and a
/// keyring reference behind dots looks exactly like a token.
fn plugin_secret_row(
    name: String,
    input: Entity<InputState>,
    place: crate::plugin::host::SecretPlace,
    cx: &mut App,
) -> impl IntoElement {
    use crate::plugin::host::SecretPlace;
    let colour = match place {
        // In clear in a file is not an error, but it is the one of the three
        // worth noticing.
        SecretPlace::Plain => cx.theme().warning,
        _ => cx.theme().muted_foreground,
    };
    v_flex()
        .w_full()
        .min_w_0()
        .gap_0p5()
        .child(plugin_field_row(name, input, cx))
        .child(
            div()
                .pl(px(138.))
                .text_xs()
                .text_color(colour)
                .child(tr!(place.label())),
        )
}

fn plugin_field_row(name: String, input: Entity<InputState>, cx: &mut App) -> impl IntoElement {
    h_flex()
        .w_full()
        .min_w_0()
        .gap_2()
        .items_center()
        .child(
            div()
                .w(px(130.))
                .truncate()
                .text_xs()
                .text_color(cx.theme().muted_foreground)
                .child(SharedString::from(name)),
        )
        .child(div().flex_1().min_w_0().child(Input::new(&input).small()))
}

fn watch_plugin_field(
    input: &Entity<InputState>,
    id: String,
    name: String,
    cx: &mut Context<PluginFields>,
) -> Subscription {
    cx.subscribe(
        input,
        move |_: &mut PluginFields, input, event: &InputEvent, cx| {
            if !matches!(event, InputEvent::Change) {
                return;
            }
            let value = input.read(cx).value().to_string();
            let (id, name) = (id.clone(), name.clone());
            Settings::update_global(cx, move |s| {
                s.plugins
                    .entry(id)
                    .or_default()
                    .settings
                    .insert(name, value);
            });
        },
    )
}

/// A secret is put away when one **leaves** the field, never on a keystroke.
///
/// Storing it means a round trip to the system keyring, which can ask the user
/// to unlock it: doing that per character would be absurd, and would ask once
/// per letter. Losing the focus validates, which is already this window's rule
/// for the task list — `InputState` has no escape event, and abandoning what
/// was typed because one clicked beside it is the worse of the two defaults.
fn keep_on_blur(
    input: &Entity<InputState>,
    id: String,
    name: String,
    window: &mut Window,
    cx: &mut Context<PluginFields>,
) -> Subscription {
    cx.subscribe_in(
        input,
        window,
        move |_: &mut PluginFields, input, event: &InputEvent, window: &mut Window, cx| {
            if !matches!(event, InputEvent::Blur | InputEvent::PressEnter { .. }) {
                return;
            }
            let value = input.read(cx).value().to_string();
            let (id, name) = (id.clone(), name.clone());
            with_app(window, cx, move |app, window, cx| {
                app.keep_secret(id, name, value, window, cx);
            });
        },
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_detected_shells_are_absolute_paths() {
        // The menu fills a field that will be executed: a relative entry there
        // would depend on the worktree's current directory.
        for (value, _) in shell_choices() {
            assert!(value.starts_with('/'), "{value}");
        }
    }
}
