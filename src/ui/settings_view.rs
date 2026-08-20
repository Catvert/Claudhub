//! La fenêtre de réglages.
//!
//! Le formulaire vient de gpui-component : pages, groupes, recherche et bouton
//! de remise à zéro sont fournis, et chaque champ se déclare par un couple
//! lire/écrire. Ces fermetures ne reçoivent qu'un `App` — c'est ce qui impose
//! que les réglages vivent dans un global plutôt que dans `ClaudhubApp`.
//!
//! Il n'y a pas de bouton « Appliquer » : chaque changement prend effet à la
//! frappe et l'écriture du fichier suit, différée. Un formulaire qui demande
//! de valider pour voir le résultat rend le choix d'une police ou d'une taille
//! impossible autrement qu'à l'aveugle.

use gpui::{div, prelude::*, px, App, Context, Corner, Entity, SharedString, Subscription, Window};
use gpui_component::button::{Button, ButtonVariants};
use gpui_component::input::{Input, InputEvent, InputState};
use gpui_component::menu::{DropdownMenu, PopupMenuItem};
use gpui_component::setting::{
    NumberFieldOptions, SettingField, SettingGroup, SettingItem, SettingPage,
};
use gpui_component::{h_flex, v_flex, Sizable, WindowExt};

use crate::tr;
use crate::ui::app::ClaudhubApp;
use crate::ui::icons::icon;
use crate::ui::settings::{
    self, LanguageChoice, Settings, ThemeMode, DEFAULT_MONO_FONT, DEFAULT_UI_FONT,
};

/// Hauteur de la fenêtre. Le formulaire a sa propre barre latérale et son
/// défilement : il lui faut une hauteur imposée, sinon il s'étire à celle de
/// son contenu et la barre latérale se retrouve dans le vide.
const HEIGHT: gpui::Pixels = px(560.);
const WIDTH: gpui::Pixels = px(880.);

impl ClaudhubApp {
    pub(super) fn open_settings(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        // Les polices installées sont demandées une fois, à l'ouverture :
        // interroger le système à chaque frame du formulaire coûterait une
        // énumération complète par image.
        let installed = cx.text_system().all_font_names();
        let ui_fonts = choices(settings::font_choices(&installed, false, DEFAULT_UI_FONT));
        let mono_fonts = choices(settings::font_choices(&installed, true, DEFAULT_MONO_FONT));
        let shells = shell_choices();
        // Le registre est peuplé de façon asynchrone au démarrage ; à
        // l'ouverture du formulaire il l'est depuis longtemps.
        let light_themes = theme_choices(gpui_component::ThemeMode::Light, cx);
        let dark_themes = theme_choices(gpui_component::ThemeMode::Dark, cx);

        window.open_dialog(cx, move |dialog, _window, _cx| {
            let (ui_fonts, shells) = (ui_fonts.clone(), shells.clone());
            let (light_themes, dark_themes) = (light_themes.clone(), dark_themes.clone());
            let (mono_fonts, terminal_fonts) = (mono_fonts.clone(), mono_fonts.clone());
            dialog
                .title(tr!("settings-title"))
                .w(WIDTH)
                .max_w(WIDTH)
                .child(
                    v_flex().h(HEIGHT).child(
                        gpui_component::setting::Settings::new("claudhub-settings")
                            .sidebar_width(px(190.))
                            .page(appearance_page(
                                ui_fonts,
                                mono_fonts,
                                light_themes,
                                dark_themes,
                            ))
                            .page(terminal_page(shells, terminal_fonts))
                            .page(review_page()),
                    ),
                )
        });
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
                            .dropdown_menu_with_anchor(Corner::TopRight, move |menu, _, _| {
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
                .dropdown_menu_with_anchor(Corner::TopRight, move |menu, _, _| {
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

fn review_page() -> SettingPage {
    SettingPage::new(tr!("settings-page-review")).group(
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

/// Bornes communes aux tailles de texte, les mêmes que celles de la molette.
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
