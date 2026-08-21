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
use gpui_component::{h_flex, v_flex, ActiveTheme, Disableable, Selectable, Sizable};

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
        ];
        let databases_ix = pages.len();
        pages.push(databases_page());
        pages.push(sentry_page());
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

/// Shells que le système déclare. Le menu ne fait que les proposer : le champ
/// reste libre, et vide veut toujours dire « le shell de connexion ».
fn shell_choices() -> Vec<(SharedString, SharedString)> {
    choices(settings::available_shells())
}

/// L'état du champ « shell », gardé d'un rendu à l'autre.
///
/// La souscription vit dedans : un `Subscription` lâché se coupe, et le champ
/// cesserait d'écrire dans les réglages dès la frame suivante.
struct ShellField {
    input: Entity<InputState>,
    _subscription: Subscription,
}

/// Le shell se saisit librement, et le menu ne fait que proposer.
///
/// Une liste fermée conviendrait si `/etc/shells` disait la vérité ; il ignore
/// tout ce qui n'est pas installé par le système — un shell compilé à la main,
/// un `nix run`, un `tmux new-session` — et on ne veut pas d'un réglage dont
/// on sort en éditant un fichier JSON.
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

/// L'état d'une ligne de la table des profils, gardé d'un rendu à l'autre.
///
/// Les trois souscriptions vivent dedans : lâchées, elles se couperaient et
/// les champs cesseraient d'écrire dans les réglages à la frame suivante.
struct AgentField {
    name: Entity<InputState>,
    command: Entity<InputState>,
    env: Entity<InputState>,
    _subscriptions: Vec<Subscription>,
}

/// La table des profils d'agent.
///
/// Un champ sur mesure parce qu'il n'y a rien d'approchant dans le formulaire
/// de gpui-component : ce sont des lignes qu'on ajoute et qu'on retire, avec
/// trois saisies chacune.
///
/// **La clé d'état porte le nombre de profils** (`claudhub-agent-{n}-{i}`).
/// `use_keyed_state` garde un état par clé : sans le compte, supprimer le
/// premier profil laisserait les champs de la ligne 0 remplis avec l'ancien,
/// et l'on écrirait dans les réglages ce qu'on croyait avoir supprimé.
/// Renommer un profil, lui, ne change pas le compte — les champs gardent donc
/// leur curseur pendant la frappe.
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
                        // La ligne est découpée en honorant les guillemets :
                        // un chemin contenant une espace ne doit pas devenir
                        // deux arguments.
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

/// Le profil lancé quand on ne dit pas lequel.
///
/// La liste des choix est relue à chaque rendu du formulaire : elle change
/// pendant qu'on édite la table juste au-dessus.
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

/// Modifie un profil en place, si l'indice existe encore.
///
/// L'indice peut être périmé d'une frame : une souscription posée pour la
/// ligne 2 survit à la disparition de la ligne 2, et écrire hors des bornes
/// paniquerait au milieu d'un rendu.
fn edit_agent(index: usize, cx: &mut App, edit: impl FnOnce(&mut settings::AgentProfile)) {
    Settings::update_global(cx, |s| {
        if let Some(profile) = s.terminal.agents.get_mut(index) {
            edit(profile);
        }
    });
}

/// Les palettes du registre pour une apparence donnée.
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
    // La police du terminal reprend la liste des chasses fixes, précédée de
    // l'entrée « comme les diffs » : le terminal a le droit de ne pas choisir,
    // pour que régler la chasse fixe une fois suffise au cas courant.
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

