//! Thème.
//!
//! Perch reprend les palettes claire et sombre de gpui-component et n'y touche
//! qu'à la marge : les couleurs des diffs et des états git, que la
//! bibliothèque n'a pas de raison de connaître.

use gpui::{px, App, Hsla, Rgba, Window};
use gpui_component::Theme;

use super::settings::{Settings, ThemeMode};

/// Applique le thème choisi. À appeler au démarrage et à chaque changement de
/// mode ou de taille de police.
pub fn apply(settings: &Settings, window: Option<&mut Window>, cx: &mut App) {
    let mode = match settings.theme {
        ThemeMode::Dark => gpui_component::ThemeMode::Dark,
        ThemeMode::Light => gpui_component::ThemeMode::Light,
        // gpui n'expose pas la préférence système de façon portable ; le mode
        // sombre est le défaut d'un outil de développement.
        ThemeMode::System => match cx.window_appearance() {
            gpui::WindowAppearance::Light | gpui::WindowAppearance::VibrantLight => {
                gpui_component::ThemeMode::Light
            }
            _ => gpui_component::ThemeMode::Dark,
        },
    };

    // `Theme::change` réinitialise les couleurs : tout réglage de palette doit
    // venir après, sinon il est effacé sans bruit.
    Theme::change(mode, window, cx);

    let theme = Theme::global_mut(cx);
    theme.font_family = settings.ui_font().to_string().into();
    theme.mono_font_family = settings.mono_font().to_string().into();
    theme.font_size = px(settings.font_size);

    cx.refresh_windows();
}

fn rgb(hex: u32) -> Hsla {
    Rgba {
        r: ((hex >> 16) & 0xff) as f32 / 255.0,
        g: ((hex >> 8) & 0xff) as f32 / 255.0,
        b: (hex & 0xff) as f32 / 255.0,
        a: 1.0,
    }
    .into()
}

/// Couleurs propres à la revue.
///
/// Le vert et le rouge sont ceux de GitHub, à dessein : ce sont les teintes
/// qu'un relecteur a déjà dans l'œil, et les fonds sont assez pâles pour que
/// le texte reste lisible dans les deux modes.
pub struct DiffColors {
    pub added_bg: Hsla,
    pub added_fg: Hsla,
    pub removed_bg: Hsla,
    pub removed_fg: Hsla,
    pub hunk_bg: Hsla,
    pub line_number: Hsla,
}

impl DiffColors {
    pub fn of(cx: &App) -> Self {
        if gpui_component::ActiveTheme::theme(cx).mode.is_dark() {
            Self {
                added_bg: Hsla {
                    a: 0.18,
                    ..rgb(0x3fb950)
                },
                added_fg: rgb(0x7ee787),
                removed_bg: Hsla {
                    a: 0.18,
                    ..rgb(0xf85149)
                },
                removed_fg: rgb(0xffa198),
                hunk_bg: Hsla {
                    a: 0.14,
                    ..rgb(0x58a6ff)
                },
                line_number: Hsla {
                    a: 0.45,
                    ..rgb(0xc9d1d9)
                },
            }
        } else {
            Self {
                added_bg: Hsla {
                    a: 0.22,
                    ..rgb(0x2da44e)
                },
                added_fg: rgb(0x0a5624),
                removed_bg: Hsla {
                    a: 0.20,
                    ..rgb(0xcf222e)
                },
                removed_fg: rgb(0x82071e),
                hunk_bg: Hsla {
                    a: 0.12,
                    ..rgb(0x0969da)
                },
                line_number: Hsla {
                    a: 0.55,
                    ..rgb(0x1f2328)
                },
            }
        }
    }
}

/// Couleur de la lettre d'état d'un fichier, dans la liste de revue.
pub fn status_color(code: crate::git::StatusCode, cx: &App) -> Hsla {
    use crate::git::StatusCode as S;
    let theme = gpui_component::ActiveTheme::theme(cx);
    match code {
        S::Added | S::Copied => rgb(0x3fb950),
        S::Modified | S::TypeChanged => rgb(0xd29922),
        S::Deleted => rgb(0xf85149),
        S::Renamed => rgb(0x58a6ff),
        S::Untracked => rgb(0x8b949e),
        S::Unmerged => rgb(0xdb6d28),
        S::Ignored | S::Unmodified => theme.muted_foreground,
    }
}
