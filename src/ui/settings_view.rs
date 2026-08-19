//! La fenêtre de réglages.
//!
//! Le formulaire vient de gpui-component : pages, groupes, recherche et bouton
//! de remise à zéro sont fournis, et chaque champ se déclare par un couple
//! lire/écrire. Ces fermetures ne reçoivent qu'un `App` — c'est ce qui impose
//! que les réglages vivent dans un global plutôt que dans `PerchApp`.
//!
//! Il n'y a pas de bouton « Appliquer » : chaque changement prend effet à la
//! frappe et l'écriture du fichier suit, différée. Un formulaire qui demande
//! de valider pour voir le résultat rend le choix d'une police ou d'une taille
//! impossible autrement qu'à l'aveugle.

use gpui::{div, prelude::*, px, App, Context, SharedString, Window};
use gpui_component::setting::{
    NumberFieldOptions, SettingField, SettingGroup, SettingItem, SettingPage,
};
use gpui_component::{v_flex, WindowExt};

use crate::tr;
use crate::ui::app::PerchApp;
use crate::ui::settings::{
    self, LanguageChoice, Settings, ThemeMode, DEFAULT_MONO_FONT, DEFAULT_UI_FONT,
};

/// Hauteur de la fenêtre. Le formulaire a sa propre barre latérale et son
/// défilement : il lui faut une hauteur imposée, sinon il s'étire à celle de
/// son contenu et la barre latérale se retrouve dans le vide.
const HEIGHT: gpui::Pixels = px(560.);
const WIDTH: gpui::Pixels = px(880.);

impl PerchApp {
    pub(super) fn open_settings(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        // Les polices installées sont demandées une fois, à l'ouverture :
        // interroger le système à chaque frame du formulaire coûterait une
        // énumération complète par image.
        let installed = cx.text_system().all_font_names();
        let ui_fonts = choices(settings::font_choices(&installed, false, DEFAULT_UI_FONT));
        let mono_fonts = choices(settings::font_choices(&installed, true, DEFAULT_MONO_FONT));
        let shells = shell_choices();

        window.open_dialog(cx, move |dialog, _window, _cx| {
            let (ui_fonts, shells) = (ui_fonts.clone(), shells.clone());
            let (mono_fonts, terminal_fonts) = (mono_fonts.clone(), mono_fonts.clone());
            dialog
                .title(tr!("settings-title"))
                .w(WIDTH)
                .max_w(WIDTH)
                .child(
                    v_flex().h(HEIGHT).child(
                        gpui_component::setting::Settings::new("perch-settings")
                            .sidebar_width(px(190.))
                            .page(appearance_page(ui_fonts, mono_fonts))
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

fn shell_choices() -> Vec<(SharedString, SharedString)> {
    let mut options = vec![(SharedString::default(), tr!("settings-shell-default"))];
    options.extend(choices(settings::available_shells()));
    options
}

fn appearance_page(
    ui_fonts: Vec<(SharedString, SharedString)>,
    mono_fonts: Vec<(SharedString, SharedString)>,
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
                .item(
                    SettingItem::new(
                        tr!("settings-shell"),
                        SettingField::dropdown(
                            shells,
                            |cx: &App| Settings::global(cx).terminal.shell.clone().into(),
                            |value: SharedString, cx: &mut App| {
                                Settings::update_global(cx, |s| {
                                    s.terminal.shell = value.to_string()
                                })
                            },
                        )
                        .default_value(SharedString::default()),
                    )
                    .description(tr!("settings-shell-help")),
                )
                .item(
                    SettingItem::new(
                        tr!("settings-agent"),
                        SettingField::input(
                            |cx: &App| Settings::global(cx).terminal.agent_command.clone().into(),
                            |value: SharedString, cx: &mut App| {
                                Settings::update_global(cx, |s| {
                                    s.terminal.agent_command = value.to_string()
                                })
                            },
                        )
                        .default_value(SharedString::from("claude")),
                    )
                    .description(tr!("settings-agent-help")),
                ),
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
            })),
    )
}

/// Bornes communes aux tailles de texte.
///
/// En dessous de huit points le texte n'est plus lisible et au-dessus de
/// trente-deux une seule ligne de diff occupe la fenêtre : ce sont les deux
/// façons de rendre l'interface inutilisable depuis le formulaire, et il n'y a
/// pas de raccourci pour en revenir tant que la molette ne zoome pas.
fn size_range() -> NumberFieldOptions {
    NumberFieldOptions {
        min: MIN_SIZE as f64,
        max: MAX_SIZE as f64,
        step: 1.0,
    }
}

pub const MIN_SIZE: f32 = 8.0;
pub const MAX_SIZE: f32 = 32.0;

pub fn clamp_size(value: f64) -> f32 {
    (value as f32).clamp(MIN_SIZE, MAX_SIZE)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sizes_stay_within_readable_bounds() {
        assert_eq!(clamp_size(0.), MIN_SIZE);
        assert_eq!(clamp_size(1_000.), MAX_SIZE);
        assert_eq!(clamp_size(13.), 13.0);
    }

    #[test]
    fn the_shell_list_always_offers_the_system_default_first() {
        let options = shell_choices();
        assert_eq!(options[0].0, SharedString::default());
    }
}