/// Le clavier.
///
/// Une page pour un seul réglage, et c'est assumé : le mode vim change le sens
/// de la moitié des touches, et c'est le premier endroit où l'on va le
/// chercher. Le rappel de `F1` y est parce qu'une aide qu'on ne trouve pas
/// n'en est pas une.
fn keyboard_page() -> SettingPage {
    SettingPage::new(tr!("settings-page-keyboard")).group(
        SettingGroup::new().title(tr!("settings-group-vim")).item(
            SettingItem::new(
                tr!("settings-vim-mode"),
                SettingField::switch(
                    |cx: &App| Settings::global(cx).vim_mode,
                    |value: bool, cx: &mut App| Settings::update_global(cx, |s| s.vim_mode = value),
                )
                .default_value(false),
            )
            .description(tr!("settings-vim-mode-help")),
        ),
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
            // La distribution ne se choisit ici qu'après coup : la question
            // est posée au premier démarrage, où l'on ne connaît encore aucun
            // réglage. Le champ existe pour en changer — une machine en a
            // souvent plusieurs — et le changement ne vaut qu'au prochain
            // lancement, le serveur en place ayant déjà ses dépôts ouverts.
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

/// Sentry : l'organisation et le jeton. Le **projet** dépend du dépôt et vit
/// dans le magasin d'état, pas ici — deux dépôts d'une même organisation n'ont
/// pas les mêmes erreurs.
/// La page des bases de données.
///
/// Les connexions se déclarent ici et nulle part ailleurs : c'est le
/// deuxième niveau du système d'extension — une déclaration, pas du code —, le
/// même que celui des profils d'agent, et le panneau « Bases » n'est que la
/// vue de cette liste.
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

/// La table des connexions.
///
/// **Un `SettingItem::render` et non un `SettingItem::new`**, seul de toute
/// cette fenêtre. Un item ordinaire met son libellé dans une colonne et son
/// champ dans ce qui reste — quatre cents pixels, taillés pour une case à
/// cocher ou un menu — et une connexion en demande cinq : nom, hôte, port,
/// utilisateur, mot de passe. Le premier essai posait une largeur en dur, ce
/// qui débordait de la colonne et sortait le sélecteur de moteur de l'écran.
/// Un élément libre prend la page entière, et le titre est écrit à la main.
///
/// **La clé d'état porte le nombre de connexions**
/// (`claudhub-database-{n}-{i}`), comme celle des profils d'agent et pour la
/// même raison : `use_keyed_state` garde un état par clé, et sans le compte,
/// supprimer la première connexion laisserait les champs de la ligne 0
/// remplis avec l'ancienne — on écrirait dans les réglages ce qu'on croyait
/// avoir supprimé. Changer de moteur, lui, ne change pas le compte : les
/// champs gardent ce qu'on y avait tapé.
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

/// Les champs d'une connexion, gardés d'un rendu à l'autre.
struct DatabaseField {
    name: Entity<InputState>,
    path: Entity<InputState>,
    host: Entity<InputState>,
    port: Entity<InputState>,
    user: Entity<InputState>,
    password: Entity<InputState>,
    databases: Entity<InputState>,
    _subscriptions: Vec<Subscription>,
}

/// Une connexion : son nom, son moteur, et l'adresse que ce moteur demande.
///
/// **Le moteur se choisit par deux boutons et non par un menu déroulant.**
/// C'est le premier geste — une connexion vide s'ouvre sur SQLite, et il faut
/// pouvoir en sortir sans deviner qu'un bouton cache une liste. Les deux
/// libellés se lisent d'un coup d'œil, ce qui est justement ce qu'on cherche
/// quand on vient déclarer une base MariaDB.
///
/// **`min_w_0` sur chaque champ élastique** : la taille minimale d'un élément
/// flex vaut par défaut celle de son contenu, et une saisie ne descend pas
/// sous la sienne — sans lui, une ligne étroite pousse ses voisins dehors au
/// lieu de les rétrécir. C'est le même piège que celui des barres de
/// défilement.
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
            // Zéro veut dire « le port du moteur » : l'afficher ferait croire
            // qu'on a choisi le port 0.
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
            // Un port illisible vaut zéro, c'est-à-dire « celui du moteur » :
            // un champ à moitié tapé ne doit pas empêcher d'écrire la suite.
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
        ];
        DatabaseField {
            name,
            path,
            host,
            port,
            user,
            password,
            databases,
            _subscriptions: subscriptions,
        }
    });
    let field = state.read(cx);
    let (name, path, host, port, user, password, databases) = (
        field.name.clone(),
        field.path.clone(),
        field.host.clone(),
        field.port.clone(),
        field.user.clone(),
        field.password.clone(),
        field.databases.clone(),
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
                        // Plein contre contour, et non le seul état
                        // « sélectionné » d'un bouton : sur deux boutons
                        // voisins de même variante, la nuance qu'il apporte ne
                        // se lit pas, et c'est justement la question qu'on se
                        // pose en arrivant — lequel des deux est actif.
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
}

/// Modifie une connexion en place, si l'indice existe encore.
///
/// Une souscription posée pour la ligne 2 survit d'une frame à la disparition
/// de la ligne 2, et écrire hors des bornes paniquerait au milieu d'un rendu.
fn edit_database(index: usize, cx: &mut App, edit: impl FnOnce(&mut crate::db::Connection)) {
    Settings::update_global(cx, |s| {
        if let Some(connection) = s.databases.get_mut(index) {
            edit(connection);
        }
    });
}

fn sentry_page() -> SettingPage {
    SettingPage::new(tr!("settings-page-sentry")).group(
        SettingGroup::new()
            .item(
                SettingItem::new(
                    tr!("settings-sentry-org"),
                    SettingField::input(
                        |cx: &App| Settings::global(cx).sentry_org.clone().into(),
                        |value: SharedString, cx: &mut App| {
                            Settings::update_global(cx, |s| s.sentry_org = value.to_string())
                        },
                    )
                    .default_value(SharedString::default()),
                )
                .description(tr!("settings-sentry-org-help")),
            )
            .item(
                SettingItem::new(
                    tr!("settings-sentry-token"),
                    SettingField::input(
                        |cx: &App| Settings::global(cx).sentry_token.clone().into(),
                        |value: SharedString, cx: &mut App| {
                            Settings::update_global(cx, |s| s.sentry_token = value.to_string())
                        },
                    )
                    .default_value(SharedString::default()),
                )
                .description(tr!("settings-sentry-token-help")),
            )
            .item(
                SettingItem::new(
                    tr!("settings-sentry-query"),
                    SettingField::input(
                        |cx: &App| Settings::global(cx).sentry_query.clone().into(),
                        |value: SharedString, cx: &mut App| {
                            Settings::update_global(cx, |s| s.sentry_query = value.to_string())
                        },
                    )
                    .default_value(SharedString::default()),
                )
                .description(tr!("settings-sentry-query-help")),
            ),
    )
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_detected_shells_are_absolute_paths() {
        // Le menu remplit un champ qui sera exécuté : une entrée relative y
        // dépendrait du répertoire courant du worktree.
        for (value, _) in shell_choices() {
            assert!(value.starts_with('/'), "{value}");
        }
    }
}
